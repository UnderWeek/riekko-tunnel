use crate::engine::{self, ActiveConnection};
use crate::import::{self, ConnectParams};
use crate::state::{pseudo_random, AppState, Group, RoutingMode, TunnelState, UNGROUPED_ID};
use crate::tun::{self, TunHandle};
use serde::Serialize;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, State};
use url::Url;

pub struct AppData {
    pub state: Mutex<AppState>,
    pub connection: Mutex<Option<ActiveConnection>>,
    pub tun: Mutex<Option<TunHandle>>,
}

/// Tears down whatever routing is currently active (TUN, system proxy, or
/// both) and clears the bookkeeping. Called on manual disconnect and when
/// the core process is found to have died unexpectedly.
///
/// Dropping the `TunHandle` (not an explicit call) is what actually runs
/// the privileged teardown script — see `TunHandle`'s `Drop` impl — so this
/// only needs to take it out of the slot, once, or the user would get a
/// second admin-password prompt for the same disconnect.
fn teardown_routing(data: &State<AppData>) {
    let _ = data.tun.lock().unwrap().take();
    let _ = crate::proxy::disable_system_proxy();
}

#[tauri::command]
pub fn get_state(data: State<AppData>) -> AppState {
    data.state.lock().unwrap().clone()
}

fn measure_latency_ms(endpoint: &str) -> Option<u32> {
    let addr = endpoint.to_socket_addrs().ok()?.next()?;
    let start = Instant::now();
    TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    Some(start.elapsed().as_millis() as u32)
}

#[tauri::command]
pub fn toggle_connection(app: AppHandle, data: State<AppData>) -> Result<AppState, String> {
    let currently_active = data.state.lock().unwrap().state.is_active();

    if currently_active {
        teardown_routing(&data);
        // Dropping the handle kills the xray/hysteria process (its Drop
        // also runs disable_system_proxy() again as a harmless safety net).
        *data.connection.lock().unwrap() = None;
        let mut app = data.state.lock().unwrap();
        app.state = TunnelState::Idle;
        app.routing_mode = None;
        app.session.uptime_secs = 0;
        app.session.rx_mb = 0.0;
        app.session.tx_mb = 0.0;
        app.session.latency_ms = 0;
        return Ok(app.clone());
    }

    let profile = {
        let app = data.state.lock().unwrap();
        app.profiles
            .iter()
            .find(|p| p.id == app.active_profile_id)
            .cloned()
    };
    let Some(profile) = profile else {
        return Err("Сначала выберите профиль на вкладке «Профили»".to_string());
    };
    let Some(uri) = profile.uri.clone() else {
        return Err("У профиля нет сохранённой ссылки — переимпортируйте его".to_string());
    };

    let params = import::parse_connect_params(&uri)?;
    let host = match &params {
        ConnectParams::Vless(p) => p.host.clone(),
        ConnectParams::Hysteria2(p) => p.host.clone(),
    };
    let active = match &params {
        ConnectParams::Vless(p) => engine::start_vless(&app, p)?,
        ConnectParams::Hysteria2(p) => engine::start_hysteria2(&app, p)?,
    };

    // The core is up and listening on 127.0.0.1:SOCKS_PORT, but nothing
    // routes through it until something tells the OS to actually use it.
    // TUN (full-system capture, one admin/root prompt) is the real thing;
    // if it's unavailable or the prompt is declined, fall back to a plain
    // system SOCKS proxy, which covers most apps but not all traffic.
    let routing_mode = match tun::up(engine::SOCKS_PORT, &host) {
        Ok(handle) => {
            *data.tun.lock().unwrap() = Some(handle);
            RoutingMode::Tun
        }
        Err(tun_err) => {
            if let Err(proxy_err) = crate::proxy::enable_system_proxy(engine::SOCKS_PORT) {
                // `active` drops here, killing the just-spawned core.
                return Err(format!(
                    "TUN недоступен ({tun_err}), а системный прокси тоже не удалось настроить ({proxy_err})"
                ));
            }
            RoutingMode::SystemProxy
        }
    };

    *data.connection.lock().unwrap() = Some(active);

    let endpoint = {
        let mut app = data.state.lock().unwrap();
        app.state = TunnelState::Connected;
        app.routing_mode = Some(routing_mode);
        app.session.uptime_secs = 0;
        app.session.rx_mb = 0.0;
        app.session.tx_mb = 0.0;
        app.session.endpoint.clone()
    };
    // Measured without holding the lock — it's a real (blocking) network
    // call and shouldn't stall other commands reading state meanwhile.
    let latency = measure_latency_ms(&endpoint).unwrap_or(0);
    let mut app = data.state.lock().unwrap();
    app.session.latency_ms = latency;
    Ok(app.clone())
}

