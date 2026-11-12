// The GUI host. It spawns the engine, owns the pipe ends, and is the only
// process allowed to touch the SQLite index.
//
// No HTTP, no socket, no listener: Tauri's IPC delivers these commands to the
// renderer, and the renderer talks to the engine only through this host.

use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use tauri::{AppHandle, Emitter};
use tokio::sync::{broadcast, Mutex};

use crate::db::Db;

pub struct Host {
    pub db: Arc<Db>,
    /// The model key lives in the OS keychain, never the DB. The host holds
    /// this handle only; no method here ever returns the secret over IPC.
    pub vault: crate::vault::KeyVault,
    pub children: Mutex<HashMap<String, Arc<crate::engine::child::EngineChild>>>,
    /// Live indexers, one per running child: they turn the pipe's lines into
    /// index rows as they arrive instead of waiting for the file tail.
    pub live: StdMutex<HashMap<String, Arc<crate::engine::ingester::LiveIndex>>>,
    pub watcher: Arc<crate::engine::watch::Watcher>,
    pub events: broadcast::Sender<crate::engine::child::EngineEvent>,
}

impl Host {
    pub fn new(db: Arc<Db>) -> Arc<Self> {
        let (events, _) = broadcast::channel(8192);
        Arc::new(Self {
            db,
            vault: crate::vault::KeyVault::new(),
            children: Mutex::new(HashMap::new()),
            live: StdMutex::new(HashMap::new()),
            watcher: Arc::new(crate::engine::watch::Watcher::new()),
            events,
        })
    }

    /// Forward every engine event to the renderer over Tauri's own IPC. This is
    /// the product's only realtime mechanism: the pipe writes, the host reads.
    pub fn start_pump(self: &Arc<Self>, app: AppHandle) {
        let mut rx = self.events.subscribe();
        tauri::async_runtime::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        let _ = app.emit(
                            "engine://event",
                            json!({"sessionId": ev.session_id, "line": ev.line}),
                        );
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        let _ = app.emit("engine://lagged", json!({"dropped": n}));
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    pub async fn child(&self, session_id: &str) -> Option<Arc<crate::engine::child::EngineChild>> {
        self.children.lock().await.get(session_id).cloned()
    }

    pub async fn insert_child(
        &self,
        session_id: &str,
        child: Arc<crate::engine::child::EngineChild>,
    ) {
        self.children
            .lock()
            .await
            .insert(session_id.to_string(), child);
    }

    pub async fn remove_child(&self, session_id: &str) {
        self.children.lock().await.remove(session_id);
        if let Ok(mut live) = self.live.lock() {
            live.remove(session_id);
        }
    }
}
