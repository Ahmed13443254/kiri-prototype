// Session config and the engine's on-disk layout.
//
// The transcript is the source of truth for a conversation (recon). This
// module owns where it lives and how settings/state are located.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub mod settings;
pub use settings::Settings;

use crate::cli::Cli;
use crate::permissions::Mode;

pub static DEBUG: AtomicBool = AtomicBool::new(false);

pub fn init_logger(debug: bool) {
    DEBUG.store(debug, Ordering::Relaxed);
    if debug {
        eprintln!("[kiri] debug logging enabled");
    }
}

pub fn debug_enabled() -> bool {
    DEBUG.load(Ordering::Relaxed)
}

#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub cwd: PathBuf,
    pub model: String,
    pub effort: String,
    pub mode: Mode,
    pub max_turns: Option<u32>,
    pub max_budget_micros: Option<i64>,
    pub add_dirs: Vec<PathBuf>,
    pub plugin_dirs: Vec<PathBuf>,
    pub settings_paths: Vec<PathBuf>,
    pub resume: Option<String>,
    pub continue_session: bool,
    pub fork_session: bool,
    pub persist: bool,
    pub output_format: String,
    pub input_format: String,
    pub partial_messages: bool,
    pub verbose: bool,
    pub debug: bool,
    pub no_memory: bool,
    pub no_hooks: bool,
    pub allowed_tools: Option<Vec<String>>,
    pub disallowed_tools: Option<Vec<String>>,
    pub session_id: String,
    pub one_shot_prompt: Option<String>,
    pub api_key: String,
    pub base_url: String,
}

impl SessionConfig {
    pub fn is_stream_json(&self) -> bool {
        self.output_format == "stream-json"
    }
}

pub fn from_cli(c: Cli) -> Result<SessionConfig> {
    let cwd = match c.cwd {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir().context("cwd")?,
    };
    let cwd = cwd.canonicalize().unwrap_or(cwd);

    let mode = match c.permission_mode.as_deref() {
        Some(m) => Mode::parse(m).with_context(|| format!("unknown permission mode {m}"))?,
        None => Mode::Default,
    };

    let model = c
        .model
        .clone()
        .or_else(|| std::env::var("KIRI_MODEL").ok())
        .unwrap_or_else(|| crate::api::DEFAULT_MODEL.to_string());

    let effort = c
        .effort
        .clone()
        .or_else(|| std::env::var("KIRI_EFFORT").ok())
        .unwrap_or_else(|| "high".to_string());

    let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
    let base_url = std::env::var("KIRI_BASE_URL")
        .or_else(|_| std::env::var("ANTHROPIC_BASE_URL"))
        .unwrap_or_else(|_| crate::api::DEFAULT_BASE.to_string());

    // Resuming reuses the source transcript's own id, so the transcript on disk stays
    // one session and the GUI's `sessions` row keeps pointing at it. Forking is the
    // one path that mints a new id, because the transcript is copied, not appended.
    let resumed_id = if c.fork_session {
        None
    } else {
        match (&c.resume, c.continue_session, &c.session_id) {
            (Some(target), false, None) => resolved_session_id(target),
            (None, true, None) => most_recent_transcript(&cwd).map(|p| file_stem(&p)),
            _ => None,
        }
    };

    let session_id = resumed_id
        .or_else(|| c.session_id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let max_budget_micros = c.max_budget_usd.map(|d| (d * 1_000_000.0).round() as i64);

    let allowed_tools = c.allowed_tools.map(|s| split_list(&s));
    let disallowed_tools = c.disallowed_tools.map(|s| split_list(&s));

    Ok(SessionConfig {
        cwd,
        model,
        effort,
        mode,
        max_turns: c.max_turns,
        max_budget_micros,
        add_dirs: c.add_dirs.iter().map(PathBuf::from).collect(),
        plugin_dirs: c.plugin_dirs.iter().map(PathBuf::from).collect(),
        settings_paths: c.settings.iter().map(PathBuf::from).collect(),
        resume: c.resume,
        continue_session: c.continue_session,
        fork_session: c.fork_session,
        persist: !c.no_session_persistence,
        output_format: c.output_format,
        input_format: c.input_format,
        partial_messages: c.include_partial_messages,
        verbose: c.verbose,
        debug: c.debug,
        no_memory: c.no_memory,
        no_hooks: c.no_hooks,
        allowed_tools,
        disallowed_tools,
        session_id,
        one_shot_prompt: c.prompt,
        api_key,
        base_url,
    })
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

/// `~/.kiri/` - the engine's own durable record, never a database.
pub fn home() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".kiri"))
        .unwrap_or_else(|| PathBuf::from(".kiri"))
}

pub fn projects_dir() -> PathBuf {
    home().join("projects")
}

pub fn sessions_dir() -> PathBuf {
    home().join("sessions")
}

