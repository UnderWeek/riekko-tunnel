use crate::engine::{self, ActiveConnection};
use crate::import::{self, ConnectParams};
use crate::proxy::SystemProxyGuard;
use crate::state::{
    unique_id, AppState, Group, RoutingMode, SessionInfo, TunnelState, UNGROUPED_ID,
};
use crate::store;
use crate::tun::{self, TunHandle, TunRequest, TunStatus};
use futures_util::StreamExt;
use serde::Serialize;
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_notification::NotificationExt as _;
use url::Url;

/// Where things live on disk. Resolved once at startup.
pub struct Paths {
    pub state_file: PathBuf,
    pub proxy_marker: PathBuf,
    pub tun_dir: PathBuf,
}

impl Paths {
    fn resolve(app: &AppHandle) -> Self {
        let fallback = std::env::temp_dir().join("riekko-tunnel");
        let config = app
            .path()
            .app_config_dir()
            .unwrap_or_else(|_| fallback.clone());
        let local = app.path().app_local_data_dir().unwrap_or(fallback);
        Self {
            state_file: config.join("state.json"),
            proxy_marker: local.join("proxy-restore.json"),
            tun_dir: local.join("tun"),
        }
    }
}

/// Bookkeeping for the running session that the UI doesn't need to see.
#[derive(Default)]
struct Runtime {
    connected_at: Option<Instant>,
    /// The exact address the core dials — what latency is measured to.
    latency_target: Option<SocketAddr>,
}

pub struct AppData {
    pub state: Mutex<AppState>,
    pub connection: Mutex<Option<ActiveConnection>>,
    pub tun: Mutex<Option<TunHandle>>,
    pub proxy: Mutex<Option<SystemProxyGuard>>,
    runtime: Mutex<Runtime>,
    /// Set while a connect/disconnect/teardown is in flight, so a double
    /// click can't start two cores and a tick can't tear down a tunnel
    /// that is still being built.
    busy: AtomicBool,
    pub exiting: AtomicBool,
    /// Set when the saved library exists but couldn't be read: saving over
    /// it would destroy it.
    persist_blocked: AtomicBool,
    paths: Mutex<Option<Paths>>,
}

impl Default for AppData {
    fn default() -> Self {
        Self {
            state: Mutex::new(AppState::default()),
            connection: Mutex::new(None),
            tun: Mutex::new(None),
            proxy: Mutex::new(None),
            runtime: Mutex::new(Runtime::default()),
            busy: AtomicBool::new(false),
            exiting: AtomicBool::new(false),
            persist_blocked: AtomicBool::new(false),
            paths: Mutex::new(None),
        }
    }
}

impl AppData {
    /// Whether a connect/disconnect/teardown is in flight.
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    pub fn has_routing(&self) -> bool {
        self.connection.lock().unwrap().is_some()
            || self.tun.lock().unwrap().is_some()
            || self.proxy.lock().unwrap().is_some()
    }

    fn with_paths<T>(&self, f: impl FnOnce(&Paths) -> T) -> Option<T> {
        self.paths.lock().unwrap().as_ref().map(f)
    }

    /// Saves the user's library. Called with the state lock held, so saves
    /// can't land out of order.
    fn persist(&self, app: &AppState) {
        if self.persist_blocked.load(Ordering::SeqCst) {
            return;
        }
        if let Some(path) = self.with_paths(|p| p.state_file.clone()) {
            if let Err(e) = store::save(&path, app) {
                eprintln!("riekko: failed to save state: {e}");
            }
        }
    }
}

/// Clears `busy` when dropped, whichever way the operation ends.
struct BusyGuard<'a>(&'a AtomicBool);

