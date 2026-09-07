mod commands;
mod engine;
mod import;
mod proxy;
mod state;
mod tun;

use commands::{
    create_group, delete_group, get_state, import_profile, import_subscription,
    move_profile_to_group, refresh_session, remove_profile, rename_group, select_profile,
    tick_session, toggle_connection, update_setting, AppData,
};
use state::AppState;
use std::sync::Mutex;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .manage(AppData {
            state: Mutex::new(AppState::default()),
            connection: Mutex::new(None),
            tun: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            toggle_connection,
            tick_session,
            refresh_session,
            select_profile,
            import_profile,
            import_subscription,
            remove_profile,
            update_setting,
            create_group,
            rename_group,
            delete_group,
            move_profile_to_group,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| {
        // Quitting (Cmd+Q, window close, etc.) must not leave the elevated
        // tun2socks process and its routes running invisibly in the
        // background — the user quit the app, they didn't ask for the VPN
        // to keep tunneling their traffic with no UI left to stop it. A raw
        // process exit skips Rust destructors entirely, so this can't rely
        // on `Drop` alone: block the exit, tear down for real, then exit.
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            let data = app_handle.state::<AppData>();
            let has_connection =
                data.connection.lock().unwrap().is_some() || data.tun.lock().unwrap().is_some();
            if has_connection {
                api.prevent_exit();
                let app_handle = app_handle.clone();
                std::thread::spawn(move || {
                    let data = app_handle.state::<AppData>();
                    // Dropping the TunHandle/ActiveConnection runs their
                    // real teardown (see tun.rs / engine.rs Drop impls).
                    let _ = data.tun.lock().unwrap().take();
                    let _ = data.connection.lock().unwrap().take();
                    let _ = proxy::disable_system_proxy();
                    app_handle.exit(0);
                });
            }
        }
    });
}
