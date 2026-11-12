// Shared session state.
//
// Everything the agent loop, the tool executors and the control channel need to
// agree on lives here. Background work is captured state, not part of the map:
// a spawned tokio task owns the OS child and writes into the shared status and
// output cells.
//
// A subagent child shares the parent's permission queue, denial log, usage and
// cost cells (one budget, one GUI, one result envelope) but owns its history,
// cancellation flag and transcript chain.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

use crate::api::{self, Client};
use crate::hooks::HookDef;
use crate::memory::MemoryFile;
use crate::permissions::{Mode, Rule};
use crate::session_state::SessionConfig;
use crate::session_state::Settings;
use crate::skills::Skill;

#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub struct Tokens {
    pub input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub output_tokens: u64,
}

impl Tokens {
    pub fn add(&mut self, u: &api::Usage) {
        self.input_tokens += u.input_tokens;
        self.cache_creation_input_tokens += u.cache_creation_input_tokens;
        self.cache_read_input_tokens += u.cache_read_input_tokens;
        self.output_tokens += u.output_tokens;
    }

    pub fn total(&self) -> u64 {
        self.input_tokens
            + self.cache_creation_input_tokens
            + self.cache_read_input_tokens
            + self.output_tokens
    }

    pub fn to_json(&self) -> Value {
        json!({
            "input_tokens": self.input_tokens,
            "cache_creation_input_tokens": self.cache_creation_input_tokens,
            "cache_read_input_tokens": self.cache_read_input_tokens,
            "output_tokens": self.output_tokens,
        })
    }
}

/// A long-running piece of work the GUI's task panel lists, polls and stops.
/// An empty list must still render (recon F05 edge), so the panel is fed by
/// `list_tasks`, never by an "assume there is one" convention.
pub struct BgTask {
    pub id: String,
    pub kind: String,
    pub subject: String,
    pub status: Arc<Mutex<String>>,
    pub output: Arc<Mutex<String>>,
    pub stop: Arc<AtomicBool>,
    pub started_at: DateTime<Utc>,
}

