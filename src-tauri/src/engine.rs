//! Drives the real, official client binaries — `xray` (Xray-core, the
//! reference VLESS implementation) and `hysteria` (the official Hysteria2
//! client) — bundled with the app as Tauri sidecars. Riekko does not
//! reimplement either protocol; it generates the config each project's own
//! docs describe and supervises the process, which is how every serious
//! VLESS/Hysteria2 GUI client (v2rayN, NekoBox, Hiddify, ...) actually works
//! under the hood. Being a sidecar means the binaries ship inside the app —
//! nothing for the user to install separately.

use crate::import::{Hysteria2Params, VlessParams};
use crate::state::pseudo_random;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tauri::AppHandle;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

/// Local SOCKS5 inbound port both cores are told to listen on. Only one
/// connection is ever active at a time, so there's no port contention.
pub const SOCKS_PORT: u16 = 10808;

pub struct ActiveConnection {
    child: Option<CommandChild>,
    alive: Arc<AtomicBool>,
    config_path: PathBuf,
}

impl ActiveConnection {
    /// False once the sidecar has exited on its own — a crashed core should
    /// surface as a real error, not a tunnel that silently stopped carrying
    /// traffic.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}

impl Drop for ActiveConnection {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            let _ = child.kill();
        }
        let _ = std::fs::remove_file(&self.config_path);
        // Safety net: however this handle goes away — explicit disconnect,
        // the core crashing, or the app quitting — the system must not be
        // left pointing its proxy at a port nothing is listening on
        // anymore, or the user's entire internet connection breaks.
        let _ = crate::proxy::disable_system_proxy();
    }
}

fn config_dir() -> PathBuf {
    std::env::temp_dir()
}

/// Builds an Xray-core JSON config for a single VLESS outbound.
/// Schema: https://xtls.github.io/config/.
fn build_vless_config(p: &VlessParams) -> Value {
    let mut stream_settings = json!({ "network": p.network });
    match p.security.as_str() {
        "tls" => {
            stream_settings["security"] = json!("tls");
            stream_settings["tlsSettings"] = json!({
                "serverName": p.sni.clone().unwrap_or_else(|| p.host.clone()),
                "allowInsecure": p.allow_insecure,
                "fingerprint": p.fingerprint.clone().unwrap_or_default(),
            });
        }
        "reality" => {
            stream_settings["security"] = json!("reality");
            stream_settings["realitySettings"] = json!({
                "serverName": p.sni.clone().unwrap_or_default(),
                "fingerprint": p.fingerprint.clone().unwrap_or_else(|| "chrome".to_string()),
                "publicKey": p.public_key.clone().unwrap_or_default(),
                "shortId": p.short_id.clone().unwrap_or_default(),
            });
        }
        _ => {
            stream_settings["security"] = json!("none");
        }
    }
    match p.network.as_str() {
        "ws" => {
            stream_settings["wsSettings"] = json!({
                "path": p.path.clone().unwrap_or_else(|| "/".to_string()),
                "headers": { "Host": p.host_header.clone().unwrap_or_else(|| p.host.clone()) }
            });
        }
        "grpc" => {
            stream_settings["grpcSettings"] = json!({
                "serviceName": p.service_name.clone().unwrap_or_default()
            });
        }
        _ => {}
    }

    let mut user = json!({ "id": p.uuid, "encryption": "none" });
    if let Some(flow) = p.flow.as_ref().filter(|f| !f.is_empty()) {
        user["flow"] = json!(flow);
    }

    json!({
        "log": { "loglevel": "warning" },
        "inbounds": [{
            "tag": "socks-in",
            "listen": "127.0.0.1",
            "port": SOCKS_PORT,
            "protocol": "socks",
            "settings": { "udp": true }
        }],
        "outbounds": [{
            "protocol": "vless",
            "settings": {
                "vnext": [{
                    "address": p.host,
                    "port": p.port,
                    "users": [user]
                }]
            },
            "streamSettings": stream_settings
        }]
    })
}