impl<'a> BusyGuard<'a> {
    fn acquire(flag: &'a AtomicBool) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| Self(flag))
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Runs blocking work (admin prompts, process spawns, network probes) off
/// the main thread. Tauri runs plain `fn` commands on the main thread, where
/// any of these would freeze the whole window.
async fn blocking<T: Send + 'static>(
    app: AppHandle,
    f: impl FnOnce(AppHandle) -> T + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(move || f(app))
        .await
        .map_err(|e| format!("Внутренняя ошибка: {e}"))
}

/// Loads persisted state, repairs whatever a crashed session left behind
/// and syncs settings with the OS. Called once from `setup`.
pub fn init(app: &AppHandle) {
    let data = app.state::<AppData>();
    let paths = Paths::resolve(app);
    crate::proxy::recover_after_crash(&paths.proxy_marker);
    let mut state = match store::load(&paths.state_file) {
        Ok(loaded) => loaded.unwrap_or_default(),
        Err(store::LoadError::Corrupt(message)) => AppState {
            last_error: Some(message),
            ..AppState::default()
        },
        Err(store::LoadError::Unreadable(message)) => {
            data.persist_blocked.store(true, Ordering::SeqCst);
            AppState {
                last_error: Some(message),
                ..AppState::default()
            }
        }
    };
    // The OS is the source of truth for autostart (the user may have
    // removed the login item by hand).
    if let Ok(enabled) = app.autolaunch().is_enabled() {
        state.settings.start_with_system = enabled;
    }
    let auto_connect =
        state.settings.auto_connect && state.profile(&state.active_profile_id).is_some();
    *data.state.lock().unwrap() = state;
    *data.paths.lock().unwrap() = Some(paths);

    spawn_monitor(app);
    if auto_connect {
        // Once per launch — a WebView reload must not reconnect after the
        // user deliberately disconnected.
        let app = app.clone();
        std::thread::spawn(move || {
            let data = app.state::<AppData>();
            let Some(_busy) = BusyGuard::acquire(&data.busy) else {
                return;
            };
            if data.state.lock().unwrap().state == TunnelState::Idle {
                let _ = connect(&app, &data);
            }
        });
    }
}

/// Watches the session from the backend. The UI's own 1 s poll stops when
/// the window is minimized (WebViews throttle and then suspend hidden
/// timers), and a dead core must still be noticed — otherwise every app
/// keeps routing into a SOCKS port nothing listens on.
fn spawn_monitor(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let data = app.state::<AppData>();
        while !data.exiting.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_secs(1));
            if !data.exiting.load(Ordering::SeqCst) {
                let _ = tick(&app);
            }
        }
    });
}

/// Tears everything down for app exit. Best-effort: if the TUN teardown
/// fails, the watchdog still cleans up once it sees the app is gone.
pub fn shutdown_all(app: &AppHandle) {
    let data = app.state::<AppData>();
    let tun = data.tun.lock().unwrap().take();
    if let Some(mut handle) = tun {
        if handle.shutdown().is_err() {
            // Don't make Drop wait and prompt a second time.
            handle.disarm();
        }
    }
    drop(data.proxy.lock().unwrap().take());
    drop(data.connection.lock().unwrap().take());
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    let enabled = app
        .state::<AppData>()
        .state
        .lock()
        .unwrap()
        .settings
        .notifications;
    if enabled {
        let _ = app.notification().builder().title(title).body(body).show();
    }
}

/// Stops the core and undoes TUN/proxy routing. If the TUN teardown fails
/// (e.g. the fallback admin prompt was declined), the core is kept running
/// and the error returned: killing it while routes still point into the
/// tunnel would leave the machine with no internet at all.
fn release_routing(data: &AppData) -> Result<(), String> {
    let tun = data.tun.lock().unwrap().take();
    if let Some(mut handle) = tun {
        if let Err(e) = handle.shutdown() {
            *data.tun.lock().unwrap() = Some(handle);
            return Err(format!("Не удалось отключить TUN: {e}"));
        }
    }
    drop(data.proxy.lock().unwrap().take());
    drop(data.connection.lock().unwrap().take());
    *data.runtime.lock().unwrap() = Runtime::default();
    Ok(())
}

fn reset_session(app: &mut AppState) {
    app.routing_mode = None;
    app.connected_profile_id = None;
    let endpoint = app
        .profile(&app.active_profile_id)
        .map(|p| p.endpoint.clone())
        .unwrap_or_default();
    app.session = SessionInfo {
        endpoint,
        ..SessionInfo::default()
    };
}

#[tauri::command]
pub fn get_state(data: State<AppData>) -> AppState {
    data.state.lock().unwrap().snapshot()
}

fn measure_latency_ms(addr: SocketAddr) -> Option<u32> {
    let start = Instant::now();
    let elapsed = || (start.elapsed().as_millis() as u32).max(1);
    match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
        Ok(_) => Some(elapsed()),
        // A refusal still took one full round trip — and it's the normal
        // answer from a Hysteria2 server, which only listens on UDP.
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => Some(elapsed()),
        Err(_) => None,
    }
}

/// Measures latency in the background and stores it if the same session
/// is still up by then.
fn spawn_latency_probe(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let data = app.state::<AppData>();
        let Some(target) = data.runtime.lock().unwrap().latency_target else {
            return;
        };
        let latency = measure_latency_ms(target).unwrap_or(0);
        if data.runtime.lock().unwrap().latency_target == Some(target) {
            let mut state = data.state.lock().unwrap();
            if state.state.is_active() {
                state.session.latency_ms = latency;
            }
        }
    });
}

