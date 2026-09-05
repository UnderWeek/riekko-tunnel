//! Full-system traffic capture via a TUN device, bridged to our local SOCKS5
//! listener by the official `tun2socks` project
//! (https://github.com/xjasonlyu/tun2socks). This is what actually routes
//! *all* traffic — not just apps that read system proxy settings — through
//! the tunnel, the same way real VPN clients work.
//!
//! Creating and configuring a TUN interface is a privileged OS operation on
//! every platform. Doing it the way Apple sanctions (NetworkExtension /
//! `NEPacketTunnelProvider`, what Shadowrocket uses) requires a paid Apple
//! Developer certificate and a signed, notarized app plus an installed
//! system-extension helper — none of which this project has. So, like every
//! other unsigned cross-platform client (NekoBox, etc.), this asks for one
//! administrator/root prompt to bring the tunnel up and one to tear it down.
//! There's no way around that without the paid signing path.
//!
//! Config recipe (device name, routes, split-default trick) follows
//! tun2socks's own documented examples:
//! https://github.com/xjasonlyu/tun2socks/wiki/Examples

use futures_util::StreamExt;
use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const TUN_DEVICE: &str = "utun123";
const TUN_IP: &str = "198.18.0.1";
/// tun2socks's own `--restapi` HTTP server — its `/traffic` endpoint streams
/// real, live up/down byte counters for everything passing through the TUN
/// device, which is the actual source of truth now that TUN carries all
/// traffic (not an estimate, not a guess at what xray/hysteria did).
const RESTAPI_PORT: u16 = 9797;

/// Bookkeeping needed to cleanly reverse `up()` — resolved once at connect
/// time so teardown targets the exact routes that were actually added, even
/// if the network changes state in between.
pub struct TunHandle {
    server_ip: String,
    gateway: String,
    up_bytes: Arc<AtomicU64>,
    down_bytes: Arc<AtomicU64>,
}

impl TunHandle {
    /// Total bytes sent / received since connect, as reported live by
    /// tun2socks. `(up, down)`.
    pub fn traffic_bytes(&self) -> (u64, u64) {
        (
            self.up_bytes.load(Ordering::Relaxed),
            self.down_bytes.load(Ordering::Relaxed),
        )
    }
}

#[derive(serde::Deserialize)]
struct TrafficSample {
    up: u64,
    down: u64,
}

/// tun2socks emits one newline-delimited JSON object per second on
/// `/traffic` for as long as the connection stays open. This just sums
/// each second's throughput into a running total; the task ends on its own
/// once tun2socks exits and the stream closes — nothing to cancel.
fn spawn_traffic_watcher(up_bytes: Arc<AtomicU64>, down_bytes: Arc<AtomicU64>) {
    tauri::async_runtime::spawn(async move {
        // Give tun2socks a moment to open its REST listener after being launched.
        tokio::time::sleep(Duration::from_millis(700)).await;

        let url = format!("http://127.0.0.1:{RESTAPI_PORT}/traffic");
        let Ok(response) = reqwest::get(&url).await else {
            return;
        };

        let mut stream = response.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(Ok(chunk)) = stream.next().await {
            buf.extend_from_slice(&chunk);
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                if let Ok(sample) = serde_json::from_slice::<TrafficSample>(&line) {
                    up_bytes.fetch_add(sample.up, Ordering::Relaxed);
                    down_bytes.fetch_add(sample.down, Ordering::Relaxed);
                }
            }
        }
    });
}

impl Drop for TunHandle {
    fn drop(&mut self) {
        // Safety net: however this handle disappears — explicit disconnect,
        // or the whole app quitting/crashing with TUN still active — the
        // elevated tun2socks process and the routes pointing at it must not
        // outlive it, or the user's internet stays broken after the app is
        // gone. This is the ONLY place teardown happens (see `down()`'s
        // doc comment) to avoid prompting for the admin password twice.
        let _ = platform::down(self);
    }
}

fn sidecar_path(base: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "исполняемый файл без родительской директории".to_string())?;
    let name = if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    };
    let path = dir.join(name);
    if !path.exists() {
        return Err(format!("{base} не найден рядом с приложением ({})", path.display()));
    }
    Ok(path)
}

