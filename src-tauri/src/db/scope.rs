// The scope every statement must carry.
//
// architecture.md: "it takes an explicit Scope (project id + capability) and
// refuses any query whose rows fall outside it". Capability is the *kind* of
// access; project_id is the tenant boundary.

use anyhow::Result;
use rusqlite::Connection;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    Project,
    Transcript,
    Decisions,
    Durability,
    Extensions,
    Background,
    Accounting,
    Write,
    Admin,
}

#[derive(Debug, Clone)]
pub struct Scope {
    pub project_id: String,
    pub caps: Vec<Capability>,
}

#[derive(Debug, thiserror::Error)]
pub enum ScopeError {
    #[error("scope has no capability for this operation")]
    MissingCapability,
    #[error("project {project_id} is not registered")]
    UnknownProject { project_id: String },
    #[error("row falls outside scope: {0}")]
    OutOfScope(String),
    #[error("write attempted on a read-only scope")]
    ReadOnly,
}

impl Scope {
    pub fn new(project_id: impl Into<String>, caps: Vec<Capability>) -> Self {
        Self {
            project_id: project_id.into(),
            caps,
        }
    }

    /// Read-only: the default a session window gets.
    pub fn read(project_id: impl Into<String>) -> Self {
        Self::new(
            project_id,
            vec![
                Capability::Project,
                Capability::Transcript,
                Capability::Decisions,
                Capability::Durability,
                Capability::Extensions,
                Capability::Background,
                Capability::Accounting,
            ],
        )
    }

    pub fn write_all(s: Self) -> Self {
        let mut s = s;
        s.caps.push(Capability::Write);
        s
    }

    pub fn admin(project_id: impl Into<String>) -> Self {
        Self::new(
            project_id,
            vec![
                Capability::Project,
                Capability::Transcript,
                Capability::Decisions,
                Capability::Durability,
                Capability::Extensions,
                Capability::Background,
                Capability::Accounting,
                Capability::Write,
                Capability::Admin,
            ],
        )
    }

    /// Migrations and `app_meta` only. Never tenant rows.
    pub fn root() -> Self {
        Self::admin("")
    }

    pub fn can(&self, cap: Capability) -> bool {
        self.caps.contains(&cap) || self.caps.contains(&Capability::Admin)
    }

    pub fn require(&self, cap: Capability) -> Result<(), ScopeError> {
        if self.can(cap) {
            Ok(())
        } else {
            Err(ScopeError::MissingCapability)
        }
    }

    pub fn require_write(&self) -> Result<(), ScopeError> {
        if self.can(Capability::Write) {
            Ok(())
        } else {
            Err(ScopeError::ReadOnly)
        }
    }

    /// Statements bind this, never a literal handed in by the caller.
    pub fn project_id(&self) -> Option<&str> {
        if self.project_id.is_empty() {
            None
        } else {
            Some(&self.project_id)
        }
    }

    /// The tenant boundary is real only if the row exists.
    pub fn assert_registered(&self, conn: &Connection) -> Result<()> {
        let Some(id) = self.project_id() else {
            return Ok(());
        };
        let n: i64 = conn.query_row(
            "select count(*) from projects where id = ?1",
            [id],
            |r| r.get(0),
        )?;
        if n == 0 {
            return Err(ScopeError::UnknownProject {
                project_id: id.to_string(),
            }
            .into());
        }
        Ok(())
    }
}

/// Fails closed: a row from another project is an error, not a skipped row.
pub fn assert_row(scope: &Scope, row_project: &str) -> Result<(), ScopeError> {
    match scope.project_id() {
        Some(p) if p == row_project => Ok(()),
        None => Ok(()),
        Some(_) => Err(ScopeError::OutOfScope(row_project.to_string())),
    }
}