/// Resolves the server once. The same address is then handed to the core
/// *and* routed around the TUN device, so the two can't disagree (DNS
/// round-robin) and the core never has to resolve anything itself — in TUN
/// mode its DNS query would be captured by the very tunnel it serves.
fn resolve_server(host: &str, port: u16) -> Result<IpAddr, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ip);
    }
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("Не удалось определить адрес сервера {host}: {e}"))?
        .collect();
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.first())
        .map(|a| a.ip())
        .ok_or_else(|| format!("Не удалось определить адрес сервера {host}"))
}

#[tauri::command]
pub async fn toggle_connection(app: AppHandle) -> Result<AppState, String> {
    blocking(app, |app| {
        let data = app.state::<AppData>();
        let Some(_busy) = BusyGuard::acquire(&data.busy) else {
            return Err("Подождите — предыдущая операция ещё выполняется".to_string());
        };
        if data.state.lock().unwrap().state.is_active() {
            disconnect(&data)
        } else {
            connect(&app, &data)
        }
    })
    .await?
}

fn disconnect(data: &AppData) -> Result<AppState, String> {
    release_routing(data)?;
    let mut app = data.state.lock().unwrap();
    app.state = TunnelState::Idle;
    app.last_error = None;
    reset_session(&mut app);
    Ok(app.snapshot())
}

fn connect(handle: &AppHandle, data: &AppData) -> Result<AppState, String> {
    let profile = {
        let app = data.state.lock().unwrap();
        app.profile(&app.active_profile_id).cloned()
    };
    let Some(profile) = profile else {
        return Err("Сначала выберите профиль на вкладке «Профили»".to_string());
    };
    let Some(uri) = profile.uri.clone() else {
        return Err("У профиля нет сохранённой ссылки — переимпортируйте его".to_string());
    };

    // Leftovers of a session that ended in an error.
    release_routing(data)?;
    {
        let mut app = data.state.lock().unwrap();
        app.state = TunnelState::Starting;
        app.last_error = None;
        reset_session(&mut app);
        // From here on the UI names the profile being connected, and
        // picking another one doesn't relabel the tunnel being built.
        app.connected_profile_id = Some(profile.id.clone());
        app.session.endpoint = profile.endpoint.clone();
    }

    match establish(handle, data, &uri) {
        Ok((routing_mode, latency_target)) => {
            *data.runtime.lock().unwrap() = Runtime {
                connected_at: Some(Instant::now()),
                latency_target: Some(latency_target),
            };
            let snapshot = {
                let mut app = data.state.lock().unwrap();
                app.state = TunnelState::Connected;
                app.routing_mode = Some(routing_mode);
                app.connected_profile_id = Some(profile.id.clone());
                app.session.endpoint = profile.endpoint.clone();
                app.snapshot()
            };
            spawn_latency_probe(handle);
            Ok(snapshot)
        }
        Err(e) => {
            let mut app = data.state.lock().unwrap();
            app.state = TunnelState::Error;
            app.last_error = Some(e.clone());
            reset_session(&mut app);
            Err(e)
        }
    }
}