pub fn logs_dir() -> PathBuf {
    home().join("logs")
}

pub fn ensure_dirs() -> Result<()> {
    for d in [home(), projects_dir(), sessions_dir(), logs_dir()] {
        std::fs::create_dir_all(&d).with_context(|| format!("create {}", d.display()))?;
    }
    Ok(())
}

/// Slug a cwd the way the transcript directory is named: `/home/u/proj` ->
/// `-home-u-proj`. Windows `C:\a\b` -> `C--a-b`.
/// Slug the directory the transcript lives under.
///
/// This is a path segment, so it must be identical for every caller holding the
/// "same" directory: the engine canonicalises `cfg.cwd`, and a host that passes
/// the raw path must still land on this name. Canonicalising here makes that
/// true from either direction. On Windows canonicalize yields the verbatim form
/// (the extended-length prefix), which is why the slug begins with its dashes.
pub fn slug(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The id a resumed transcript carries: its file stem. The transcript is the
/// source of truth, so resuming the same file twice opens the same session.
fn resolved_session_id(target: &str) -> Option<String> {
    find_transcript(target).ok().map(|p| file_stem(&p))
}

fn file_stem(p: &std::path::Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

pub fn transcript_path(cwd: &Path, session_id: &str) -> PathBuf {
    projects_dir()
        .join(slug(cwd))
        .join(format!("{session_id}.jsonl"))
}

/// Locate a transcript by session id (any project) or by absolute path.
pub fn find_transcript(target: &str) -> Result<PathBuf> {
    let candidate = PathBuf::from(target);
    if candidate.is_absolute() && candidate.exists() {
        return Ok(candidate);
    }
    let projects = projects_dir();
    if let Ok(entries) = std::fs::read_dir(&projects) {
        for e in entries.flatten() {
            let p = e.path().join(format!("{target}.jsonl"));
            if p.exists() {
                return Ok(p);
            }
        }
    }
    bail!("transcript_not_found: {target}")
}

/// Most recently modified transcript in the cwd's slug directory.
pub fn most_recent_transcript(cwd: &Path) -> Option<PathBuf> {
    let dir = projects_dir().join(slug(cwd));
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(m) = e.metadata() else { continue };
        let Ok(t) = m.modified() else { continue };
        if best.as_ref().map(|(bt, _)| t > *bt).unwrap_or(true) {
            best = Some((t, p));
        }
    }
    best.map(|(_, p)| p)
}

/// A conversation entry in the transcript. `parentUuid` chains entries so a
/// fork can cut at any point.
pub fn entry(kind: &str, session_id: &str, uuid: &str, parent: Option<&str>) -> Value {
    let mut v = json!({
        "type": kind,
        "sessionId": session_id,
        "uuid": uuid,
        "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    });
    if let Some(p) = parent {
        v["parentUuid"] = Value::String(p.to_string());
    }
    v
}

/// Append one JSON line to the transcript, flushing immediately so a GUI crash
/// never loses a turn that the engine already emitted.
pub fn append_transcript(path: &Path, value: &Value) -> Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open transcript {}", path.display()))?;
    let bytes = serde_json::to_vec(value)?;
    f.write_all(&bytes)?;
    f.write_all(b"\n")?;
    f.flush()?;
    Ok(())
}

/// Read every entry of a transcript. The GUI does its own tailing; this is for
/// `--resume` and for `--fork-session`.
pub fn read_transcript(path: &Path) -> Result<Vec<Value>> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut out = Vec::new();
    for line in BufReader::new(f).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(v) => out.push(v),
            Err(e) => eprintln!("[kiri] skipping corrupt transcript line: {e}"),
        }
    }
    Ok(out)
}

/// Seed a fork's transcript with the source's entries.
///
/// Recon's contract for a fork is "new session id, same history". The model's
/// context is rebuilt from the source in memory (`replay`), but a GUI that opens
/// the fork reads its transcript file, so that file must begin with the source's
/// lines. Each line is re-stamped with the fork's session id: a transcript that
/// named two sessions would make `--resume <fork>` ambiguous. Existing target
/// lines are never overwritten.
pub fn copy_transcript(src: &Path, dst: &Path) -> Result<usize> {
    if dst == src {
        return Ok(0);
    }
    if !src.exists() {
        bail!("fork source not found: {}", src.display());
    }
    if dst.exists() && std::fs::metadata(dst)?.len() > 0 {
        return Ok(0);
    }
    let text = std::fs::read_to_string(src)?;
    let mut out = String::new();
    let mut n = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(obj) = v.as_object_mut() {
            obj.insert("sessionId".into(), Value::String(file_stem(dst)));
        }
        out.push_str(&serde_json::to_string(&v)?);
        out.push('\n');
        n += 1;
    }
    std::fs::write(dst, out)?;
    Ok(n)
}
