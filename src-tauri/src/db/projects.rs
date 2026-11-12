// Project and workspace rows. Every statement binds the scope's project id.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::{Capability, Scope};

#[derive(Debug, Clone)]
pub struct Project {
    pub id: String,
    pub path: String,
    pub name: String,
}

fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn row_to_project(r: &rusqlite::Row) -> rusqlite::Result<Project> {
    Ok(Project {
        id: r.get(0)?,
        path: r.get(1)?,
        name: r.get(2)?,
    })
}

/// Upsert a project by its absolute path and stamp `last_opened_at`.
pub fn open_project(conn: &Connection, path: &str, name: &str) -> Result<Project> {
    let existing: Option<Project> = conn
        .query_row(
            "select id, path, name from projects where path = ?1",
            [path],
            row_to_project,
        )
        .ok();

    let p = match existing {
        Some(p) => {
            conn.execute(
                "update projects set last_opened_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                     updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') where id = ?1",
                [&p.id],
            )?;
            p
        }
        None => {
            let p = Project {
                id: new_id(),
                path: path.to_string(),
                name: name.to_string(),
            };
            conn.execute(
                "insert into projects (id, path, name) values (?1, ?2, ?3)",
                params![p.id, p.path, p.name],
            )?;
            p
        }
    };

    // A project always has its primary workspace.
    let has_primary: i64 = conn.query_row(
        "select count(*) from workspaces where project_id = ?1 and kind = 'primary'",
        [&p.id],
        |r| r.get(0),
    )?;
    if has_primary == 0 {
        conn.execute(
            "insert into workspaces (id, project_id, path, kind) values (?1, ?2, ?3, 'primary')",
            params![new_id(), p.id, p.path],
        )?;
    }

    Ok(p)
}

pub fn primary_workspace(conn: &Connection, project_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "select id from workspaces where project_id = ?1 and kind = 'primary' and removed_at is null",
            [project_id],
            |r| r.get(0),
        )
        .ok())
}

pub fn list_projects(conn: &Connection) -> Result<Vec<Value>> {
    let mut st = conn.prepare(
        "select id, path, name, last_opened_at,
                (select count(*) from sessions s where s.project_id = projects.id)
         from projects order by last_opened_at desc",
    )?;
    let rows = st.query_map([], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "path": r.get::<_, String>(1)?,
            "name": r.get::<_, String>(2)?,
            "lastOpenedAt": r.get::<_, String>(3)?,
            "sessionCount": r.get::<_, i64>(4)?,
        }))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The project a path belongs to, for the app's "open folder" entry point.
pub fn project_for_path(conn: &Connection, path: &str) -> Result<Option<Project>> {
    Ok(conn
        .query_row(
            "select id, path, name from projects where path = ?1",
            [path],
            row_to_project,
        )
        .ok())
}

pub fn scope_for(project_id: &str) -> Scope {
    Scope::write_all(Scope::read(project_id))
}

pub fn require_project(scope: &Scope) -> Result<(), super::ScopeError> {
    scope.require(Capability::Project)
}
