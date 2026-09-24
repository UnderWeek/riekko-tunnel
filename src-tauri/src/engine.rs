//! Drives the real, official client binaries — `xray` (Xray-core, the
//! reference VLESS implementation) and `hysteria` (the official Hysteria2
//! client) — bundled with the app as Tauri sidecars. Riekko does not
//! reimplement either protocol; it generates the config each project's own
//! docs describe and supervises the process, which is how every serious
//! VLESS/Hysteria2 GUI client (v2rayN, NekoBox, Hiddify, ...) actually works
//! under the hood. Being a sidecar means the binaries ship inside the app —
//! nothing for the user to install separately.

use crate::import::{join_host_port, ConnectParams, Hysteria2Params, VlessParams};
use crate::state::unique_id;
use serde_json::{json, Value};
use std::io::Write as _;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tauri::AppHandle;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

/// How much of the core's own output is kept for error reports. Xray logs
/// every failed dial at `warning`, so an uncapped buffer grows for as long
/// as the tunnel is up.
const OUTPUT_TAIL_BYTES: usize = 8 * 1024;

pub struct ActiveConnection {
    child: Option<CommandChild>,
    pid: u32,
    alive: Arc<AtomicBool>,
    output: Arc<StdMutex<String>>,
    config_path: PathBuf,
}

impl ActiveConnection {
    /// False once the sidecar has exited on its own — a crashed core should
    /// surface as a real error, not a tunnel that silently stopped carrying
    /// traffic.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The core's last words, for telling the user *why* it stopped.
    pub fn output_tail(&self) -> String {
        let text = self.output.lock().map(|b| b.clone()).unwrap_or_default();
        last_meaningful_line(&text)
    }
}

impl Drop for ActiveConnection {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            let _ = child.kill();
        }
        let _ = std::fs::remove_file(&self.config_path);
    }
}

/// The local listeners a core exposes: SOCKS5 (what tun2socks and the
/// macOS/GNOME system proxy use) and HTTP (what the Windows system proxy
/// needs — WinINet and Chromium read its `socks=` entry as SOCKS4, which
/// Hysteria's listener rejects outright).
#[derive(Clone, Copy)]
pub struct LocalPorts {
    pub socks: u16,
    pub http: u16,
}

impl LocalPorts {
    pub fn pick() -> Result<Self, String> {
        let socks = free_port()?;
        let mut http = free_port()?;
        while http == socks {
            http = free_port()?;
        }
        Ok(Self { socks, http })
    }
}

/// Picks a currently free localhost port. The fixed 10808 the app used to
/// hard-code is also v2rayN's default, and an orphaned core from a crashed
/// session would keep it bound too — either way the next connect failed.
pub fn free_port() -> Result<u16, String> {
    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .map_err(|e| format!("Не удалось найти свободный локальный порт: {e}"))
}

/// Configs carry credentials (UUID, password): create them fresh (never
/// following a planted symlink) and readable by the current user only.
pub fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(contents)
}

fn push_output(buf: &mut String, text: &str) {
    buf.push_str(text);
    if buf.len() > OUTPUT_TAIL_BYTES {
        let mut cut = buf.len() - OUTPUT_TAIL_BYTES;
        while !buf.is_char_boundary(cut) {
            cut += 1;
        }
        buf.drain(..cut);
    }
}

/// Core logs are timestamped, multi-line and mostly noise; the last
/// non-empty line is almost always the actual fatal error.
fn last_meaningful_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("")
        .chars()
        .take(400)
        .collect()
}

