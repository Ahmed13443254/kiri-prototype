// One-writer stdout: all NDJSON goes through here so no two tasks can
// interleave bytes mid-line.

use serde_json::Value;
use std::io::Write;
use std::sync::Mutex;
use tokio::sync::Mutex as AsyncMutex;

static WRITER: Mutex<()> = Mutex::new(());
static WRITER_ASYNC: AsyncMutex<()> = AsyncMutex::const_new(());

pub fn emit_sync(v: &Value) -> anyhow::Result<()> {
    let _g = WRITER.lock().unwrap_or_else(|e| e.into_inner());
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer(&mut lock, v)?;
    lock.write_all(b"\n")?;
    lock.flush()?;
    Ok(())
}

pub async fn emit(v: &Value) -> anyhow::Result<()> {
    let _g = WRITER_ASYNC.lock().await;
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer(&mut lock, v)?;
    lock.write_all(b"\n")?;
    lock.flush()?;
    Ok(())
}
