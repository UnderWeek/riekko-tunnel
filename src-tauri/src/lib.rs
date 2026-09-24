mod commands;
mod engine;
mod import;
mod proxy;
mod state;
mod store;
mod sys;
mod tun;

use commands::{
    create_group, delete_group, get_state, import_profile, import_subscription,
    move_profile_to_group, refresh_session, remove_profile, rename_group, select_profile,
    tick_session, toggle_connection, update_setting, AppData,
};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tauri::Manager;
use tauri_plugin_autostart::MacosLauncher;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        // Must come first. A second copy would share (and fight over) the
        // saved library, the proxy crash marker and the TUN stop file —
        // e.g. restore the proxy while the first copy still shows
        // "connected". Launching again just brings the window back.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_notification::init())
        .manage(AppData::default())
        .setup(|app| {
            commands::init(app.handle());
            Ok(())
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
        // (Should the app die without getting here, the TUN watchdog sees
        // it and cleans up on its own.)
        match event {
            tauri::RunEvent::ExitRequested { api, .. } => {
                let data = app_handle.state::<AppData>();
                let needs_cleanup = data.has_routing() || data.is_busy();
                if needs_cleanup && !data.exiting.swap(true, Ordering::SeqCst) {
                    api.prevent_exit();
                    let app_handle = app_handle.clone();
                    std::thread::spawn(move || {
                        // Let a connect in progress (e.g. waiting on the
                        // admin prompt) finish first, or it would install
                        // routing right after we tore everything down.
                        let data = app_handle.state::<AppData>();
                        let deadline = Instant::now() + Duration::from_secs(120);
                        while data.is_busy() && Instant::now() < deadline {
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        commands::shutdown_all(&app_handle);
                        app_handle.exit(0);
                    });
                }
            }
            // macOS Cmd+Q / Dock "Quit" / logout terminate the app without
            // an ExitRequested first — this is the last chance to clean up.
            tauri::RunEvent::Exit => {
                if app_handle.state::<AppData>().has_routing() {
                    commands::shutdown_all(app_handle);
                }
            }
            _ => {}
        }
    });
}