/// Builds an Xray-core JSON config for a single VLESS outbound.
/// Schema: https://xtls.github.io/config/.
///
/// `dial_host` is where the core actually connects — the server IP resolved
/// once at connect time, so the core and the TUN route exclusion always
/// agree on it (and the core never needs DNS, which in TUN mode would loop
/// back into the tunnel it is supposed to carry).
fn build_vless_config(p: &VlessParams, dial_host: &str, ports: LocalPorts) -> Value {
    let server_name = p
        .sni
        .clone()
        .or_else(|| p.host_header.clone())
        .unwrap_or_else(|| p.host.clone());
    let http_host = p.host_header.clone().unwrap_or_else(|| p.host.clone());
    let path = p.path.clone().unwrap_or_else(|| "/".to_string());

    let mut stream_settings = json!({ "network": p.network });
    match p.security.as_str() {
        "tls" => {
            // No `allowInsecure`: Xray 26 refuses to start with it at all
            // (migrated to `pinnedPeerCertSha256`, fed from `pcs` below).
            let mut tls = json!({ "serverName": server_name });
            if let Some(fp) = &p.fingerprint {
                tls["fingerprint"] = json!(fp);
            }
            if let Some(alpn) = &p.alpn {
                let list: Vec<&str> = alpn
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect();
                tls["alpn"] = json!(list);
            }
            if let Some(pin) = &p.pinned_cert_sha256 {
                tls["pinnedPeerCertSha256"] = json!(pin);
            }
            stream_settings["security"] = json!("tls");
            stream_settings["tlsSettings"] = tls;
        }
        "reality" => {
            let mut reality = json!({
                "serverName": p.sni.clone().unwrap_or_default(),
                "fingerprint": p.fingerprint.clone().unwrap_or_else(|| "chrome".to_string()),
                "publicKey": p.public_key.clone().unwrap_or_default(),
                "shortId": p.short_id.clone().unwrap_or_default(),
            });
            if let Some(spx) = &p.spider_x {
                reality["spiderX"] = json!(spx);
            }
            stream_settings["security"] = json!("reality");
            stream_settings["realitySettings"] = reality;
        }
        _ => {
            stream_settings["security"] = json!("none");
        }
    }
    match p.network.as_str() {
        "ws" => {
            stream_settings["wsSettings"] = json!({ "path": path, "host": http_host });
        }
        "httpupgrade" => {
            stream_settings["httpupgradeSettings"] = json!({ "path": path, "host": http_host });
        }
        "xhttp" => {
            let mut xhttp = json!({ "path": path, "host": http_host });
            if let Some(mode) = &p.mode {
                xhttp["mode"] = json!(mode);
            }
            stream_settings["xhttpSettings"] = xhttp;
        }
        "grpc" => {
            stream_settings["grpcSettings"] = json!({
                "serviceName": p.service_name.clone().unwrap_or_default(),
                "multiMode": p.mode.as_deref() == Some("multi"),
            });
        }
        "tcp" if p.header_type.as_deref() == Some("http") => {
            stream_settings["tcpSettings"] = json!({
                "header": {
                    "type": "http",
                    "request": { "path": [path], "headers": { "Host": [http_host] } }
                }
            });
        }
        _ => {}
    }

    let mut user = json!({ "id": p.uuid, "encryption": p.encryption });
    if let Some(flow) = p.flow.as_ref().filter(|f| !f.is_empty()) {
        user["flow"] = json!(flow);
    }

    json!({
        "log": { "loglevel": "warning" },
        "inbounds": [{
            "tag": "socks-in",
            "listen": "127.0.0.1",
            "port": ports.socks,
            "protocol": "socks",
            "settings": { "udp": true }
        }, {
            "tag": "http-in",
            "listen": "127.0.0.1",
            "port": ports.http,
            "protocol": "http"
        }],
        "outbounds": [{
            "protocol": "vless",
            "settings": {
                "vnext": [{
                    "address": dial_host,
                    "port": p.port,
                    "users": [user]
                }]
            },
            "streamSettings": stream_settings
        }]
    })
}

