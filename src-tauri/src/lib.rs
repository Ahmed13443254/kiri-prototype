use std::sync::Arc;

pub mod commands;
pub mod db;
pub mod engine;
pub mod host;
pub mod vault;


use tauri::{Manager, RunEvent};

use host::Host;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let db = db::Db::open_default()?;
            let host = Host::new(db);
            host.start_pump(app.handle().clone());
            engine::schedule::spawn_all(host.clone());
            engine::watch::spawn_watch(host.clone());
            host.vault.probe();
            app.manage(host);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::open_project,
            commands::list_projects,
            commands::list_sessions,
            commands::get_session,
            commands::delete_session,
            commands::read_messages,
            commands::list_rules,
            commands::upsert_rule,
            commands::delete_rule,
            commands::spawn_session,
            commands::send_prompt,
            commands::interrupt_session,
            commands::answer_permission,
            commands::set_permission_mode,
            commands::set_model,
            commands::context_usage,
            commands::compact_session,
            commands::list_tasks,
            commands::stop_task,
            commands::search_blocks,
            commands::export_session,
            commands::write_export,
            commands::read_file,
            commands::list_settings,
            commands::set_setting,
            commands::list_memory,
            commands::vault_status,
            commands::vault_set_key,
            commands::vault_clear,
            commands::list_subagents,
            commands::cost_rollup,
            commands::list_transcripts,
            commands::list_checkpoints,
            commands::checkpoint_diff,
            commands::restore_checkpoint_file,
            commands::restore_checkpoint,
        ])
        .build(tauri::generate_context!())
        .expect("error while building kiri")
        .run(|app, event| {
            if matches!(event, RunEvent::ExitRequested { .. }) {
                let host = app.state::<Arc<Host>>().inner().clone();
                tauri::async_runtime::block_on(async move {
                    let children: Vec<_> = host.children.lock().await.values().cloned().collect();
                    for child in children {
                        let _ = child.shutdown().await;
                    }
                });
            }
        });
}
