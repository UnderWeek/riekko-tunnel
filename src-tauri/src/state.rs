use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
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
#[serde(default)]
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

#[derive(Serialize, Deserialize, Clone, Default)]
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
    /// The profile the user picked — what the next connect will use.
    pub active_profile_id: String,
    /// The profile the running tunnel was actually started with. Differs
    /// from `active_profile_id` when the user picks another profile while
    /// connected: the tunnel keeps going to the old server until reconnect.
    pub connected_profile_id: Option<String>,
    /// Why the last connect attempt failed or the tunnel dropped, so an
    /// `ERROR` state always comes with an explanation.
    pub last_error: Option<String>,
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
            connected_profile_id: None,
            last_error: None,
            settings: Settings::default(),
            session: SessionInfo::default(),
        }
    }
}

impl AppState {
    pub fn profile(&self, id: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    /// Picks a profile for the next connect and, while nothing is
    /// connected, shows its endpoint. A running tunnel keeps displaying the
    /// server it is really connected to.
    pub fn select(&mut self, id: &str) {
        let Some(endpoint) = self.profile(id).map(|p| p.endpoint.clone()) else {
            return;
        };
        self.active_profile_id = id.to_string();
        if self.connected_profile_id.is_none() {
            self.session.endpoint = endpoint;
        }
    }

    /// Makes sure `active_profile_id` points at an existing profile,
    /// falling back to the first one (or none).
    pub fn ensure_selection(&mut self) {
        let current = self
            .profile(&self.active_profile_id)
            .or_else(|| self.profiles.first())
            .map(|p| p.id.clone());
        match current {
            Some(id) => self.select(&id),
            None => {
                self.active_profile_id.clear();
                if self.connected_profile_id.is_none() {
                    self.session.endpoint.clear();
                }
            }
        }
    }
}

/// The part of `AppState` that survives a restart: the user's library and
/// preferences, not the runtime connection status.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedState {
    pub groups: Vec<Group>,
    pub profiles: Vec<Profile>,
    pub active_profile_id: String,
    pub settings: Settings,
}

impl PersistedState {
    pub fn from_app(app: &AppState) -> Self {
        Self {
            groups: app.groups.clone(),
            profiles: app.profiles.clone(),
            active_profile_id: app.active_profile_id.clone(),
            settings: app.settings.clone(),
        }
    }

    pub fn into_app(self) -> AppState {
        let mut app = AppState {
            groups: self.groups,
            profiles: self.profiles,
            active_profile_id: self.active_profile_id,
            settings: self.settings,
            ..AppState::default()
        };
        // A group deleted by a buggy older build (or a hand-edited file)
        // must not make its profiles invisible.
        let known: Vec<String> = app.groups.iter().map(|g| g.id.clone()).collect();
        for profile in app.profiles.iter_mut() {
            if profile.group_id != UNGROUPED_ID && !known.contains(&profile.group_id) {
                profile.group_id = UNGROUPED_ID.to_string();
            }
        }
        app.ensure_selection();
        app
    }
}

/// A process-unique id: wall-clock nanos alone can repeat (coarse clocks,
/// several ids minted in one call), so a counter is mixed in.
pub fn unique_id(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{nanos:x}-{n}")
}