/// Starts the core and routes traffic into it. On error nothing is left
/// running: the core is dropped (killed) and no routing is installed.
fn establish(
    handle: &AppHandle,
    data: &AppData,
    uri: &str,
) -> Result<(RoutingMode, SocketAddr), String> {
    let params = import::parse_connect_params(uri)?;
    let server = resolve_server(params.host(), params.port())?;
    let ports = engine::LocalPorts::pick()?;
    let (tun_dir, proxy_marker) = data
        .with_paths(|p| (p.tun_dir.clone(), p.proxy_marker.clone()))
        .ok_or_else(|| "Приложение ещё не инициализировано".to_string())?;
    let core = engine::start(handle, &params, &server.to_string(), ports)?;
    let core_name = match params {
        ConnectParams::Vless(_) => "xray",
        ConnectParams::Hysteria2(_) => "hysteria",
    };
    let core_pid = core.pid();
    // Owned by AppData from the moment it exists, so quitting during the
    // (possibly long) admin prompt still stops it and deletes its config.
    *data.connection.lock().unwrap() = Some(core);
    let drop_core = || drop(data.connection.lock().unwrap().take());

    // The core is up and listening on 127.0.0.1, but nothing
    // routes through it until something tells the OS to actually use it.
    // TUN (full-system capture, one admin/root prompt) is the real thing;
    // if it's unavailable or the prompt is declined, fall back to a plain
    // system SOCKS proxy, which covers most apps but not all traffic.
    let tun_request = TunRequest {
        socks_port: ports.socks,
        server_ip: match server {
            IpAddr::V4(v4) => Some(v4),
            // Only IPv4 is captured, so an IPv6 server needs no exclusion.
            IpAddr::V6(_) => None,
        },
        core_pid,
        work_dir: &tun_dir,
    };
    let routing_mode = match tun::up(tun_request) {
        Ok(tun_handle) => {
            *data.tun.lock().unwrap() = Some(tun_handle);
            RoutingMode::Tun
        }
        Err(tun_err) => match SystemProxyGuard::enable(ports, &proxy_marker) {
            Ok(guard) => {
                *data.proxy.lock().unwrap() = Some(guard);
                RoutingMode::SystemProxy
            }
            Err(proxy_err) => {
                drop_core();
                return Err(format!(
                    "TUN недоступен ({tun_err}), а системный прокси тоже не удалось настроить ({proxy_err})"
                ));
            }
        },
    };

    // The admin prompt can take a while; the core may have given up in the
    // meantime (e.g. Hysteria2 rejecting the password).
    let died = {
        let conn = data.connection.lock().unwrap();
        conn.as_ref()
            .filter(|c| !c.is_alive())
            .map(|c| c.output_tail())
    };
    if let Some(reason) = died {
        let message = format!("{core_name} остановился: {reason}");
        return Err(match release_routing(data) {
            Ok(()) => message,
            Err(e) => format!("{message}. {e}"),
        });
    }
    Ok((routing_mode, SocketAddr::new(server, params.port())))
}

#[tauri::command]
pub async fn tick_session(app: AppHandle) -> Result<AppState, String> {
    blocking(app, |app| tick(&app)).await
}

fn tick(handle: &AppHandle) -> AppState {
    let data = handle.state::<AppData>();

    // A connect/disconnect in flight owns the connection; don't judge it
    // half-built. And only a live session can fail: once a teardown has
    // failed (the user declined the fallback prompt), the state is Error and
    // retrying is left to the user instead of re-prompting every second.
    // The busy flag is only taken once there is a failure to handle, so a
    // routine tick can't make a Connect click bounce with "please wait".
    let active = data.state.lock().unwrap().state.is_active();
    let suspect = active && detect_failure(&data).is_some();
    if let Some(_busy) = suspect.then(|| BusyGuard::acquire(&data.busy)).flatten() {
        let still_active = data.state.lock().unwrap().state.is_active();
        if let Some(failure) = detect_failure(&data).filter(|_| still_active) {
            let teardown = release_routing(&data);
            let message = match teardown {
                Ok(()) => failure,
                Err(e) => format!("{failure}. {e}"),
            };
            {
                let mut app = data.state.lock().unwrap();
                app.state = TunnelState::Error;
                app.last_error = Some(message.clone());
                reset_session(&mut app);
            }
            notify(handle, "Riekko: соединение разорвано", &message);
        }
    }

    let tun_status = data.tun.lock().unwrap().as_ref().map(|t| t.status());
    let traffic = data.tun.lock().unwrap().as_ref().map(|t| t.traffic_bytes());
    let connected_at = data.runtime.lock().unwrap().connected_at;

    let mut app = data.state.lock().unwrap();
    if app.state.is_active() {
        if let Some(since) = connected_at {
            app.session.uptime_secs = since.elapsed().as_secs();
        }
        if let Some((up, down)) = traffic {
            app.session.tx_mb = up as f64 / 1_000_000.0;
            app.session.rx_mb = down as f64 / 1_000_000.0;
        }
        // macOS (and to a lesser extent other OSes) can silently drop our
        // routes on a network change — the core stays alive, but traffic
        // quietly goes out unprotected. The watchdog re-adds them, so a
        // brief "Reconnecting" that heals itself is the honest outcome.
        let previous = app.state;
        match tun_status {
            Some(TunStatus::Degraded) => app.state = TunnelState::Reconnecting,
            Some(TunStatus::Healthy) => app.state = TunnelState::Connected,
            _ => {}
        }
        let changed = (previous != app.state).then_some(app.state);
        let snapshot = app.snapshot();
        drop(app);
        match changed {
            Some(TunnelState::Reconnecting) => notify(
                handle,
                "Riekko: связь прервалась",
                "Система перестала направлять трафик в туннель — восстанавливаем маршруты…",
            ),
            Some(TunnelState::Connected) => notify(
                handle,
                "Riekko: соединение восстановлено",
                "Трафик снова идёт через туннель.",
            ),
            _ => {}
        }
        return snapshot;
    }
    app.snapshot()
}