impl BgTask {
    pub fn status(&self) -> String {
        self.status.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn snapshot(&self) -> Value {
        json!({
            "id": self.id,
            "kind": self.kind,
            "subject": self.subject,
            "status": self.status(),
            "output": self.output.lock().map(|g| g.clone()).unwrap_or_default(),
            "startedAt": self.started_at.to_rfc3339(),
        })
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

pub struct Runtime {
    pub cfg: SessionConfig,
    settings: Mutex<Settings>,
    api: Mutex<Client>,
    hooks: Mutex<Vec<HookDef>>,
    skills: Mutex<Vec<Skill>>,
    pub memory: Vec<MemoryFile>,
    session_id: Mutex<String>,
    pub started_at: DateTime<Utc>,

    pub tx: mpsc::UnboundedSender<Value>,

    pub cwd: Mutex<PathBuf>,
    pub mode_inner: Mutex<Mode>,
    pub rule_list: Mutex<Vec<Rule>>,
    pub todos: Mutex<Vec<Value>>,
    pub tasks: Mutex<HashMap<String, BgTask>>,
    pub task_store: Mutex<HashMap<String, Value>>,
    /// Shared with every subagent child: the GUI answers one queue, and a
    /// child's permission request must resolve through the parent's reader.
    pub pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
    pub transcript: Mutex<Option<PathBuf>>,
    /// The uuid of the last entry written to this runtime's transcript; the
    /// next entry chains onto it (`parentUuid`).
    last_uuid: Mutex<Option<String>>,

    pub cancelled: Arc<AtomicBool>,
    /// A subagent also stops when its parent is interrupted.
    parent_cancelled: Option<Arc<AtomicBool>>,
    /// The task table's stop flag for a background agent (TaskStop).
    pub stop_flag: Mutex<Option<Arc<AtomicBool>>>,
    pub queued: AtomicUsize,
    pub last_input_tokens: AtomicUsize,
    pub turns: AtomicUsize,
    /// Shared with subagent children: one session budget, one result usage.
    pub usage: Arc<Mutex<Tokens>>,
    pub cost_micros: Arc<AtomicI64>,
    pub permission_denials: Arc<Mutex<Vec<Value>>>,

    pub deferred_tools: Mutex<HashMap<String, Value>>,
    pub loaded_deferred: Mutex<Vec<String>>,
    pub extra_tools: Mutex<Vec<Value>>,

    /// Discovered MCP servers (status + tools), shared with children.
    pub mcp: Arc<Mutex<Vec<Value>>>,
    pub history: Mutex<Vec<Value>>,
    /// Prompts the GUI has sent but that no turn has started yet. It is the
    /// queue the `interrupt` receipt reports (`still_queued`) and the one
    /// `cancel_queued` drains, so the GUI never keeps a second copy.
    pub prompts: Mutex<std::collections::VecDeque<Value>>,
    pub prompt_ready: tokio::sync::Notify,
    /// Set once stdin reaches EOF; `take_prompt` then answers `None`
    /// instead of parking the worker forever.
    pub stdin_closed: AtomicBool,
    pub worktree: Mutex<Option<crate::workspaces::Worktree>>,
    pub pre_plan_mode: Mutex<Option<Mode>>,
    pub compact_requested: AtomicBool,
    pub cwd_override: Mutex<Option<PathBuf>>,
    pub allowed_tools_override: Mutex<Option<Vec<String>>>,
    pub model_override: Mutex<Option<String>>,
    pub effort_override: Mutex<Option<String>>,

    pub is_subagent: bool,
    pub parent_tool_use_id: Option<String>,
    pub agent_id: Option<String>,
    /// The AgentDef body for this subagent, appended to its system prompt.
    pub agent_prompt: Mutex<Option<String>>,
}

impl Runtime {
    pub fn new(
        cfg: SessionConfig,
        settings: Settings,
        tx: mpsc::UnboundedSender<Value>,
        hooks: Vec<HookDef>,
        skills: Vec<Skill>,
        memory: Vec<MemoryFile>,
        mode: Mode,
    ) -> Arc<Self> {
        let rules = settings.rules();
        let cwd = cfg.cwd.clone();
        let session_id = cfg.session_id.clone();
        let mut api = Client::new(&cfg.base_url, &cfg.api_key);
        api.set_events(tx.clone());
        Arc::new(Self {
            cfg,
            settings: Mutex::new(settings),
            api: Mutex::new(api),
            hooks: Mutex::new(hooks),
            skills: Mutex::new(skills),
            memory,
            session_id: Mutex::new(session_id),
            started_at: Utc::now(),
            tx,
            cwd: Mutex::new(cwd),
            mode_inner: Mutex::new(mode),
            rule_list: Mutex::new(rules),
            todos: Mutex::new(Vec::new()),
            tasks: Mutex::new(HashMap::new()),
            task_store: Mutex::new(HashMap::new()),
            pending: Arc::new(Mutex::new(HashMap::new())),
            transcript: Mutex::new(None),
            last_uuid: Mutex::new(None),
            cancelled: Arc::new(AtomicBool::new(false)),
            parent_cancelled: None,
            stop_flag: Mutex::new(None),
            queued: AtomicUsize::new(0),
            last_input_tokens: AtomicUsize::new(0),
            turns: AtomicUsize::new(0),
            usage: Arc::new(Mutex::new(Tokens::default())),
            cost_micros: Arc::new(AtomicI64::new(0)),
            permission_denials: Arc::new(Mutex::new(Vec::new())),
            deferred_tools: Mutex::new(HashMap::new()),
            loaded_deferred: Mutex::new(Vec::new()),
            extra_tools: Mutex::new(Vec::new()),
            mcp: Arc::new(Mutex::new(Vec::new())),
            history: Mutex::new(Vec::new()),
            prompts: Mutex::new(std::collections::VecDeque::new()),
            prompt_ready: tokio::sync::Notify::new(),
            stdin_closed: AtomicBool::new(false),
            worktree: Mutex::new(None),
            pre_plan_mode: Mutex::new(None),
            compact_requested: AtomicBool::new(false),
            cwd_override: Mutex::new(None),
            allowed_tools_override: Mutex::new(None),
            model_override: Mutex::new(None),
            effort_override: Mutex::new(None),
            is_subagent: false,
            parent_tool_use_id: None,
            agent_id: None,
            agent_prompt: Mutex::new(None),
        })
    }

    pub fn child(
        &self,
        cwd: PathBuf,
        agent_id: String,
        parent_tool_use_id: String,
        model: String,
    ) -> Arc<Runtime> {
        let mut cfg = self.cfg.clone();
        cfg.cwd = cwd.clone();
        cfg.model = model;
        cfg.resume = None;
        cfg.fork_session = false;
        cfg.session_id = self.session_id();
        Arc::new(Runtime {
            cfg,
            settings: Mutex::new(self.settings()),
            // The child shares the parent's endpoint and key but owns its
            // stream-cancellation flag, so stopping a background agent cannot
            // interrupt the parent's in-flight turn (and vice versa).
            api: Mutex::new(self.api_client().fork_cancel()),
            hooks: Mutex::new(self.hooks()),
            skills: Mutex::new(self.skills()),
            memory: Vec::new(),
            session_id: Mutex::new(self.session_id()),
            started_at: Utc::now(),
            tx: self.tx.clone(),
            cwd: Mutex::new(cwd),
            mode_inner: Mutex::new(self.mode()),
            rule_list: Mutex::new(self.rules()),
            todos: Mutex::new(Vec::new()),
            tasks: Mutex::new(HashMap::new()),
            task_store: Mutex::new(HashMap::new()),
            pending: self.pending.clone(),
            transcript: Mutex::new(None),
            last_uuid: Mutex::new(None),
            cancelled: Arc::new(AtomicBool::new(false)),
            parent_cancelled: Some(self.cancelled.clone()),
            stop_flag: Mutex::new(None),
            queued: AtomicUsize::new(0),
            last_input_tokens: AtomicUsize::new(0),
            turns: AtomicUsize::new(0),
            usage: self.usage.clone(),
            cost_micros: self.cost_micros.clone(),
            permission_denials: self.permission_denials.clone(),
            deferred_tools: Mutex::new(HashMap::new()),
            loaded_deferred: Mutex::new(Vec::new()),
            extra_tools: Mutex::new(Vec::new()),
            mcp: self.mcp.clone(),
            history: Mutex::new(Vec::new()),
            prompts: Mutex::new(std::collections::VecDeque::new()),
            prompt_ready: tokio::sync::Notify::new(),
            stdin_closed: AtomicBool::new(false),
            worktree: Mutex::new(None),
            pre_plan_mode: Mutex::new(None),
            compact_requested: AtomicBool::new(false),
            cwd_override: Mutex::new(None),
            allowed_tools_override: Mutex::new(None),
            model_override: Mutex::new(None),
            effort_override: Mutex::new(None),
            is_subagent: true,
            parent_tool_use_id: Some(parent_tool_use_id),
            agent_id: Some(agent_id),
            agent_prompt: Mutex::new(None),
        })
    }

    pub fn emit(&self, v: Value) {
        let _ = self.tx.send(v);
    }

    /// Lifecycle message: init | compact_boundary | informational |
    /// worker_shutting_down.
    pub fn system(&self, subtype: &str, extra: Value) {
        let mut v = json!({
            "type": "system",
            "subtype": subtype,
            "session_id": self.session_id(),
        });
        if let (Some(obj), Value::Object(e)) = (v.as_object_mut(), extra) {
            for (k, val) in e {
                obj.insert(k, val);
            }
        }
        self.emit(v);
    }

    pub fn session_id(&self) -> String {
        self.session_id.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn set_session_id(&self, id: String) {
        if let Ok(mut g) = self.session_id.lock() {
            *g = id;
        }
    }

    pub fn api_client(&self) -> Client {
        self.api
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|_| Client::new(&self.cfg.base_url, &self.cfg.api_key))
    }

    pub fn set_api(&self, c: Client) {
        if let Ok(mut g) = self.api.lock() {
            *g = c;
        }
    }

    pub fn hooks(&self) -> Vec<HookDef> {
        self.hooks.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn set_hooks(&self, h: Vec<HookDef>) {
        if let Ok(mut g) = self.hooks.lock() {
            *g = h;
        }
    }

    pub fn skills(&self) -> Vec<Skill> {
        self.skills.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn set_skills(&self, s: Vec<Skill>) {
        if let Ok(mut g) = self.skills.lock() {
            *g = s;
        }
    }

    pub fn mcp_servers(&self) -> Vec<Value> {
        self.mcp.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn set_mcp_servers(&self, s: Vec<Value>) {
        if let Ok(mut g) = self.mcp.lock() {
            *g = s;
        }
    }

    pub fn cwd(&self) -> PathBuf {
        self.cwd.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn set_cwd(&self, p: PathBuf) {
        if let Ok(mut g) = self.cwd.lock() {
            *g = p;
        }
    }

    pub fn reset_cancel(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        if self.cancelled.load(Ordering::SeqCst) {
            return true;
        }
        if let Some(p) = &self.parent_cancelled {
            if p.load(Ordering::SeqCst) {
                return true;
            }
        }
        if let Ok(g) = self.stop_flag.lock() {
            if let Some(s) = g.as_ref() {
                if s.load(Ordering::SeqCst) {
                    return true;
                }
            }
        }
        false
    }

    pub fn register_pending(&self, id: &str) -> oneshot::Receiver<Value> {
        let (tx, rx) = oneshot::channel();
        if let Ok(mut m) = self.pending.lock() {
            m.insert(id.to_string(), tx);
        }
        rx
    }

    pub fn resolve_pending(&self, id: &str, v: Value) -> bool {
        if let Ok(mut m) = self.pending.lock() {
            if let Some(tx) = m.remove(id) {
                let _ = tx.send(v);
                return true;
            }
        }
        false
    }

    /// Deny every prompt still queued (interrupt / shutdown).
    pub fn deny_pending(&self) {
        if let Ok(mut m) = self.pending.lock() {
            for (_, tx) in m.drain() {
                let _ = tx.send(json!({"behavior": "deny", "message": "interrupted"}));
            }
        }
    }

    pub fn add_usage(&self, model: &str, u: &api::Usage) {
        if let Ok(mut g) = self.usage.lock() {
            g.add(u);
        }
        if let Some(c) = api::cost_micros(model, u) {
            self.cost_micros.fetch_add(c, Ordering::SeqCst);
        }
    }

    pub fn usage_snapshot(&self) -> Tokens {
        self.usage.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn total_cost_micros(&self) -> i64 {
        self.cost_micros.load(Ordering::SeqCst)
    }

    /// Record one denied call. Every deny path (hook, rule, mode, GUI answer,
    /// timeout, subagent allowlist, budget) lands here; the cell is shared
    /// with subagent children, so their denials reach the parent's
    /// `result.permission_denials` too.
    pub fn record_denial(
        &self,
        tool_name: &str,
        tool_use_id: &str,
        input: &Value,
        reason: &str,
        source: &str,
    ) {
        let entry = json!({
            "tool_name": tool_name,
            "tool_use_id": tool_use_id,
            "tool_input": input,
            "reason": reason,
            "source": source,
            "agent_id": self.agent_id,
            "timestamp": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        });
        if let Ok(mut g) = self.permission_denials.lock() {
            g.push(entry);
        }
    }

    pub fn transcript_path(&self) -> Option<PathBuf> {
        self.transcript.lock().ok().and_then(|g| g.clone())
    }

    pub fn set_transcript(&self, p: Option<PathBuf>) {
        if let Ok(mut g) = self.transcript.lock() {
            *g = p;
        }
    }

    /// Append one entry to the transcript, stamped consistently: every line
    /// carries `uuid`, `parentUuid` (chained onto the previous line),
    /// `sessionId` and an ISO-8601 UTC `timestamp`. Values the caller already
    /// set are kept, so an entry is never re-stamped with a second identity.
    pub fn record(&self, mut entry: Value) -> Result<()> {
        let Some(p) = self.transcript_path() else {
            return Ok(());
        };
        if let Some(obj) = entry.as_object_mut() {
            let uuid = obj
                .get("uuid")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let parent = self
                .last_uuid
                .lock()
                .ok()
                .and_then(|mut g| g.replace(uuid.clone()));
            let kind = obj
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("system")
                .to_string();
            let base =
                crate::session_state::entry(&kind, &self.session_id(), &uuid, parent.as_deref());
            if let Some(b) = base.as_object() {
                for (k, v) in b {
                    obj.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
            obj.entry("parentUuid".to_string()).or_insert(Value::Null);
        }
        crate::session_state::append_transcript(&p, &entry)?;
        Ok(())
    }

    pub fn add_task(&self, t: BgTask) {
        if let Ok(mut m) = self.tasks.lock() {
            m.insert(t.id.clone(), t);
        }
    }

    pub fn task_status(&self, id: &str) -> Option<String> {
        self.tasks
            .lock()
            .ok()
            .and_then(|m| m.get(id).map(|t| t.status()))
    }

    pub fn stop_task(&self, id: &str) -> bool {
        if let Ok(m) = self.tasks.lock() {
            if let Some(t) = m.get(id) {
                t.request_stop();
                return true;
            }
        }
        false
    }

    pub fn tasks_json(&self) -> Vec<Value> {
        let mut v: Vec<Value> = self
            .tasks
            .lock()
            .map(|m| m.values().map(|t| t.snapshot()).collect())
            .unwrap_or_default();
        v.sort_by(|a, b| {
            a["startedAt"]
                .as_str()
                .unwrap_or("")
                .cmp(b["startedAt"].as_str().unwrap_or(""))
        });
        v
    }

    pub fn append_task_output(&self, id: &str, chunk: &str) {
        if let Ok(m) = self.tasks.lock() {
            if let Some(t) = m.get(id) {
                if let Ok(mut s) = t.output.lock() {
                    s.push_str(chunk);
                    if s.len() > 256 * 1024 {
                        let mut cut = s.len() - 192 * 1024;
                        while cut < s.len() && !s.is_char_boundary(cut) {
                            cut += 1;
                        }
                        let tail = s[cut..].to_string();
                        *s = tail;
                    }
                }
            }
        }
    }
}


impl Runtime {
    pub fn settings(&self) -> Settings {
        self.settings.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn set_settings(&self, s: Settings) {
        if let Ok(mut g) = self.settings.lock() {
            *g = s;
        }
    }

    pub fn setting_str(&self, key: &str) -> Option<String> {
        self.settings.lock().ok().and_then(|g| g.str(key))
    }

    pub fn setting_u64(&self, key: &str) -> Option<u64> {
        self.settings.lock().ok().and_then(|g| g.u64(key))
    }

    pub fn setting_bool(&self, key: &str) -> Option<bool> {
        self.settings.lock().ok().and_then(|g| g.bool(key))
    }

    pub fn setting_value(&self, key: &str) -> Option<Value> {
        self.settings.lock().ok().and_then(|g| g.get(key).cloned())
    }

    pub fn setting_array(&self, key: &str) -> Vec<Value> {
        self.settings.lock().ok().map(|g| g.array(key)).unwrap_or_default()
    }

    pub fn effective_model(&self) -> String {
        if let Some(m) = self.model_override.lock().ok().and_then(|g| g.clone()) {
            return m;
        }
        self.setting_str("model").unwrap_or_else(|| self.cfg.model.clone())
    }

    pub fn effective_effort(&self) -> String {
        if let Some(m) = self.effort_override.lock().ok().and_then(|g| g.clone()) {
            return m;
        }
        self.setting_str("effort")
            .unwrap_or_else(|| self.cfg.effort.clone())
    }

    pub fn set_model(&self, m: String) {
        if let Ok(mut g) = self.model_override.lock() {
            *g = Some(m);
        }
    }

    pub fn set_effort(&self, e: String) {
        if let Ok(mut g) = self.effort_override.lock() {
            *g = Some(e);
        }
    }

    /// Derive the model-facing conversation from a loaded transcript.
    ///
    /// A `compact-boundary` entry replaces everything before it: the summary
    /// stands in for the dropped messages, so replaying the pre-boundary lines
    /// beside it would re-inflate the exact context compaction removed.
    pub fn replay(&self, entries: &[Value], fork: bool) -> Result<()> {
        let mut msgs: Vec<Value> = Vec::new();
        for e in entries {
            if fork && e.get("isSidechain").and_then(Value::as_bool).unwrap_or(false) {
                continue;
            }
            let ty = e.get("type").and_then(Value::as_str).unwrap_or("");
            match ty {
                "user" | "assistant" => {
                    if let Some(m) = e.get("message") {
                        msgs.push(m.clone());
                    }
                }
                "compact-boundary" => {
                    msgs.clear();
                    if let Some(summary) = e.get("summary") {
                        msgs.push(json!({"role": "user", "content": summary}));
                    }
                }
                _ => {}
            }
        }
        if let Ok(mut h) = self.history.lock() {
            *h = msgs;
        }
        Ok(())
    }

    pub fn mode(&self) -> Mode {
        let m = self.mode_inner.lock().map(|g| *g).unwrap_or(Mode::Default);
        m
    }

    pub fn set_mode(&self, m: Mode) {
        if let Ok(mut g) = self.mode_inner.lock() {
            *g = m;
        }
    }

    pub fn rules(&self) -> Vec<Rule> {
        self.rule_list.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn set_rules(&self, r: Vec<Rule>) {
        if let Ok(mut g) = self.rule_list.lock() {
            *g = r;
        }
    }

    pub fn push_rule(&self, r: Rule) {
        if let Ok(mut g) = self.rule_list.lock() {
            g.push(r);
        }
    }

    /// Queue one GUI prompt; the turn worker wakes and drains it.
    pub fn enqueue_prompt(&self, msg: Value) {
        if let Ok(mut q) = self.prompts.lock() {
            q.push_back(msg);
            self.queued.store(q.len(), Ordering::SeqCst);
        }
        self.prompt_ready.notify_one();
    }

    /// Drop prompts that have not started a turn. Returns how many were
    /// cancelled, which is what the `interrupt` receipt reports.
    pub fn cancel_queued_prompts(&self) -> u64 {
        if let Ok(mut q) = self.prompts.lock() {
            let n = q.len() as u64;
            q.clear();
            self.queued.store(0, Ordering::SeqCst);
            n
        } else {
            0
        }
    }

    pub fn queued_prompts(&self) -> u64 {
        self.prompts
            .lock()
            .map(|q| q.len() as u64)
            .unwrap_or_else(|_| self.queued.load(Ordering::SeqCst) as u64)
    }

    /// The next queued prompt, or `None` once this runtime can never receive
    /// one again. The lock is released across the await, which is what lets
    /// `cancel_queued` win the race with a prompt that has not started a turn.
    pub async fn take_prompt(&self) -> Option<Value> {
        loop {
            if let Ok(mut q) = self.prompts.lock() {
                if let Some(v) = q.pop_front() {
                    self.queued.store(q.len(), Ordering::SeqCst);
                    return Some(v);
                }
            }
            if self.stdin_closed.load(Ordering::SeqCst) {
                return None;
            }
            self.prompt_ready.notified().await;
        }
    }
}

impl Runtime {
    /// A subagent runs this to completion: its own history, at most one
    /// report out. No transcript of its own (the parent's file records the
    /// start/end notices), no user prompts -- every permission decision is
    /// forwarded to the GUI through the shared pending queue, and every tool
    /// call goes through the same hook/permission gate as the parent's.
    pub async fn run_isolated(
        self: &Arc<Runtime>,
        prompt: &str,
        description: &str,
        agent_type: &str,
    ) -> Result<String> {
        {
            let mut h = self.history.lock().unwrap();
            h.push(json!({"role": "user", "content": [{"type": "text", "text": prompt}]}));
        }

        let system = crate::agent::build_system(self);
        let allow = self
            .allowed_tools_override
            .lock()
            .ok()
            .and_then(|g| g.clone());
        let mut steps = 0u32;
        let max_steps = self.cfg.max_turns.unwrap_or(48);

        loop {
            steps += 1;
            if steps > max_steps {
                return Ok(format!(
                    "Subagent {agent_type} stopped after {max_steps} steps without a final report."
                ));
            }
            if self.is_cancelled() {
                return Ok("Subagent interrupted.".to_string());
            }
            // One budget covers the whole session (features.csv: "Budget
            // shared across subagents"): a subagent stops when the shared
            // spend reaches the cap, and the stop is recorded as a denial.
            if let Some(cap) = self.cfg.max_budget_micros {
                if self.total_cost_micros() >= cap {
                    self.record_denial(
                        "Agent",
                        "",
                        &json!({"subagent_type": agent_type, "description": description}),
                        "max_budget_usd reached",
                        "budget",
                    );
                    return Err(anyhow::anyhow!("max_budget_usd reached"));
                }
            }

            let mut request = json!({
                "model": self.effective_model(),
                "max_tokens": 16000,
                "system": system,
                "messages": self.history.lock().unwrap().clone(),
            });
            let mut tool_defs = crate::agent::build_tools(self);
            if let Some(allow) = &allow {
                tool_defs.retain(|t| {
                    t.get("name")
                        .and_then(Value::as_str)
                        .map(|n| allow.iter().any(|a| a == n))
                        .unwrap_or(false)
                });
            }
            if !tool_defs.is_empty() {
                request["tools"] = Value::Array(tool_defs);
            }

            let no_stream = |_: crate::api::Delta| -> Result<()> { Ok(()) };
            let mut sink = no_stream;
            let api = self.api_client();
            // Watch cancellation while the stream is open: TaskStop sets the
            // task's stop flag and a parent interrupt sets the shared flag;
            // either must stop this agent promptly, not at the next step.
            let completed = tokio::select! {
                r = api.stream(&request, &mut sink) => r?,
                _ = wait_while_running(self) => {
                    api.cancel();
                    return Ok("Subagent interrupted.".to_string());
                }
            };

            // Subagent spend lands in the shared session cells, so the
            // parent's budget check and result usage/cost include it.
            self.add_usage(&completed.model, &completed.usage);

            let text: String = completed
                .content
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");

            let tool_uses: Vec<Value> = completed
                .content
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
                .cloned()
                .collect();

            if tool_uses.is_empty() {
                let _ = description;
                return Ok(text);
            }

            self.history
                .lock()
                .unwrap()
                .push(json!({"role": "assistant", "content": completed.content}));

            let mut results = Vec::new();
            for call in &tool_uses {
                if self.is_cancelled() {
                    return Ok("Subagent interrupted.".to_string());
                }
                let id = call
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let name = call
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let input = call.get("input").cloned().unwrap_or_else(|| json!({}));

                if let Some(allow) = &allow {
                    if !allow.iter().any(|a| a == &name) {
                        let reason =
                            format!("tool {name} is not in this subagent's allowlist");
                        self.record_denial(&name, &id, &input, &reason, "subagent_allowlist");
                        results.push(json!({
                            "type": "tool_result",
                            "tool_use_id": id,
                            "content": [{"type": "text", "text": format!(
                                "permission denied: {reason}")}],
                            "is_error": true,
                        }));
                        continue;
                    }
                }

                // The same gate the parent uses: PreToolUse hooks, prompt-hook
                // judgement, the six-step permission pipeline, the GUI prompt
                // through the shared queue, then PostToolUse. Attribution
                // (agent_id) is attached by `run_one`.
                let v = crate::agent::run_one(self.clone(), call.clone()).await;
                results.push(v);
            }

            self.history
                .lock()
                .unwrap()
                .push(json!({"role": "user", "content": results}));
        }
    }
}

/// Resolve once the runtime is cancelled (parent interrupt, TaskStop flag or
/// the child's own flag). Polls a cheap atomic; no wake-up plumbing needed.
async fn wait_while_running(rt: &Arc<Runtime>) {
    loop {
        if rt.is_cancelled() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn test_config() -> SessionConfig {
        SessionConfig {
            cwd: std::env::temp_dir(),
            model: "m".into(),
            effort: "high".into(),
            mode: Mode::Default,
            max_turns: None,
            max_budget_micros: None,
            add_dirs: Vec::new(),
            plugin_dirs: Vec::new(),
            settings_paths: Vec::new(),
            resume: None,
            continue_session: false,
            fork_session: false,
            persist: false,
            output_format: "stream-json".into(),
            input_format: "text".into(),
            partial_messages: false,
            verbose: false,
            debug: false,
            no_memory: false,
            no_hooks: false,
            allowed_tools: None,
            disallowed_tools: None,
            session_id: "test-session".into(),
            one_shot_prompt: None,
            api_key: String::new(),
            base_url: String::new(),
        }
    }

    pub(crate) fn test_runtime() -> Arc<Runtime> {
        let (tx, _rx) = mpsc::unbounded_channel();
        Runtime::new(
            test_config(),
            Settings::default(),
            tx,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Mode::Default,
        )
    }

    #[test]
    fn replay_truncates_at_the_last_compact_boundary() {
        let rt = test_runtime();
        let entries = vec![
            json!({"type": "user", "message": {"role": "user", "content": "old question"}}),
            json!({"type": "assistant", "message": {"role": "assistant", "content": "old answer"}}),
            json!({"type": "compact-boundary", "summary": "the brief"}),
            json!({"type": "user", "message": {"role": "user", "content": "new question"}}),
        ];
        rt.replay(&entries, false).unwrap();
        let h = rt.history.lock().unwrap().clone();
        assert_eq!(h.len(), 2, "pre-boundary messages must not be replayed: {h:?}");
        assert_eq!(h[0]["content"], "the brief");
        assert_eq!(h[1]["content"], "new question");
    }

    #[test]
    fn record_stamps_uuid_parent_timestamp_and_session() {
        let rt = test_runtime();
        let dir = std::env::temp_dir().join(format!("kiri-record-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        rt.set_transcript(Some(path.clone()));

        rt.record(json!({"type": "user", "message": {"role": "user", "content": "a"}})).unwrap();
        rt.record(json!({"type": "assistant", "message": {"role": "assistant", "content": "b"}})).unwrap();

        let lines = crate::session_state::read_transcript(&path).unwrap();
        assert_eq!(lines.len(), 2);
        for l in &lines {
            assert!(l.get("timestamp").and_then(Value::as_str).is_some());
            assert!(l.get("uuid").and_then(Value::as_str).is_some());
            assert_eq!(l["sessionId"], "test-session");
        }
        assert!(lines[0]["parentUuid"].is_null());
        assert_eq!(lines[1]["parentUuid"], lines[0]["uuid"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn child_shares_budget_denials_and_queue_but_not_cancellation() {
        let rt = test_runtime();
        let child = rt.child(
            std::env::temp_dir(),
            "agent-x".into(),
            "tu-x".into(),
            "m".into(),
        );
        // Shared cells: a denial or spend in the child is the parent's.
        child.record_denial("Bash", "t1", &json!({}), "deny_rule", "permission");
        assert_eq!(rt.permission_denials.lock().unwrap().len(), 1);
        child.cost_micros.fetch_add(5, Ordering::SeqCst);
        assert_eq!(rt.total_cost_micros(), 5);

        // The child's permission request resolves through the parent queue.
        let rx = child.register_pending("perm-1");
        assert!(rt.resolve_pending("perm-1", json!({"behavior": "allow"})));
        assert_eq!(rx.blocking_recv().unwrap()["behavior"], "allow");

        // Cancellation propagates parent -> child, never child -> parent.
        rt.cancel();
        assert!(child.is_cancelled());
        rt.reset_cancel();
        child.cancel();
        assert!(child.is_cancelled());
        assert!(!rt.is_cancelled());

        // TaskStop's flag stops the child too.
        child.reset_cancel();
        *child.stop_flag.lock().unwrap() = Some(Arc::new(AtomicBool::new(true)));
        assert!(child.is_cancelled());
        assert!(!rt.is_cancelled());
    }
}
