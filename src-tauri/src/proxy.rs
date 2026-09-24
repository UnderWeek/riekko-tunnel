//! Points the OS at the local SOCKS5 listener the core process opens.
//! Spawning xray/hysteria alone gets traffic nowhere — an app has to
//! actually be told to use that proxy. System-wide proxy settings are what
//! makes browsers and most apps route through the tunnel without per-app
//! configuration; it's not full TUN-level capture (a UDP/ICMP packet or an
//! app that ignores system proxy settings won't be covered), but it's the
//! same mechanism plain SOCKS-based clients have relied on for years, and a
//! prerequisite either way.
//!
//! Whatever proxy setup the user had before is snapshotted first and put
//! back on disconnect — blindly switching the proxy "off" would wipe out a
//! corporate proxy the user relies on. The snapshot is also written to disk
//! before anything changes, so if the app crashes while the proxy points
//! at a dead port, the next launch restores it (`recover_after_crash`).

use crate::engine::LocalPorts;
use crate::sys;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Owns the "system proxy points at us" state; dropping it restores the
/// user's previous settings.
pub struct SystemProxyGuard {
    saved: platform::Saved,
    marker: PathBuf,
}

impl SystemProxyGuard {
    /// Snapshots the current settings, records the snapshot at `marker`,
    /// then points the system proxy at the core's local listeners.
    pub fn enable(ports: LocalPorts, marker: &Path) -> Result<Self, String> {
        let saved = platform::snapshot()?;
        if let Some(dir) = marker.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let json = serde_json::to_vec(&saved).map_err(|e| e.to_string())?;
        std::fs::write(marker, json)
            .map_err(|e| format!("Не удалось сохранить настройки прокси: {e}"))?;
        if let Err(e) = platform::enable(ports) {
            platform::restore(&saved);
            let _ = std::fs::remove_file(marker);
            return Err(e);
        }
        Ok(Self {
            saved,
            marker: marker.to_path_buf(),
        })
    }
}

impl Drop for SystemProxyGuard {
    fn drop(&mut self) {
        platform::restore(&self.saved);
        let _ = std::fs::remove_file(&self.marker);
    }
}