/// Why the running session is dead, if it is.
fn detect_failure(data: &AppData) -> Option<String> {
    {
        let conn = data.connection.lock().unwrap();
        if let Some(core) = conn.as_ref().filter(|c| !c.is_alive()) {
            let reason = core.output_tail();
            return Some(if reason.is_empty() {
                "Процесс туннеля неожиданно завершился".to_string()
            } else {
                format!("Процесс туннеля неожиданно завершился: {reason}")
            });
        }
    }
    let tun = data.tun.lock().unwrap();
    if tun.as_ref().map(|t| t.status()) == Some(TunStatus::Gone) {
        return Some("TUN-интерфейс неожиданно закрылся".to_string());
    }
    None
}

#[tauri::command]
pub async fn refresh_session(app: AppHandle) -> Result<AppState, String> {
    blocking(app, |app| {
        let data = app.state::<AppData>();
        let target = data.runtime.lock().unwrap().latency_target;
        if let Some(target) = target {
            let latency = measure_latency_ms(target).unwrap_or(0);
            let mut state = data.state.lock().unwrap();
            if state.state.is_active()
                && data.runtime.lock().unwrap().latency_target == Some(target)
            {
                state.session.latency_ms = latency;
            }
        }
        let snapshot = data.state.lock().unwrap().snapshot();
        snapshot
    })
    .await
}

#[tauri::command]
pub fn select_profile(id: String, data: State<AppData>) -> AppState {
    let mut app = data.state.lock().unwrap();
    app.select(&id);
    data.persist(&app);
    app.snapshot()
}

#[tauri::command]
pub fn import_profile(uri: String, data: State<AppData>) -> Result<AppState, String> {
    let profile = import::parse_uri(&uri)?;
    let mut app = data.state.lock().unwrap();
    if app.profiles.iter().any(|p| p.uri == profile.uri) {
        return Err("Этот ключ уже добавлен".to_string());
    }
    app.profiles.push(profile);
    app.ensure_selection();
    data.persist(&app);
    Ok(app.snapshot())
}

#[tauri::command]
pub fn remove_profile(id: String, data: State<AppData>) -> AppState {
    let mut app = data.state.lock().unwrap();
    app.profiles.retain(|p| p.id != id);
    app.ensure_selection();
    data.persist(&app);
    app.snapshot()
}

#[tauri::command]
pub fn update_setting(
    key: String,
    value: bool,
    app: AppHandle,
    data: State<AppData>,
) -> Result<AppState, String> {
    if key == "start_with_system" {
        let autolaunch = app.autolaunch();
        let result = if value {
            autolaunch.enable()
        } else {
            autolaunch.disable()
        };
        result.map_err(|e| format!("Не удалось изменить автозапуск: {e}"))?;
    }
    let mut state = data.state.lock().unwrap();
    match key.as_str() {
        "auto_connect" => state.settings.auto_connect = value,
        "start_with_system" => state.settings.start_with_system = value,
        "notifications" => state.settings.notifications = value,
        other => return Err(format!("Неизвестная настройка: {other}")),
    }
    data.persist(&state);
    Ok(state.snapshot())
}

#[tauri::command]
pub fn create_group(name: String, data: State<AppData>) -> AppState {
    let mut app = data.state.lock().unwrap();
    let trimmed = name.trim();
    let final_name = if trimmed.is_empty() {
        "Новая группа".to_string()
    } else {
        trimmed.to_string()
    };
    app.groups.push(Group {
        id: unique_id("group"),
        name: final_name,
    });
    data.persist(&app);
    app.snapshot()
}

#[tauri::command]
pub fn rename_group(id: String, name: String, data: State<AppData>) -> AppState {
    let mut app = data.state.lock().unwrap();
    let trimmed = name.trim();
    if !trimmed.is_empty() {
        if let Some(group) = app.groups.iter_mut().find(|g| g.id == id) {
            group.name = trimmed.to_string();
        }
    }
    data.persist(&app);
    app.snapshot()
}