fn escape_yaml(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Builds the official Hysteria2 client YAML config.
/// Schema: https://v2.hysteria.network/docs/getting-started/Client/.
fn build_hysteria2_config(p: &Hysteria2Params) -> String {
    let mut yaml = format!(
        "server: \"{}:{}\"\nauth: \"{}\"\n",
        p.host,
        p.port,
        escape_yaml(&p.password)
    );
    yaml.push_str("tls:\n");
    if let Some(sni) = &p.sni {
        yaml.push_str(&format!("  sni: \"{}\"\n", escape_yaml(sni)));
    }
    yaml.push_str(&format!("  insecure: {}\n", p.insecure));
    if let Some(obfs_password) = &p.obfs_password {
        yaml.push_str("obfs:\n  type: salamander\n  salamander:\n");
        yaml.push_str(&format!("    password: \"{}\"\n", escape_yaml(obfs_password)));
    }
    yaml.push_str(&format!("socks5:\n  listen: \"127.0.0.1:{SOCKS_PORT}\"\n"));
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
    let command = app
        .shell()
        .sidecar(sidecar_name)
        .map_err(|e| format!("Не удалось подготовить {sidecar_name}: {e}"))?
        .args(args);

    let (mut rx, child) = command
        .spawn()
        .map_err(|e| format!("Не удалось запустить {sidecar_name}: {e}"))?;

    let alive = Arc::new(AtomicBool::new(true));
    let last_output = Arc::new(StdMutex::new(String::new()));

    {
        let alive = alive.clone();
        let last_output = last_output.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    CommandEvent::Stdout(bytes) | CommandEvent::Stderr(bytes) => {
                        if let Ok(mut buf) = last_output.lock() {
                            buf.push_str(&String::from_utf8_lossy(&bytes));
                        }
                    }
                    CommandEvent::Error(message) => {
                        if let Ok(mut buf) = last_output.lock() {
                            buf.push_str(&message);
                        }
                        alive.store(false, Ordering::SeqCst);
                    }
                    CommandEvent::Terminated(_) => {
                        alive.store(false, Ordering::SeqCst);
                    }
                    _ => {}
                }
            }
        });
    }

    std::thread::sleep(Duration::from_millis(400));

    if !alive.load(Ordering::SeqCst) {
        let message = last_output.lock().map(|b| b.clone()).unwrap_or_default();
        let _ = std::fs::remove_file(&config_path);
        return Err(format!(
            "{sidecar_name} завершился с ошибкой: {}",
            message.trim()
        ));
    }

    Ok(ActiveConnection {
        child: Some(child),
        alive,
        config_path,
    })
}

/// Starts the bundled `xray run -c <config>` for a single VLESS outbound.
pub fn start_vless(app: &AppHandle, p: &VlessParams) -> Result<ActiveConnection, String> {
    let config = build_vless_config(p);
    let config_path = config_dir().join(format!("riekko-xray-{}.json", pseudo_random(11)));
    std::fs::write(&config_path, config.to_string())
        .map_err(|e| format!("Не удалось записать конфиг: {e}"))?;

    let config_str = config_path.to_string_lossy().into_owned();
    // `Shell::sidecar()` resolves relative to the executable's own
    // directory, joining whatever name is passed as-is — it does NOT
    // strip the `externalBin` config's "binaries/" prefix. Tauri's build
    // step copies the sidecar next to the exe under its bare filename
    // (`target/debug/xray`), so the lookup key here must be the bare
    // name too, or resolution 404s with "No such file or directory".
    spawn_sidecar(
        app,
        "xray",
        vec!["run".to_string(), "-c".to_string(), config_str],
        config_path,
    )
}