/// Restores proxy settings left pointing at a dead port by a session that
/// never got to clean up (crash, force quit, power loss).
pub fn recover_after_crash(marker: &Path) {
    let Ok(bytes) = std::fs::read(marker) else {
        return;
    };
    if let Ok(saved) = serde_json::from_slice::<platform::Saved>(&bytes) {
        platform::restore(&saved);
    }
    let _ = std::fs::remove_file(marker);
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    #[derive(Serialize, Deserialize)]
    pub struct ServiceSocks {
        service: String,
        enabled: bool,
        server: String,
        port: String,
    }

    #[derive(Serialize, Deserialize)]
    pub struct Saved(Vec<ServiceSocks>);

    fn networksetup(args: &[&str]) -> Result<String, String> {
        let output = sys::command("networksetup")
            .args(args)
            .output()
            .map_err(|e| format!("networksetup: {e}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        // networksetup reports most failures on stdout with exit code 0.
        if !output.status.success() || stdout.contains("** Error") {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "networksetup: {}",
                format!("{stdout}{stderr}").trim()
            ));
        }
        Ok(stdout)
    }

    fn network_services() -> Vec<String> {
        networksetup(&["-listallnetworkservices"])
            .unwrap_or_default()
            .lines()
            .skip(1) // header line: "An asterisk (*) denotes that a network service is disabled."
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('*'))
            .map(|l| l.to_string())
            .collect()
    }

    pub fn snapshot() -> Result<Saved, String> {
        let services = network_services();
        if services.is_empty() {
            return Err("Не удалось получить список сетевых служб (networksetup)".to_string());
        }
        let mut saved = Vec::new();
        for service in services {
            let text = networksetup(&["-getsocksfirewallproxy", &service]).unwrap_or_default();
            let field = |name: &str| {
                text.lines()
                    .find_map(|l| l.strip_prefix(name))
                    .map(|v| v.trim().to_string())
                    .unwrap_or_default()
            };
            saved.push(ServiceSocks {
                enabled: field("Enabled:") == "Yes",
                server: field("Server:"),
                port: field("Port:"),
                service,
            });
        }
        Ok(Saved(saved))
    }

    pub fn enable(ports: LocalPorts) -> Result<(), String> {
        let port = ports.socks.to_string();
        for service in network_services() {
            networksetup(&["-setsocksfirewallproxy", &service, "127.0.0.1", &port])?;
            networksetup(&["-setsocksfirewallproxystate", &service, "on"])?;
        }
        Ok(())
    }

    pub fn restore(saved: &Saved) {
        for s in &saved.0 {
            if !s.server.is_empty() {
                let port = if s.port.is_empty() {
                    "0"
                } else {
                    s.port.as_str()
                };
                let _ = networksetup(&["-setsocksfirewallproxy", &s.service, &s.server, port]);
            }
            let state = if s.enabled { "on" } else { "off" };
            let _ = networksetup(&["-setsocksfirewallproxystate", &s.service, state]);
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;

    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    /// Local and private-network destinations stay direct, or proxy mode
    /// would cut the user off from their own LAN (printers, NAS, router).
    const BYPASS: &str = "localhost;127.*;10.*;172.16.*;172.17.*;172.18.*;172.19.*;172.20.*;\
172.21.*;172.22.*;172.23.*;172.24.*;172.25.*;172.26.*;172.27.*;172.28.*;172.29.*;172.30.*;\
172.31.*;192.168.*;<local>";

    #[derive(Serialize, Deserialize)]
    pub struct Saved {
        enable: Option<u32>,
        server: Option<String>,
        overrides: Option<String>,
    }

    fn query(name: &str) -> Option<String> {
        let output = sys::command("reg")
            .args(["query", KEY, "/v", name])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        // "    ProxyServer    REG_SZ    socks=127.0.0.1:1080"
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| {
                let mut parts = line.trim().splitn(3, "    ");
                match (parts.next(), parts.next(), parts.next()) {
                    (Some(n), Some(_ty), value) if n.eq_ignore_ascii_case(name) => {
                        Some(value.unwrap_or("").trim().to_string())
                    }
                    _ => None,
                }
            })
    }

    fn set(name: &str, kind: &str, data: &str) -> Result<(), String> {
        let status = sys::command("reg")
            .args(["add", KEY, "/v", name, "/t", kind, "/d", data, "/f"])
            .status()
            .map_err(|e| format!("reg add {name}: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("reg add {name} завершился с ошибкой"))
        }
    }

    fn delete(name: &str) {
        let _ = sys::command("reg")
            .args(["delete", KEY, "/v", name, "/f"])
            .status();
    }

    /// Tells WinINet (and every app reading its settings: Edge, Chrome,
    /// most of the system) that the registry changed. Without this,
    /// running apps keep using the old settings until they restart.
    fn notify_settings_changed() {
        #[link(name = "wininet")]
        extern "system" {
            fn InternetSetOptionW(
                h_internet: *mut std::ffi::c_void,
                option: u32,
                buffer: *mut std::ffi::c_void,
                length: u32,
            ) -> i32;
        }
        const INTERNET_OPTION_REFRESH: u32 = 37;
        const INTERNET_OPTION_SETTINGS_CHANGED: u32 = 39;
        // SAFETY: both options are documented to take a NULL handle and
        // no buffer.
        unsafe {
            InternetSetOptionW(
                std::ptr::null_mut(),
                INTERNET_OPTION_SETTINGS_CHANGED,
                std::ptr::null_mut(),
                0,
            );
            InternetSetOptionW(
                std::ptr::null_mut(),
                INTERNET_OPTION_REFRESH,
                std::ptr::null_mut(),
                0,
            );
        }
    }

    pub fn snapshot() -> Result<Saved, String> {
        let enable = query("ProxyEnable").and_then(|v| {
            let v = v.trim();
            match v.strip_prefix("0x") {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => v.parse().ok(),
            }
        });
        Ok(Saved {
            enable,
            server: query("ProxyServer"),
            overrides: query("ProxyOverride"),
        })
    }

    /// An HTTP proxy for every protocol: WinINet and Chromium read a
    /// `socks=` entry as SOCKS4, which Hysteria's listener rejects and which
    /// would resolve DNS locally even with Xray.
    pub fn enable(ports: LocalPorts) -> Result<(), String> {
        set(
            "ProxyServer",
            "REG_SZ",
            &format!("127.0.0.1:{}", ports.http),
        )?;
        set("ProxyOverride", "REG_SZ", BYPASS)?;
        set("ProxyEnable", "REG_DWORD", "1")?;
        notify_settings_changed();
        Ok(())
    }

    pub fn restore(saved: &Saved) {
        match &saved.server {
            Some(server) => {
                let _ = set("ProxyServer", "REG_SZ", server);
            }
            None => delete("ProxyServer"),
        }
        match &saved.overrides {
            Some(overrides) => {
                let _ = set("ProxyOverride", "REG_SZ", overrides);
            }
            None => delete("ProxyOverride"),
        }
        let _ = set(
            "ProxyEnable",
            "REG_DWORD",
            &saved.enable.unwrap_or(0).to_string(),
        );
        notify_settings_changed();
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    /// Best-effort GNOME/GSettings support. Other desktop environments
    /// (KDE, Sway, ...) don't share a common proxy API and aren't covered —
    /// which is reported as an error instead of pretending it worked.
    #[derive(Serialize, Deserialize)]
    pub struct Saved {
        mode: String,
        host: String,
        port: String,
    }

    fn gsettings(args: &[&str]) -> Result<String, String> {
        let output = sys::command("gsettings")
            .args(args)
            .output()
            .map_err(|e| format!("gsettings: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "Системный прокси доступен только в GNOME-совместимых окружениях ({})",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    pub fn snapshot() -> Result<Saved, String> {
        // Values are kept in GSettings' own text format ('none', 0, ...),
        // which `gsettings set` accepts back verbatim.
        Ok(Saved {
            mode: gsettings(&["get", "org.gnome.system.proxy", "mode"])?,
            host: gsettings(&["get", "org.gnome.system.proxy.socks", "host"])?,
            port: gsettings(&["get", "org.gnome.system.proxy.socks", "port"])?,
        })
    }

    pub fn enable(ports: LocalPorts) -> Result<(), String> {
        gsettings(&["set", "org.gnome.system.proxy.socks", "host", "127.0.0.1"])?;
        gsettings(&[
            "set",
            "org.gnome.system.proxy.socks",
            "port",
            &ports.socks.to_string(),
        ])?;
        gsettings(&["set", "org.gnome.system.proxy", "mode", "manual"])?;
        Ok(())
    }

    pub fn restore(saved: &Saved) {
        let _ = gsettings(&["set", "org.gnome.system.proxy.socks", "host", &saved.host]);
        let _ = gsettings(&["set", "org.gnome.system.proxy.socks", "port", &saved.port]);
        let _ = gsettings(&["set", "org.gnome.system.proxy", "mode", &saved.mode]);
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod platform {
    use super::*;

    #[derive(Serialize, Deserialize)]
    pub struct Saved;

    pub fn snapshot() -> Result<Saved, String> {
        Err("Системный прокси не поддерживается на этой платформе".to_string())
    }
    pub fn enable(_ports: LocalPorts) -> Result<(), String> {
        Err("Системный прокси не поддерживается на этой платформе".to_string())
    }
    pub fn restore(_saved: &Saved) {}
}
