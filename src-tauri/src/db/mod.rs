// Storage surface. `architecture.md` calls this "local storage surface (GUI
// calls its own DB, never the engine's files)". Every call here is one of those
// table rows, and none of them opens a socket.
//
// `Db` is the single process-wide handle. Its `conn` is private to the db
// module tree, so no other module can hold a `Connection`.

pub mod accounting;
pub mod background;
pub mod checkpoints;
pub mod decisions;
pub mod migrate;
pub mod projects;
pub mod scope;
pub mod history;
pub mod search;
pub mod transcript;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub use projects::Project;
pub use scope::{Capability, Scope, ScopeError};

pub struct Db {
    conn: Mutex<Connection>,
    pub path: PathBuf,
}

/// The engine's home. Both processes must agree on this path or the host tails
/// a directory nothing writes to (recon: the transcript is the source of
/// truth). The engine uses `~/.kiri`; the host resolves the same here rather
/// than inventing an app-data dir of its own.
pub fn kiri_home() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".kiri"))
        .unwrap_or_else(|| PathBuf::from(".kiri"))
}

/// The index lives beside the transcripts it derives from.
pub fn db_path() -> PathBuf {
    kiri_home().join("kiri.sqlite3")
}

pub fn projects_dir() -> PathBuf {
    kiri_home().join("projects")
}

impl Db {
    pub fn open(path: &Path) -> Result<Arc<Self>> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;

        conn.execute_batch(
            "pragma journal_mode = wal;
             pragma foreign_keys = on;
             pragma synchronous = normal;
             pragma busy_timeout = 5000;",
        )?;

        migrate::apply(&conn)?;

        Ok(Arc::new(Self {
            conn: Mutex::new(conn),
            path: path.to_path_buf(),
        }))
    }

    pub fn open_default() -> Result<Arc<Self>> {
        Self::open(&db_path())
    }

    /// Borrow the connection under a checked scope.
    pub(crate) fn read<T>(
        &self,
        scope: &Scope,
        f: impl FnOnce(&Connection) -> Result<T>,
    ) -> Result<T> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("db poisoned"))?;
        scope.assert_registered(&conn)?;
        f(&conn)
    }

    /// A write is one transaction; a refusal mid-way leaves no half-written turn.
    pub(crate) fn write<T>(
        &self,
        scope: &Scope,
        f: impl FnOnce(&rusqlite::Transaction) -> Result<T>,
    ) -> Result<T> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("db poisoned"))?;
        scope.assert_registered(&conn)?;
        scope.require_write()?;
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    pub fn meta(&self) -> Result<Value> {
        let scope = Scope::root();
        self.read(&scope, |c| {
            let mut st = c.prepare(
                "select version, applied_at from app_meta order by version desc limit 1",
            )?;
            let v: Option<(i64, String)> = st.query_row([], |r| Ok((r.get(0)?, r.get(1)?))).ok();
            Ok(match v {
                Some((ver, applied)) => json!({"version": ver, "applied_at": applied}),
                None => Value::Null,
            })
        })
    }

    /// GDPR deletion: the project row, then the transcripts on disk and the
    /// keychain entry are removed by the caller. Cascade handles the DB rows.
    pub fn delete_project(&self, scope: &Scope) -> Result<u64> {
        scope.require(Capability::Admin)?;
        let id = scope
            .project_id()
            .ok_or(ScopeError::MissingCapability)?
            .to_string();
        self.write(scope, |tx| {
            tx.execute("delete from projects where id = ?1", [&id])?;
            Ok(tx.changes())
        })
    }

    pub fn checkpoint(&self) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow::anyhow!("db poisoned"))?;
        conn.execute_batch("pragma wal_checkpoint(passive);")?;
        Ok(())
    }

    pub fn vacuum(&self) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow::anyhow!("db poisoned"))?;
        conn.execute_batch("pragma wal_checkpoint(truncate); vacuum;")?;
        Ok(())
    }

    /// Migrations run before any scope exists; this is the only unscoped path
    /// the rest of the app may use, and only for `_kiri_migrations`.
    pub fn schema_version(&self) -> Result<i64> {
        let conn = self.conn.lock().map_err(|_| anyhow::anyhow!("db poisoned"))?;
        migrate::current(&conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("kiri-{}-{}.sqlite3", name, uuid::Uuid::new_v4()))
    }

    /// Migrations 001..004 apply to an empty file and leave the two objects
    /// 004 introduced. This is the schema the store layer queries; if it is
    /// missing, `list_sessions` and the hourly rollup fail at runtime.
    #[test]
    fn migrations_apply_and_004_is_present() {
        let p = tmp_db("mig");
        let db = Db::open(&p).expect("open + migrate");
        let scope = Scope::root();
        db.read(&scope, |c| {
            let cols: Vec<String> = {
                let mut st = c.prepare("pragma table_info(turns)")?;
                let rows = st.query_map([], |r| r.get::<_, String>(1))?;
                let mut v = Vec::new();
                for row in rows {
                    v.push(row?);
                }
                v
            };
            assert!(cols.iter().any(|c| c == "prompt_text"), "turns.prompt_text missing: {cols:?}");
            let n: i64 = c.query_row(
                "select count(*) from sqlite_master where type='table' and name='usage_hourly'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(n, 1, "usage_hourly missing");
            Ok(())
        })
        .unwrap();
        std::fs::remove_file(p).ok();
    }

    /// The regression that made the session list blank: open a turn with a
    /// prompt, then list sessions. Both the title derivation and the turn
    /// count must survive, and no row may be dropped.
    #[test]
    fn list_sessions_reads_title_and_turn_count() {
        let p = tmp_db("list");
        let db = Db::open(&p).expect("open");
        let root = Scope::root();
        let project = db
            .read(&root, |c| Ok(projects::open_project(c, "C:/kiri-test", "kiri-test")?))
            .expect("project");
        let rscope = Scope::write_all(Scope::read(project.id.clone()));
        db.write(&rscope, |tx| {
            history::upsert_session(
                tx,
                &project.id,
                None,
                "s-1",
                None,
                "claude-opus-5-5",
                Some("high"),
                "default",
                "C:/kiri-test/s-1.jsonl",
                None,
                None,
            )?;
            let turn = history::open_turn(tx, "s-1", "write a note", Some("default"), Some("high"), Some("m"))?;
            history::close_turn(tx, turn, "done", 1234)?;
            Ok(())
        })
        .expect("seed");

        let rows = db
            .read(&rscope, |c| Ok(history::list_sessions(c, &project.id)?))
            .expect("list_sessions must not fail");
        assert_eq!(rows.len(), 1, "one session expected, got {rows:?}");
        assert_eq!(rows[0]["title"], "write a note");
        assert_eq!(rows[0]["turnCount"], 1);
        std::fs::remove_file(p).ok();
    }
}