fn escape_yaml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Builds the official Hysteria2 client YAML config.
/// Schema: https://v2.hysteria.network/docs/getting-started/Client/.
fn build_hysteria2_config(p: &Hysteria2Params, dial_host: &str, ports: LocalPorts) -> String {
    let mut yaml = format!(
        "server: \"{}\"\nauth: \"{}\"\n",
        escape_yaml(&join_host_port(dial_host, &p.port_spec)),
        escape_yaml(&p.password)
    );
    yaml.push_str("tls:\n");
    // Dialing a pinned IP would otherwise drop the SNI the server's
    // certificate is issued for.
    let is_domain = p.host.parse::<std::net::IpAddr>().is_err();
    if let Some(sni) = p.sni.as_ref().or(is_domain.then_some(&p.host)) {
        yaml.push_str(&format!("  sni: \"{}\"\n", escape_yaml(sni)));
    }
    yaml.push_str(&format!("  insecure: {}\n", p.insecure));
    if let Some(pin) = &p.pin_sha256 {
        yaml.push_str(&format!("  pinSHA256: \"{}\"\n", escape_yaml(pin)));
    }
    if let Some(ech) = &p.ech {
        yaml.push_str(&format!("  ech: \"{}\"\n", escape_yaml(ech)));
    }
    if let Some((kind, password)) = &p.obfs {
        yaml.push_str(&format!("obfs:\n  type: {kind}\n  {kind}:\n"));
        yaml.push_str(&format!("    password: \"{}\"\n", escape_yaml(password)));
    }
    yaml.push_str(&format!(
        "socks5:\n  listen: \"127.0.0.1:{}\"\n",
        ports.socks
    ));
    yaml.push_str(&format!("http:\n  listen: \"127.0.0.1:{}\"\n", ports.http));
    yaml
}

/// Spawns a bundled sidecar, watches its output/exit in the background, and
/// gives it a short window to fail fast (bad config, port already in use) so
/// a real error can be reported instead of claiming success blindly.
fn spawn_sidecar(
    app: &AppHandle,
    sidecar_name: &str,
    args: Vec<String>,
    config_path: PathBuf,
) -> Result<ActiveConnection, String> {
    let fail = |message: String| {
        let _ = std::fs::remove_file(&config_path);
        message
    };
    // `Shell::sidecar()` resolves relative to the executable's own
    // directory, joining whatever name is passed as-is — it does NOT
    // strip the `externalBin` config's "binaries/" prefix. Tauri's build
    // step copies the sidecar next to the exe under its bare filename
    // (`target/debug/xray`), so the lookup key here must be the bare
    // name too, or resolution 404s with "No such file or directory".
    let command = app
        .shell()
        .sidecar(sidecar_name)
        .map_err(|e| fail(format!("Не удалось подготовить {sidecar_name}: {e}")))?
        .args(args);

    let (mut rx, child) = command
        .spawn()
        .map_err(|e| fail(format!("Не удалось запустить {sidecar_name}: {e}")))?;

    let alive = Arc::new(AtomicBool::new(true));
    let output = Arc::new(StdMutex::new(String::new()));

    {
        let alive = alive.clone();
        let output = output.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    CommandEvent::Stdout(bytes) | CommandEvent::Stderr(bytes) => {
                        if let Ok(mut buf) = output.lock() {
                            push_output(&mut buf, &String::from_utf8_lossy(&bytes));
                        }
                    }
                    CommandEvent::Error(message) => {
                        if let Ok(mut buf) = output.lock() {
                            push_output(&mut buf, &message);
                        }
                        alive.store(false, Ordering::SeqCst);
                    }
                    CommandEvent::Terminated(_) => {
                        alive.store(false, Ordering::SeqCst);
                    }
                    _ => {}
                }
            }
            // The channel closing means the process is gone too.
            alive.store(false, Ordering::SeqCst);
        });
    }

    let connection = ActiveConnection {
        pid: child.pid(),
        child: Some(child),
        alive,
        output,
        config_path,
    };

    std::thread::sleep(Duration::from_millis(400));

    if !connection.is_alive() {
        let reason = connection.output_tail();
        return Err(if reason.is_empty() {
            format!("{sidecar_name} завершился сразу после запуска")
        } else {
            format!("{sidecar_name} завершился с ошибкой: {reason}")
        });
    }

    Ok(connection)
}

