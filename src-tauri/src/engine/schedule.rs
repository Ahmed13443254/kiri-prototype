// The host's in-process jobs (architecture.md: no queue service, no cron daemon).
//
//   wakeup.fired      every 15 s  -- claim due wakeups, write the prompt
//   cron.tick         every 30 s  -- claim due cron jobs, submit the prompt
//   transcript.watch  fs events, 1 s debounce -- tail .jsonl into the index
//   session.reap      every 30 s  -- pid gone -> interrupted (or done)
//   checkpoint.gc     hourly      -- unused blobs older than 7 days, bounded
//   usage.rollup      hourly      -- cached per-hour totals
//   db.vacuum         weekly, idle only
//
// Each claim is a single-row write, not a table scan.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use tokio::time::{interval, MissedTickBehavior};

use crate::db::{Scope, Db};
use crate::host::Host;

pub fn spawn_all(host: Arc<Host>) {
    spawn(host.clone(), "wakeup.fired", Duration::from_secs(15), |h| wakeup_fired(h));
    spawn(host.clone(), "cron.tick", Duration::from_secs(30), |h| cron_tick(h));
    spawn(host.clone(), "session.reap", Duration::from_secs(30), |h| session_reap(h));
    spawn(host.clone(), "checkpoint.gc", Duration::from_secs(3600), |h| checkpoint_gc(h));
    spawn(host.clone(), "usage.rollup", Duration::from_secs(3600), |h| usage_rollup(h));
    spawn(host, "db.vacuum", Duration::from_secs(7 * 24 * 3600), |h| db_vacuum(h));
}

fn spawn<F, Fut>(host: Arc<Host>, name: &'static str, every: Duration, job: F)
where
    F: Fn(Arc<Host>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send,
{
    let _ = name;
    tauri::async_runtime::spawn(async move {
        let mut ticker = interval(every);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        ticker.tick().await; // the first tick fires immediately; skip it
        loop {
            ticker.tick().await;
            let _ = job(host.clone()).await;
        }
    });
}

/// 15 s: claim due wakeups and write the prompt to the owning child's stdin.
/// A sleeping session (no child, dead pipe) is skipped, not resurrected.
async fn wakeup_fired(host: Arc<Host>) -> Result<()> {
    let scope = Scope::root();
    let due = host
        .db
        .write(&scope, |tx| Ok(crate::db::background::claim_due_wakeups(tx, 32)?))?;

    for (_, session_id, prompt) in due {
        if prompt.trim().is_empty() {
            continue;
        }
        let Some(child) = host.child(&session_id).await else {
            continue; // no live engine: this wakeup is not deliverable
        };
        if child.send_user_prompt(&prompt).await.is_err() {
            continue; // pipe gone: leave the row claimed, the GUI shows the gap
        }
    }
    Ok(())
}

/// 30 s: claim due cron jobs promising and submit them the same way.
async fn cron_tick(host: Arc<Host>) -> Result<()> {
    let scope = Scope::root();
    let due = host
        .db
        .read(&scope, |c| Ok(crate::db::background::claim_due_crons(c, 32)?))?;

    for job in due {
        let session_id = job["sessionId"].as_str().unwrap_or_default().to_string();
        let prompt = job["prompt"].as_str().unwrap_or_default().to_string();
        let recurring = job["recurring"].as_bool().unwrap_or(true);
        if prompt.trim().is_empty() {
            continue;
        }
        let Some(child) = host.child(&session_id).await else {
            continue;
        };
        if child.send_user_prompt(&prompt).await.is_err() {
            continue;
        }
        let scope = Scope::root();
        let _ = host.db.write(&scope, |tx| {
            crate::db::background::advance_cron(
                tx,
                job["id"].as_str().unwrap_or_default(),
                job["cron"].as_str().unwrap_or_default(),
                recurring,
            )?;
            Ok(())
        });
    }
    Ok(())
}

/// 30 s: a session whose pid is gone becomes `interrupted`, or `done` if a
/// result already arrived. Rows are never deleted.
async fn session_reap(host: Arc<Host>) -> Result<()> {
    let scope = Scope::root();

    // Read the live-pid set first, then drop the DB lock before mutating the
    // child table: a scheduler must never await while holding a transaction.
    let live: Vec<(String, i64)> = host.db.read(&scope, |conn| {
        let mut st = conn.prepare(
            "select id, pid from sessions where ended_at is null and pid is not null",
        )?;
        let rows: Vec<(String, i64)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    })?;

    let mut dead = Vec::new();
    for (id, pid) in live {
        if !process_alive(pid) {
            host.db.write(&scope, |tx| {
                tx.execute(
                    "update sessions set status = case
                        when exists (select 1 from turns t where t.session_id = sessions.id
                                     and t.status = 'done') then 'done'
                        else 'interrupted' end,
                       ended_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                     where id = ?1 and ended_at is null",
                    [&id],
                )?;
                Ok(())
            })?;
            dead.push(id);
        }
    }

    if !dead.is_empty() {
        let mut children = host.children.lock().await;
        for id in dead {
            children.remove(&id);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn process_alive(pid: i64) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn process_alive(pid: i64) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

/// Hourly: drop unreferenced blobs older than 7 days, at most 2% of the DB
/// size per run so it cannot stall the app.
async fn checkpoint_gc(host: Arc<Host>) -> Result<()> {
    let scope = Scope::root();
    host.db.write(&scope , |tx| {
        let page: i64 = tx.query_row("pragma page_size", [], |r| r.get(0))?;
        let pages: i64 = tx.query_row("pragma page_count", [], |r| r.get(0))?;
        let bytes = page.saturating_mul(pages);
        // 2% of the DB, expressed in rows: a conservative row is 512 bytes.
        let cap = ((bytes / 512) as f64 * 0.02).max(1.0) as i64;
        crate::db::checkpoints::gc_blobs(tx, 7, cap)?;
        Ok(())
    })
}

/// Hourly: rewrite the cached per-hour totals the cost panel reads.
async fn usage_rollup(host: Arc<Host>) -> Result<()> {
    let scope = Scope::root();
    host.db.write(&scope, |tx| {
        tx.execute(
            "insert into usage_hourly (hour, micros, requests)
             select substr(created_at, 1, 13) || ':00:00Z' as hour,
                    coalesce(sum(cost_micros), 0),
                    count(*)
               from usage
              group by hour
             on conflict (hour) do update set
               micros = excluded.micros,
               requests = excluded.requests",
            [],
        )?;
        Ok(())
    })
}

/// Weekly, and only when nothing else is writing.
async fn db_vacuum(host: Arc<Host>) -> Result<()> {
    let children = host.children.lock().await;
    if !children.is_empty() {
        return Ok(());
    }
    drop(children);
    host.db.vacuum()?;
    Ok(())
}

pub fn _typecheck(_h: &Arc<Host>, _d: &Arc<Db>, _v: &Value, _r: Result<()>) {}
