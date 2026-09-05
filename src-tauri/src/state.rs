use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TunnelState {
    Idle,
    Starting,
    Connected,
    Reconnecting,
    Error,
}

impl TunnelState {
    pub fn is_active(self) -> bool {
        matches!(self, TunnelState::Connected | TunnelState::Reconnecting)
    }
}

/// How traffic is actually being routed while connected — shown in the UI
/// so "Connected" never silently means less than it looks like.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RoutingMode {
    /// Full-system capture via a TUN interface — everything goes through
    /// the tunnel, not just proxy-aware apps.
    Tun,
    /// Fallback when TUN setup failed or was declined (e.g. the admin
    /// password prompt was cancelled): only the system SOCKS proxy is set,
    /// which most apps honor but not all.
    SystemProxy,
}

/// The one group that always exists and can't be renamed or deleted.
/// Freshly imported single keys land here; user-created groups and
/// subscription imports get their own id in `AppState::groups`.
pub const UNGROUPED_ID: &str = "ungrouped";

#[derive(Serialize, Deserialize, Clone)]
pub struct Group {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub transport: String,
    pub protocol: String,
    pub group_id: String,
    /// The original share link, kept so a real transport can re-parse
    /// credentials later. `None` for manually entered profiles.
    pub uri: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Settings {
    pub auto_connect: bool,
    pub start_with_system: bool,
    pub notifications: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_connect: false,
            start_with_system: false,
            notifications: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SessionInfo {
    pub endpoint: String,
    pub latency_ms: u32,
    pub uptime_secs: u64,
    pub rx_mb: f64,
    pub tx_mb: f64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct AppState {
    pub state: TunnelState,
    pub routing_mode: Option<RoutingMode>,
    pub groups: Vec<Group>,
    pub profiles: Vec<Profile>,
    pub active_profile_id: String,
    pub settings: Settings,
    pub session: SessionInfo,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            state: TunnelState::Idle,
            routing_mode: None,
            groups: Vec::new(),
            profiles: Vec::new(),
            active_profile_id: String::new(),
            settings: Settings::default(),
            session: SessionInfo {
                endpoint: String::new(),
                latency_ms: 0,
                uptime_secs: 0,
                rx_mb: 0.0,
                tx_mb: 0.0,
            },
        }
    }
}

/// A cheap source of variance for mock telemetry and generated ids, until a
/// real transport reports actual numbers.
pub fn pseudo_random(seed_offset: u64) -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        .wrapping_add(seed_offset)
}