fn resolve_ipv4(host: &str) -> Result<String, String> {
    (host, 0u16)
        .to_socket_addrs()
        .map_err(|e| format!("Не удалось разрешить {host}: {e}"))?
        .find(|a| a.is_ipv4())
        .map(|a| a.ip().to_string())
        .ok_or_else(|| format!("Не удалось получить IPv4-адрес для {host}"))
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::process::Command;

    fn default_route() -> Result<(String, String), String> {
        let output = Command::new("route")
            .args(["-n", "get", "default"])
            .output()
            .map_err(|e| format!("route -n get default: {e}"))?;
        let text = String::from_utf8_lossy(&output.stdout);
        let mut gateway = None;
        let mut interface = None;
        for line in text.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("gateway:") {
                gateway = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("interface:") {
                interface = Some(v.trim().to_string());
            }
        }
        match (gateway, interface) {
            (Some(g), Some(i)) => Ok((g, i)),
            _ => Err("Не удалось определить основной сетевой интерфейс (маршрут по умолчанию)".to_string()),
        }
    }

    fn run_elevated(script: &str, tag: &str) -> Result<(), String> {
        let script_path = std::env::temp_dir().join(format!("riekko-tun-{tag}.sh"));
        std::fs::write(&script_path, script).map_err(|e| format!("Не удалось записать скрипт: {e}"))?;

        let applescript = format!(
            "do shell script \"/bin/bash '{}'\" with administrator privileges",
            script_path.display()
        );
        let output = Command::new("osascript")
            .arg("-e")
            .arg(&applescript)
            .output()
            .map_err(|e| format!("osascript: {e}"))?;
        let _ = std::fs::remove_file(&script_path);

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(if stderr.contains("User canceled") {
                "Запрос пароля администратора отменён".to_string()
            } else {
                format!("Настройка TUN не удалась: {}", stderr.trim())
            });
        }
        Ok(())
    }

    pub fn up(socks_port: u16, server_host: &str) -> Result<TunHandle, String> {
        let tun2socks = sidecar_path("tun2socks")?;
        let (gateway, interface) = default_route()?;
        let server_ip = resolve_ipv4(server_host)?;

        let script = format!(
            r#"set -e
"{tun2socks}" --device {device} --proxy socks5://127.0.0.1:{port} --interface {iface} --restapi 127.0.0.1:{restapi} --loglevel silent > /tmp/riekko-tun2socks.log 2>&1 &
disown
echo $! > /tmp/riekko-tun2socks.pid
sleep 1
ifconfig {device} {ip} {ip} up
route add -host {server_ip} {gateway} >/dev/null 2>&1 || true
route add -net 1.0.0.0/8 {ip}
route add -net 2.0.0.0/7 {ip}
route add -net 4.0.0.0/6 {ip}
route add -net 8.0.0.0/5 {ip}
route add -net 16.0.0.0/4 {ip}
route add -net 32.0.0.0/3 {ip}
route add -net 64.0.0.0/2 {ip}
route add -net 128.0.0.0/1 {ip}
route add -net 198.18.0.0/15 {ip}
"#,
            tun2socks = tun2socks.display(),
            device = TUN_DEVICE,
            port = socks_port,
            iface = interface,
            restapi = RESTAPI_PORT,
            ip = TUN_IP,
            server_ip = server_ip,
            gateway = gateway,
        );

        run_elevated(&script, "up")?;

        let up_bytes = Arc::new(AtomicU64::new(0));
        let down_bytes = Arc::new(AtomicU64::new(0));
        spawn_traffic_watcher(up_bytes.clone(), down_bytes.clone());

        Ok(TunHandle {
            server_ip,
            gateway,
            up_bytes,
            down_bytes,
        })
    }

    pub fn down(handle: &TunHandle) -> Result<(), String> {
        let script = format!(
            r#"PID=$(cat /tmp/riekko-tun2socks.pid 2>/dev/null || echo "")
if [ -n "$PID" ]; then kill "$PID" 2>/dev/null || true; fi
route delete -host {server_ip} {gateway} >/dev/null 2>&1 || true
route delete -net 1.0.0.0/8 {ip} >/dev/null 2>&1 || true
route delete -net 2.0.0.0/7 {ip} >/dev/null 2>&1 || true
route delete -net 4.0.0.0/6 {ip} >/dev/null 2>&1 || true
route delete -net 8.0.0.0/5 {ip} >/dev/null 2>&1 || true
route delete -net 16.0.0.0/4 {ip} >/dev/null 2>&1 || true
route delete -net 32.0.0.0/3 {ip} >/dev/null 2>&1 || true
route delete -net 64.0.0.0/2 {ip} >/dev/null 2>&1 || true
route delete -net 128.0.0.0/1 {ip} >/dev/null 2>&1 || true
route delete -net 198.18.0.0/15 {ip} >/dev/null 2>&1 || true
rm -f /tmp/riekko-tun2socks.pid /tmp/riekko-tun2socks.log
"#,
            ip = TUN_IP,
            server_ip = handle.server_ip,
            gateway = handle.gateway,
        );
        run_elevated(&script, "down")
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use std::process::Command;

    /// Best-effort: written from tun2socks's own documented Windows recipe,
    /// but not verified on real Windows hardware in this dev environment.
    fn primary_interface() -> Result<String, String> {
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(Get-NetRoute -DestinationPrefix 0.0.0.0/0 | Sort-Object -Property RouteMetric | Select-Object -First 1 -ExpandProperty InterfaceAlias)",
            ])
            .output()
            .map_err(|e| format!("powershell: {e}"))?;
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if name.is_empty() {
            Err("Не удалось определить основной сетевой адаптер".to_string())
        } else {
            Ok(name)
        }
    }

    fn run_elevated(script: &str, tag: &str) -> Result<(), String> {
        let script_path = std::env::temp_dir().join(format!("riekko-tun-{tag}.ps1"));
        std::fs::write(&script_path, script).map_err(|e| format!("Не удалось записать скрипт: {e}"))?;

        let status = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Start-Process powershell -Verb RunAs -Wait -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','{}'",
                    script_path.display()
                ),
            ])
            .status()
            .map_err(|e| format!("powershell: {e}"))?;
        let _ = std::fs::remove_file(&script_path);

        if !status.success() {
            return Err("Настройка TUN не удалась (запрос UAC отклонён или ошибка сценария)".to_string());
        }
        Ok(())
    }

    pub fn up(socks_port: u16, server_host: &str) -> Result<TunHandle, String> {
        let tun2socks = sidecar_path("tun2socks")?;
        let interface = primary_interface()?;
        let server_ip = resolve_ipv4(server_host)?;

        let script = format!(
            r#"$mainRoute = Get-NetRoute -DestinationPrefix 0.0.0.0/0 | Sort-Object RouteMetric | Select-Object -First 1
Start-Process -FilePath "{tun2socks}" -ArgumentList '--device','wintun','--proxy','socks5://127.0.0.1:{port}','--interface','{iface}','--restapi','127.0.0.1:{restapi}','--loglevel','silent' -WindowStyle Hidden
Start-Sleep -Seconds 2
netsh interface ipv4 set address name="wintun" source=static addr={ip} mask=255.255.255.0
New-NetRoute -DestinationPrefix "{server_ip}/32" -InterfaceIndex $mainRoute.ifIndex -NextHop $mainRoute.NextHop -ErrorAction SilentlyContinue | Out-Null
netsh interface ipv4 add route 0.0.0.0/1 "wintun" {ip} metric=1
netsh interface ipv4 add route 128.0.0.0/1 "wintun" {ip} metric=1
"#,
            tun2socks = tun2socks.display(),
            port = socks_port,
            iface = interface,
            restapi = RESTAPI_PORT,
            ip = TUN_IP,
            server_ip = server_ip,
        );

        run_elevated(&script, "up")?;

        let up_bytes = Arc::new(AtomicU64::new(0));
        let down_bytes = Arc::new(AtomicU64::new(0));
        spawn_traffic_watcher(up_bytes.clone(), down_bytes.clone());

        Ok(TunHandle {
            server_ip,
            gateway: String::new(),
            up_bytes,
            down_bytes,
        })
    }

    pub fn down(handle: &TunHandle) -> Result<(), String> {
        let script = format!(
            r#"netsh interface ipv4 delete route 0.0.0.0/1 "wintun" >$null 2>&1
netsh interface ipv4 delete route 128.0.0.0/1 "wintun" >$null 2>&1
route delete {server_ip} >$null 2>&1
Get-Process tun2socks-* -ErrorAction SilentlyContinue | Stop-Process -Force
"#,
            server_ip = handle.server_ip,
        );
        run_elevated(&script, "down")
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::process::Command;

    /// Best-effort: `pkexec` (PolicyKit) is the standard one-shot graphical
    /// privilege prompt on most desktop Linux distros; not verified here.
    fn primary_interface() -> Result<String, String> {
        let output = Command::new("sh")
            .arg("-c")
            .arg("ip route show default | awk '{print $5; exit}'")
            .output()
            .map_err(|e| format!("ip route: {e}"))?;
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if name.is_empty() {
            Err("Не удалось определить основной сетевой интерфейс".to_string())
        } else {
            Ok(name)
        }
    }

    fn default_gateway() -> Result<String, String> {
        let output = Command::new("sh")
            .arg("-c")
            .arg("ip route show default | awk '{print $3; exit}'")
            .output()
            .map_err(|e| format!("ip route: {e}"))?;
        let gw = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if gw.is_empty() {
            Err("Не удалось определить шлюз по умолчанию".to_string())
        } else {
            Ok(gw)
        }
    }

    fn run_elevated(script: &str, tag: &str) -> Result<(), String> {
        let script_path = std::env::temp_dir().join(format!("riekko-tun-{tag}.sh"));
        std::fs::write(&script_path, script).map_err(|e| format!("Не удалось записать скрипт: {e}"))?;
        let output = Command::new("pkexec")
            .arg("/bin/bash")
            .arg(&script_path)
            .output()
            .map_err(|e| format!("pkexec: {e} (нужен PolicyKit)"))?;
        let _ = std::fs::remove_file(&script_path);
        if !output.status.success() {
            return Err(format!(
                "Настройка TUN не удалась: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }

    pub fn up(socks_port: u16, server_host: &str) -> Result<TunHandle, String> {
        let tun2socks = sidecar_path("tun2socks")?;
        let interface = primary_interface()?;
        let gateway = default_gateway()?;
        let server_ip = resolve_ipv4(server_host)?;

        let script = format!(
            r#"set -e
ip tuntap add mode tun dev {device}
ip addr add {ip}/15 dev {device}
ip link set dev {device} up
ip route add {server_ip} via {gateway}
ip route add 0.0.0.0/1 via {ip} dev {device}
ip route add 128.0.0.0/1 via {ip} dev {device}
nohup "{tun2socks}" --device {device} --proxy socks5://127.0.0.1:{port} --interface {iface} --restapi 127.0.0.1:{restapi} --loglevel silent > /tmp/riekko-tun2socks.log 2>&1 &
disown
echo $! > /tmp/riekko-tun2socks.pid
"#,
            device = TUN_DEVICE,
            ip = TUN_IP,
            server_ip = server_ip,
            gateway = gateway,
            iface = interface,
            port = socks_port,
            restapi = RESTAPI_PORT,
            tun2socks = tun2socks.display(),
        );

        run_elevated(&script, "up")?;

        let up_bytes = Arc::new(AtomicU64::new(0));
        let down_bytes = Arc::new(AtomicU64::new(0));
        spawn_traffic_watcher(up_bytes.clone(), down_bytes.clone());

        Ok(TunHandle {
            server_ip,
            gateway,
            up_bytes,
            down_bytes,
        })
    }

    pub fn down(handle: &TunHandle) -> Result<(), String> {
        let script = format!(
            r#"PID=$(cat /tmp/riekko-tun2socks.pid 2>/dev/null || echo "")
if [ -n "$PID" ]; then kill "$PID" 2>/dev/null || true; fi
ip route del {server_ip} via {gateway} 2>/dev/null || true
ip route del 0.0.0.0/1 dev {device} 2>/dev/null || true
ip route del 128.0.0.0/1 dev {device} 2>/dev/null || true
ip link set dev {device} down 2>/dev/null || true
ip tuntap del mode tun dev {device} 2>/dev/null || true
rm -f /tmp/riekko-tun2socks.pid /tmp/riekko-tun2socks.log
"#,
            device = TUN_DEVICE,
            server_ip = handle.server_ip,
            gateway = handle.gateway,
        );
        run_elevated(&script, "down")
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod platform {
    use super::*;

    pub fn up(_socks_port: u16, _server_host: &str) -> Result<TunHandle, String> {
        Err("TUN-режим не поддерживается на этой платформе".to_string())
    }
    pub fn down(_handle: &TunHandle) -> Result<(), String> {
        Ok(())
    }
}

pub fn up(socks_port: u16, server_host: &str) -> Result<TunHandle, String> {
    platform::up(socks_port, server_host)
}
