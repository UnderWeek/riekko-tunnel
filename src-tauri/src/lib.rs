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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