#[tauri::command]
pub fn delete_group(id: String, data: State<AppData>) -> AppState {
    let mut app = data.state.lock().unwrap();
    app.groups.retain(|g| g.id != id);
    for profile in app.profiles.iter_mut() {
        if profile.group_id == id {
            profile.group_id = UNGROUPED_ID.to_string();
        }
    }
    data.persist(&app);
    app.snapshot()
}

#[tauri::command]
pub fn move_profile_to_group(
    profile_id: String,
    group_id: String,
    data: State<AppData>,
) -> AppState {
    let mut app = data.state.lock().unwrap();
    let target = if group_id == UNGROUPED_ID || app.groups.iter().any(|g| g.id == group_id) {
        group_id
    } else {
        UNGROUPED_ID.to_string()
    };
    if let Some(profile) = app.profiles.iter_mut().find(|p| p.id == profile_id) {
        profile.group_id = target;
    }
    data.persist(&app);
    app.snapshot()
}

#[derive(Serialize, Clone)]
pub struct ImportSubscriptionResult {
    pub state: AppState,
    pub added: usize,
    pub group_name: String,
}

const MAX_SUBSCRIPTION_BYTES: usize = 2_000_000;

/// Downloads a subscription with a hard timeout and size cap enforced while
/// streaming — not after buffering an arbitrarily large body.
async fn fetch_subscription(url: Url) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("RiekkoTunnel/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("Не удалось загрузить подписку: {e}"))?;
    let response = client.get(url).send().await.map_err(|e| {
        if e.is_timeout() {
            "Сервер подписки не ответил вовремя".to_string()
        } else {
            "Не удалось загрузить подписку".to_string()
        }
    })?;
    if !response.status().is_success() {
        return Err(format!("Сервер вернул ошибку {}", response.status()));
    }
    if response.content_length().unwrap_or(0) as usize > MAX_SUBSCRIPTION_BYTES {
        return Err("Файл подписки слишком большой".to_string());
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "Не удалось прочитать содержимое подписки".to_string())?;
        body.extend_from_slice(&chunk);
        if body.len() > MAX_SUBSCRIPTION_BYTES {
            return Err("Файл подписки слишком большой".to_string());
        }
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

#[tauri::command]
pub async fn import_subscription(
    url: String,
    data: State<'_, AppData>,
) -> Result<ImportSubscriptionResult, String> {
    let parsed = Url::parse(url.trim()).map_err(|_| "Не удалось разобрать ссылку".to_string())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("Подписка должна начинаться с http:// или https://".to_string());
    }

    let body = fetch_subscription(parsed.clone()).await?;
    let group_name = import::group_name_from_url(&parsed);
    let profiles = import::parse_subscription_body(&body);
    if profiles.is_empty() {
        return Err("В файле не найдено ни одного поддерживаемого ключа".to_string());
    }

    let mut app = data.state.lock().unwrap();
    let group_id = if let Some(existing) = app.groups.iter().find(|g| g.name == group_name) {
        existing.id.clone()
    } else {
        let id = unique_id("group");
        app.groups.push(Group {
            id: id.clone(),
            name: group_name.clone(),
        });
        id
    };

    // Re-importing (refreshing) a subscription must not duplicate every key.
    let mut added = 0;
    for mut profile in profiles {
        if app.profiles.iter().any(|p| p.uri == profile.uri) {
            continue;
        }
        profile.group_id = group_id.clone();
        app.profiles.push(profile);
        added += 1;
    }
    app.ensure_selection();
    data.persist(&app);

    Ok(ImportSubscriptionResult {
        state: app.snapshot(),
        added,
        group_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn latency_counts_a_refused_connection_as_a_round_trip() {
        // Grab a port, then free it: connecting now gets an instant RST.
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let latency = measure_latency_ms(SocketAddr::from(([127, 0, 0, 1], port)));
        assert!(latency.is_some_and(|ms| ms >= 1));
    }

    #[test]
    fn resolve_server_passes_ip_literals_through() {
        assert_eq!(
            resolve_server("203.0.113.7", 443).unwrap().to_string(),
            "203.0.113.7"
        );
        assert_eq!(
            resolve_server("2001:db8::1", 443).unwrap().to_string(),
            "2001:db8::1"
        );
        assert!(resolve_server("localhost", 443).is_ok());
    }

    #[test]
    fn busy_guard_is_exclusive_and_released_on_drop() {
        let flag = AtomicBool::new(false);
        let guard = BusyGuard::acquire(&flag).unwrap();
        assert!(BusyGuard::acquire(&flag).is_none());
        drop(guard);
        assert!(BusyGuard::acquire(&flag).is_some());
    }
}
