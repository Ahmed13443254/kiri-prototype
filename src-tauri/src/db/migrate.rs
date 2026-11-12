// Numbered migrations embedded at build time so the app is self-contained.

use anyhow::{Context, Result};
use rusqlite::Connection;

const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../../migrations/001_init.sql")),
    (2, include_str!("../../migrations/002_blobs_created.sql")),
    (3, include_str!("../../migrations/003_search_index.sql")),
    (4, include_str!("../../migrations/004_turn_prompt_hourly.sql")),
];

pub fn apply(conn: &Connection) -> Result<()> {
    // app_meta is created by 001 itself; detect and apply in order.
    conn.execute_batch(
        "create table if not exists _kiri_migrations (
             version integer primary key,
             applied_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
         );",
    )?;

    let applied: Vec<i64> = {
        let mut st = conn.prepare("select version from _kiri_migrations")?;
        let rows = st.query_map([], |r| r.get(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    for (version, sql) in MIGRATIONS {
        if applied.contains(version) {
            continue;
        }
        if *version == 1 {
            // 001 owns app_meta; insert its own version row inside the batch.
            conn.execute_batch(sql)
                .with_context(|| format!("apply migration {version}"))?;
            conn.execute(
                "insert or ignore into _kiri_migrations (version) values (?1)",
                [version],
            )?;
            continue;
        }
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)
            .with_context(|| format!("apply migration {version}"))?;
        tx.execute(
            "insert into _kiri_migrations (version) values (?1)",
            [version],
        )?;
        tx.commit()?;
    }
    Ok(())
}

pub fn current(conn: &Connection) -> Result<i64> {
    let v: i64 = conn
        .query_row(
            "select coalesce(max(version), 0) from _kiri_migrations",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(v)
}