/// Starts the right bundled core for `params`, dialing `dial_host` and
/// exposing SOCKS5 and HTTP listeners on `ports`.
pub fn start(
    app: &AppHandle,
    params: &ConnectParams,
    dial_host: &str,
    ports: LocalPorts,
) -> Result<ActiveConnection, String> {
    let (sidecar, subcommand, file_name, contents) = match params {
        ConnectParams::Vless(p) => (
            "xray",
            "run",
            format!("{}.json", unique_id("riekko-xray")),
            build_vless_config(p, dial_host, ports).to_string(),
        ),
        ConnectParams::Hysteria2(p) => (
            "hysteria",
            "client",
            format!("{}.yaml", unique_id("riekko-hysteria")),
            build_hysteria2_config(p, dial_host, ports),
        ),
    };
    let config_path = std::env::temp_dir().join(file_name);
    write_private(&config_path, contents.as_bytes())
        .map_err(|e| format!("Не удалось записать конфиг: {e}"))?;
    let config_str = config_path.to_string_lossy().into_owned();
    spawn_sidecar(
        app,
        sidecar,
        vec![subcommand.to_string(), "-c".to_string(), config_str],
        config_path,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::parse_connect_params;
    use std::process::{Command, Stdio};

    const TEST_PORT: u16 = 10808;
    const PORTS: LocalPorts = LocalPorts {
        socks: TEST_PORT,
        http: TEST_PORT + 1,
    };

    fn vless_params(uri: &str) -> VlessParams {
        match parse_connect_params(uri).unwrap() {
            ConnectParams::Vless(p) => p,
            _ => panic!("expected vless"),
        }
    }

    fn hysteria2_params(uri: &str) -> Hysteria2Params {
        match parse_connect_params(uri).unwrap() {
            ConnectParams::Hysteria2(p) => p,
            _ => panic!("expected hysteria2"),
        }
    }

    #[test]
    fn vless_config_matches_xray_schema() {
        let p = vless_params(
            "vless://b831381d-6324-4d53-ad4f-8cda48b30811@nl.example.net:443?type=ws&security=tls&path=%2Fws&host=cdn.example.com#T",
        );
        let config = build_vless_config(&p, "203.0.113.7", PORTS);
        let outbound = &config["outbounds"][0];
        assert_eq!(outbound["protocol"], "vless");
        // Dials the pinned IP, but still presents the real names.
        assert_eq!(outbound["settings"]["vnext"][0]["address"], "203.0.113.7");
        assert_eq!(outbound["settings"]["vnext"][0]["port"], 443);
        assert_eq!(
            outbound["settings"]["vnext"][0]["users"][0]["id"],
            "b831381d-6324-4d53-ad4f-8cda48b30811"
        );
        let stream = &outbound["streamSettings"];
        assert_eq!(stream["network"], "ws");
        assert_eq!(stream["security"], "tls");
        assert_eq!(stream["tlsSettings"]["serverName"], "cdn.example.com");
        assert!(stream["tlsSettings"].get("allowInsecure").is_none());
        assert_eq!(stream["wsSettings"]["path"], "/ws");
        assert_eq!(stream["wsSettings"]["host"], "cdn.example.com");
        assert_eq!(config["inbounds"][0]["protocol"], "socks");
        assert_eq!(config["inbounds"][0]["port"], TEST_PORT);
        assert_eq!(config["inbounds"][1]["protocol"], "http");
        assert_eq!(config["inbounds"][1]["port"], TEST_PORT + 1);
    }

    #[test]
    fn vless_sni_falls_back_to_link_host_for_ip_dial() {
        let p = vless_params("vless://uuid@nl.example.net:443?security=tls");
        let config = build_vless_config(&p, "203.0.113.7", PORTS);
        assert_eq!(
            config["outbounds"][0]["streamSettings"]["tlsSettings"]["serverName"],
            "nl.example.net"
        );
    }

    #[test]
    fn hysteria2_config_matches_official_schema() {
        let p = hysteria2_params(
            "hysteria2://s3cr3t@de.example.net:8443?insecure=1&sni=example.com&obfs=salamander&obfs-password=hunter2#B",
        );
        let yaml = build_hysteria2_config(&p, "203.0.113.7", PORTS);
        assert!(yaml.contains("server: \"203.0.113.7:8443\""));
        assert!(yaml.contains("auth: \"s3cr3t\""));
        assert!(yaml.contains("sni: \"example.com\""));
        assert!(yaml.contains("insecure: true"));
        assert!(yaml.contains("type: salamander"));
        assert!(yaml.contains("password: \"hunter2\""));
        assert!(yaml.contains("socks5:\n  listen: \"127.0.0.1:10808\""));
        assert!(yaml.contains("http:\n  listen: \"127.0.0.1:10809\""));
    }

    #[test]
    fn hysteria2_keeps_domain_as_sni_when_dialing_ip() {
        let p = hysteria2_params("hysteria2://pw@de.example.net:20000-30000");
        let yaml = build_hysteria2_config(&p, "2001:db8::1", PORTS);
        assert!(
            yaml.contains("server: \"[2001:db8::1]:20000-30000\""),
            "{yaml}"
        );
        assert!(yaml.contains("sni: \"de.example.net\""), "{yaml}");
    }

    #[test]
    fn yaml_escaping_survives_control_characters() {
        let p = hysteria2_params("hysteria2://p%22w%0Ad@de.example.net:443");
        let yaml = build_hysteria2_config(&p, "203.0.113.7", PORTS);
        assert!(yaml.contains(r#"auth: "p\"w\nd""#), "{yaml}");
    }

    #[test]
    fn output_buffer_is_bounded() {
        let mut buf = String::new();
        for _ in 0..10_000 {
            push_output(&mut buf, "ошибка соединения\n");
        }
        assert!(buf.len() <= OUTPUT_TAIL_BYTES);
        assert_eq!(last_meaningful_line(&buf), "ошибка соединения");
    }

    #[test]
    fn write_private_refuses_to_overwrite() {
        let path = std::env::temp_dir().join(unique_id("riekko-test-private"));
        write_private(&path, b"one").unwrap();
        assert!(write_private(&path, b"two").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_file(&path);
    }

    fn bundled_binary(base: &str) -> Option<PathBuf> {
        let triple = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            "aarch64-apple-darwin"
        } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
            "x86_64-apple-darwin"
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            "x86_64-unknown-linux-gnu"
        } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            "x86_64-pc-windows-msvc"
        } else {
            return None;
        };
        let ext = if cfg!(windows) { ".exe" } else { "" };
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(format!("{base}-{triple}{ext}"));
        path.is_file().then_some(path)
    }

    /// Asks the real, bundled Xray-core to validate a generated config
    /// (`xray run -test`) — not just that our JSON *looks* right, but that
    /// Xray itself accepts it. Skips quietly if the sidecar for this host
    /// platform hasn't been fetched.
    fn assert_xray_accepts(uri: &str) {
        let Some(binary) = bundled_binary("xray") else {
            eprintln!("skipping: no bundled xray binary for this host");
            return;
        };
        let config = build_vless_config(&vless_params(uri), "203.0.113.7", PORTS);
        let config_path =
            std::env::temp_dir().join(format!("{}.json", unique_id("riekko-test-xray")));
        std::fs::write(&config_path, config.to_string()).unwrap();
        let output = Command::new(&binary)
            .args(["run", "-test", "-c"])
            .arg(&config_path)
            .stdin(Stdio::null())
            .output()
            .expect("failed to spawn bundled xray binary");
        let _ = std::fs::remove_file(&config_path);
        assert!(
            output.status.success(),
            "xray rejected the config for {uri}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn real_xray_binary_accepts_generated_configs() {
        for uri in [
            "vless://b831381d-6324-4d53-ad4f-8cda48b30811@example.com:443?security=tls#T",
            // allowInsecure=1 used to make Xray 26 refuse to start at all.
            "vless://b831381d-6324-4d53-ad4f-8cda48b30811@example.com:443?security=tls&allowInsecure=1&fp=chrome&alpn=h2,http/1.1#T",
            "vless://b831381d-6324-4d53-ad4f-8cda48b30811@example.com:443?type=ws&security=tls&path=%2Fws&host=cdn.example.com#T",
            "vless://b831381d-6324-4d53-ad4f-8cda48b30811@example.com:443?type=xhttp&security=tls&path=%2Fx&mode=auto#T",
            "vless://b831381d-6324-4d53-ad4f-8cda48b30811@example.com:443?type=splithttp&path=%2Fx#T",
            "vless://b831381d-6324-4d53-ad4f-8cda48b30811@example.com:443?type=httpupgrade&path=%2Fu&host=a.example.com#T",
            "vless://b831381d-6324-4d53-ad4f-8cda48b30811@example.com:443?type=grpc&serviceName=svc&mode=multi&security=tls#T",
            "vless://b831381d-6324-4d53-ad4f-8cda48b30811@example.com:80?type=tcp&headerType=http&host=a.example.com#T",
            "vless://b831381d-6324-4d53-ad4f-8cda48b30811@example.com:443?security=reality&sni=a.example.com&fp=chrome&pbk=R-bXDZUrx3hBgeN_NdQKFEiiYAdCj0plQNJfPSU-ywQ&sid=de&spx=%2F&flow=xtls-rprx-vision&type=tcp#R",
        ] {
            assert_xray_accepts(uri);
        }
    }

    /// Unlike Xray's SOCKS outbound (lazy — it only dials on first proxied
    /// request), Hysteria2's client eagerly dials and QUIC/TLS-handshakes
    /// the server at startup. Since `example.com` isn't a real Hysteria2
    /// endpoint, the handshake is *expected* to fail — so this test can't
    /// assert "stays alive" like the Xray one. Instead it proves our YAML
    /// is shaped correctly by asserting the failure is a genuine network/TLS
    /// error (proof hysteria parsed the config and tried to connect), not a
    /// config-shape rejection (unknown field, YAML syntax, etc).
    #[test]
    fn real_hysteria_binary_accepts_generated_config() {
        let Some(binary) = bundled_binary("hysteria") else {
            eprintln!("skipping: no bundled hysteria binary for this host");
            return;
        };
        let p = hysteria2_params(
            "hysteria2://password@example.com:443?obfs=gecko&obfs-password=g3cko&pinSHA256=AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99#T",
        );
        let yaml = build_hysteria2_config(
            &p,
            "127.0.0.1",
            LocalPorts {
                socks: TEST_PORT + 2,
                http: TEST_PORT + 3,
            },
        );
        let config_path =
            std::env::temp_dir().join(format!("{}.yaml", unique_id("riekko-test-hy")));
        std::fs::write(&config_path, yaml).unwrap();

        let mut child = Command::new(&binary)
            .arg("client")
            .arg("-c")
            .arg(&config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn bundled hysteria binary");

        std::thread::sleep(Duration::from_millis(800));
        let _ = child.kill();
        let output = child.wait_with_output().unwrap();
        let _ = std::fs::remove_file(&config_path);

        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .to_lowercase();

        for bad in [
            "yaml",
            "unmarshal",
            "unknown field",
            "invalid config",
            "parse",
        ] {
            assert!(
                !text.contains(bad),
                "hysteria rejected our config shape (found {bad:?} in output): {text}"
            );
        }
        assert!(
            text.contains("client mode"),
            "hysteria never got as far as starting the client: {text}"
        );
    }
}