#[tauri::command]
pub fn tick_session(data: State<AppData>) -> AppState {
    let core_died = {
        let mut conn = data.connection.lock().unwrap();
        let died = if let Some(active) = conn.as_mut() {
            !active.is_alive()
        } else {
            false
        };
        if died {
            *conn = None;
        }
        died
    };

    if core_died {
        teardown_routing(&data);
    }

    // Real traffic counters, straight from tun2socks's own REST API — only
    // available in TUN mode, where it's actually seeing all the traffic.
    let tun_traffic = data.tun.lock().unwrap().as_ref().map(|h| h.traffic_bytes());

    // macOS (and to a lesser extent other OSes) can silently drop our
    // manually-added routes on a network change — the core stays alive and
    // "Connected" would otherwise keep showing even though traffic is
    // quietly going out unprotected again. This is a real, no-privilege
    // check of whether the OS is actually still routing through the
    // tunnel, not just an assumption that nothing crashed. A watchdog
    // (spawned alongside tun2socks, see tun::up) is usually already
    // re-adding the routes in the background, so a brief "Reconnecting"
    // blip that heals itself a few ticks later is the expected, honest
    // outcome — not an error.
    let tun_mode_active = {
        let app = data.state.lock().unwrap();
        !core_died && app.state.is_active() && app.routing_mode == Some(RoutingMode::Tun)
    };
    let tun_unhealthy = tun_mode_active && !tun::is_healthy();

    let mut app = data.state.lock().unwrap();
    if core_died {
        app.state = TunnelState::Error;
        app.routing_mode = None;
    } else if app.state.is_active() {
        app.session.uptime_secs += 1;
        if let Some((up, down)) = tun_traffic {
            app.session.tx_mb = up as f64 / 1_000_000.0;
            app.session.rx_mb = down as f64 / 1_000_000.0;
        }
        if tun_mode_active {
            app.state = if tun_unhealthy {
                TunnelState::Reconnecting
            } else {
                TunnelState::Connected
            };
        }
    }
    app.clone()
}

#[tauri::command]
pub fn refresh_session(data: State<AppData>) -> AppState {
    let (active, endpoint) = {
        let app = data.state.lock().unwrap();
        (app.state.is_active(), app.session.endpoint.clone())
    };
    if active {
        let latency = measure_latency_ms(&endpoint).unwrap_or(0);
        data.state.lock().unwrap().session.latency_ms = latency;
    }
    data.state.lock().unwrap().clone()
}

#[tauri::command]
pub fn select_profile(id: String, data: State<AppData>) -> AppState {
    let mut app = data.state.lock().unwrap();
    if let Some(profile) = app.profiles.iter().find(|p| p.id == id).cloned() {
        app.active_profile_id = profile.id.clone();
        app.session.endpoint = profile.endpoint.clone();
    }
    app.clone()
}

#[tauri::command]
pub fn import_profile(uri: String, data: State<AppData>) -> Result<AppState, String> {
    let profile = import::parse_uri(&uri, pseudo_random(6))?;
    let mut app = data.state.lock().unwrap();
    app.profiles.push(profile);
    Ok(app.clone())
}

#[tauri::command]
pub fn remove_profile(id: String, data: State<AppData>) -> AppState {
    let mut app = data.state.lock().unwrap();
    app.profiles.retain(|p| p.id != id);
    if app.active_profile_id == id {
        if let Some(first) = app.profiles.first().cloned() {
            app.active_profile_id = first.id.clone();
            app.session.endpoint = first.endpoint.clone();
        } else {
            app.active_profile_id.clear();
        }
    }
    app.clone()
}

#[tauri::command]
pub fn update_setting(key: String, value: bool, data: State<AppData>) -> AppState {
    let mut app = data.state.lock().unwrap();
    match key.as_str() {
        "auto_connect" => app.settings.auto_connect = value,
        "start_with_system" => app.settings.start_with_system = value,
        "notifications" => app.settings.notifications = value,
        _ => {}
    }
    app.clone()
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
    let id = format!("group-{}", pseudo_random(7));
    app.groups.push(Group {
        id,
        name: final_name,
    });
    app.clone()
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
    app.clone()
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
    app.clone()
}

#[tauri::command]
pub fn move_profile_to_group(profile_id: String, group_id: String, data: State<AppData>) -> AppState {
    let mut app = data.state.lock().unwrap();
    let target = if group_id == UNGROUPED_ID || app.groups.iter().any(|g| g.id == group_id) {
        group_id
    } else {
        UNGROUPED_ID.to_string()
    };
    if let Some(profile) = app.profiles.iter_mut().find(|p| p.id == profile_id) {
        profile.group_id = target;
    }
    app.clone()
}

#[derive(Serialize, Clone)]
pub struct ImportSubscriptionResult {
    pub state: AppState,
    pub added: usize,
    pub group_name: String,
}

const MAX_SUBSCRIPTION_BYTES: usize = 2_000_000;

#[tauri::command]
pub async fn import_subscription(
    url: String,
    data: State<'_, AppData>,
) -> Result<ImportSubscriptionResult, String> {
    let parsed = Url::parse(url.trim()).map_err(|_| "Не удалось разобрать ссылку".to_string())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("Подписка должна начинаться с http:// или https://".to_string());
    }

    let response = reqwest::get(parsed.clone())
        .await
        .map_err(|_| "Не удалось загрузить подписку".to_string())?;

    if !response.status().is_success() {
        return Err(format!("Сервер вернул ошибку {}", response.status()));
    }

    let body = response
        .text()
        .await
        .map_err(|_| "Не удалось прочитать содержимое подписки".to_string())?;

    if body.len() > MAX_SUBSCRIPTION_BYTES {
        return Err("Файл подписки слишком большой".to_string());
    }

    let group_name = import::group_name_from_url(&parsed);
    let mut profiles = import::parse_subscription_body(&body, pseudo_random(9));
    if profiles.is_empty() {
        return Err("В файле не найдено ни одного поддерживаемого ключа".to_string());
    }

    let mut app = data.state.lock().unwrap();
    let group_id = if let Some(existing) = app.groups.iter().find(|g| g.name == group_name) {
        existing.id.clone()
    } else {
        let id = format!("group-{}", pseudo_random(8));
        app.groups.push(Group {
            id: id.clone(),
            name: group_name.clone(),
        });
        id
    };

    for profile in profiles.iter_mut() {
        profile.group_id = group_id.clone();
    }
    let added = profiles.len();
    app.profiles.extend(profiles);

    Ok(ImportSubscriptionResult {
        state: app.clone(),
        added,
        group_name,
    })
}
