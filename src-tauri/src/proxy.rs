//! Points the OS at the local SOCKS5 listener the core process opens.
//! Spawning xray/hysteria alone gets traffic nowhere — an app has to
//! actually be told to use that proxy. System-wide proxy settings are what
//! makes browsers and most apps route through the tunnel without per-app
//! configuration; it's not full TUN-level capture (a UDP/ICMP packet or an
//! app that ignores system proxy settings won't be covered), but it's the
//! same mechanism plain SOCKS-based clients have relied on for years, and a
//! prerequisite either way.

#[cfg(target_os = "macos")]
mod platform {
    use std::process::Command;

    fn network_services() -> Vec<String> {
        let Ok(output) = Command::new("networksetup")
            .arg("-listallnetworkservices")
            .output()
        else {
            return Vec::new();
        };
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .skip(1) // header line: "An asterisk (*) denotes that a network service is disabled."
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('*'))
            .map(|l| l.to_string())
            .collect()
    }

    pub fn enable(port: u16) -> Result<(), String> {
        let services = network_services();
        if services.is_empty() {
            return Err("Не удалось получить список сетевых служб (networksetup)".to_string());
        }
        for service in services {
            let _ = Command::new("networksetup")
                .args(["-setsocksfirewallproxy", &service, "127.0.0.1", &port.to_string()])
                .output();
            let _ = Command::new("networksetup")
                .args(["-setsocksfirewallproxystate", &service, "on"])
                .output();
        }
        Ok(())
    }

    pub fn disable() -> Result<(), String> {
        for service in network_services() {
            let _ = Command::new("networksetup")
                .args(["-setsocksfirewallproxystate", &service, "off"])
                .output();
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use std::process::Command;

    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";

    pub fn enable(port: u16) -> Result<(), String> {
        let proxy = format!("socks=127.0.0.1:{port}");
        Command::new("reg")
            .args(["add", KEY, "/v", "ProxyServer", "/d", &proxy, "/f"])
            .output()
            .map_err(|e| format!("reg add ProxyServer: {e}"))?;
        Command::new("reg")
            .args(["add", KEY, "/v", "ProxyEnable", "/t", "REG_DWORD", "/d", "1", "/f"])
            .output()
            .map_err(|e| format!("reg add ProxyEnable: {e}"))?;
        Ok(())
    }

    pub fn disable() -> Result<(), String> {
        Command::new("reg")
            .args(["add", KEY, "/v", "ProxyEnable", "/t", "REG_DWORD", "/d", "0", "/f"])
            .output()
            .map_err(|e| format!("reg add ProxyEnable: {e}"))?;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::process::Command;

    /// Best-effort GNOME/GSettings support. Other desktop environments
    /// (KDE, Sway, ...) don't share a common proxy API and aren't covered.
    pub fn enable(port: u16) -> Result<(), String> {
        let _ = Command::new("gsettings")
            .args(["set", "org.gnome.system.proxy", "mode", "manual"])
            .output();
        let _ = Command::new("gsettings")
            .args(["set", "org.gnome.system.proxy.socks", "host", "127.0.0.1"])
            .output();
        Command::new("gsettings")
            .args([
                "set",
                "org.gnome.system.proxy.socks",
                "port",
                &port.to_string(),
            ])
            .output()
            .map_err(|e| format!("gsettings: {e}"))?;
        Ok(())
    }

    pub fn disable() -> Result<(), String> {
        Command::new("gsettings")
            .args(["set", "org.gnome.system.proxy", "mode", "none"])
            .output()
            .map_err(|e| format!("gsettings: {e}"))?;
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod platform {
    pub fn enable(_port: u16) -> Result<(), String> {
        Err("Системный прокси не поддерживается на этой платформе".to_string())
    }
    pub fn disable() -> Result<(), String> {
        Ok(())
    }
}

pub fn enable_system_proxy(port: u16) -> Result<(), String> {
    platform::enable(port)
}

pub fn disable_system_proxy() -> Result<(), String> {
    platform::disable()
}
