//! K-92 desktop backend. Hosts the engine in-process until the Windows Service lands (M7).

mod commands;
mod config;
mod state;

use std::sync::Arc;

use tauri::Manager;

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("K92_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app.path().app_config_dir()?;
            let state = Arc::new(state::AppState::new(data_dir));
            state::start_scheduler(app.handle().clone(), state.clone());
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::inspect_destination,
            commands::configure,
            commands::update_settings,
            commands::update_sources,
            commands::start_backup,
            commands::cancel_job,
            commands::list_snapshots,
            commands::list_dir,
            commands::search_snapshot,
            commands::file_versions,
            commands::plan_restore,
            commands::run_restore,
            commands::start_verify,
            commands::get_storage,
            commands::get_activity,
        ])
        .run(tauri::generate_context!())
        .expect("error while running K-92");
}