/// Starts the bundled `hysteria client -c <config>`.
pub fn start_hysteria2(app: &AppHandle, p: &Hysteria2Params) -> Result<ActiveConnection, String> {
    let yaml = build_hysteria2_config(p);
    let config_path = config_dir().join(format!("riekko-hysteria-{}.yaml", pseudo_random(12)));
    std::fs::write(&config_path, yaml).map_err(|e| format!("Не удалось записать конфиг: {e}"))?;

    let config_str = config_path.to_string_lossy().into_owned();
    spawn_sidecar(
        app,
        "hysteria",
        vec!["client".to_string(), "-c".to_string(), config_str],
        config_path,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::parse_connect_params;
    use crate::import::ConnectParams;
    use std::process::{Command, Stdio};

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
        let config = build_vless_config(&p);
        assert_eq!(config["outbounds"][0]["protocol"], "vless");
        assert_eq!(
            config["outbounds"][0]["settings"]["vnext"][0]["address"],
            "nl.example.net"
        );
        assert_eq!(config["outbounds"][0]["settings"]["vnext"][0]["port"], 443);
        assert_eq!(
            config["outbounds"][0]["settings"]["vnext"][0]["users"][0]["id"],
            "b831381d-6324-4d53-ad4f-8cda48b30811"
        );
        assert_eq!(config["outbounds"][0]["streamSettings"]["network"], "ws");
        assert_eq!(config["outbounds"][0]["streamSettings"]["security"], "tls");
        assert_eq!(
            config["outbounds"][0]["streamSettings"]["wsSettings"]["path"],
            "/ws"
        );
        assert_eq!(config["inbounds"][0]["protocol"], "socks");
        assert_eq!(config["inbounds"][0]["port"], SOCKS_PORT);
    }

    #[test]
    fn hysteria2_config_matches_official_schema() {
        let p = hysteria2_params(
            "hysteria2://s3cr3t@de.example.net:8443?insecure=1&sni=example.com&obfs=salamander&obfs-password=hunter2#B",
        );
        let yaml = build_hysteria2_config(&p);
        assert!(yaml.contains("server: \"de.example.net:8443\""));
        assert!(yaml.contains("auth: \"s3cr3t\""));
        assert!(yaml.contains("sni: \"example.com\""));
        assert!(yaml.contains("insecure: true"));
        assert!(yaml.contains("type: salamander"));
        assert!(yaml.contains("password: \"hunter2\""));
        assert!(yaml.contains("listen: \"127.0.0.1:10808\""));
    }

    fn bundled_binary(base: &str) -> Option<PathBuf> {
        let triple = std::env::var("TARGET").ok().unwrap_or_else(|| {
            // Not cross-compiling in tests: infer from the host build.
            if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
                "aarch64-apple-darwin".to_string()
            } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
                "x86_64-apple-darwin".to_string()
            } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
                "x86_64-unknown-linux-gnu".to_string()
            } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
                "x86_64-pc-windows-msvc".to_string()
            } else {
                return String::new();
            }
        });
        if triple.is_empty() {
            return None;
        }
        let ext = if cfg!(windows) { ".exe" } else { "" };
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(format!("{base}-{triple}{ext}"));
        path.is_file().then_some(path)
    }

    /// Feeds our generated config straight to the real, bundled xray binary
    /// and confirms it accepts it and starts listening — not just that our
    /// JSON *looks* right, but that Xray-core itself agrees. Skips quietly
    /// if the sidecar for this host platform hasn't been fetched.
    #[test]
    fn real_xray_binary_accepts_generated_config() {
        let Some(binary) = bundled_binary("xray") else {
            eprintln!("skipping: no bundled xray binary for this host");
            return;
        };
        let p = vless_params("vless://uuid@example.com:443?security=tls#T");
        let config = build_vless_config(&p);
        let config_path = std::env::temp_dir().join("riekko-test-xray-config.json");
        std::fs::write(&config_path, config.to_string()).unwrap();

        let mut child = Command::new(&binary)
            .arg("run")
            .arg("-c")
            .arg(&config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn bundled xray binary");

        std::thread::sleep(Duration::from_millis(600));
        let exited_early = child.try_wait().ok().flatten().is_some();
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&config_path);

        assert!(
            !exited_early,
            "xray exited immediately instead of accepting our generated config"
        );
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
        let p = hysteria2_params("hysteria2://password@example.com:443#T");
        let yaml = build_hysteria2_config(&p);
        let config_path = std::env::temp_dir().join("riekko-test-hysteria-config.yaml");
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

        for bad in ["yaml", "unmarshal", "unknown field", "invalid config", "parse"] {
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
