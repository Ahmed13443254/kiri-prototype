use clap::Parser;

/// kiri engine - an agent loop that speaks newline-delimited JSON on stdio.
#[derive(Parser, Debug, Clone)]
#[command(name = "kiri-engine", version, about)]
pub struct Cli {
    /// One-shot prompt (print mode). Without it, stream-json input only.
    #[arg(short = 'p', long = "print", value_name = "PROMPT")]
    pub prompt: Option<String>,

    /// Input framing: text | stream-json
    #[arg(long = "input-format", default_value = "text")]
    pub input_format: String,

    /// Output framing: text | stream-json
    #[arg(long = "output-format", default_value = "text")]
    pub output_format: String,

    /// Emit stream_event deltas alongside assistant messages.
    #[arg(long = "include-partial-messages", default_value_t = false)]
    pub include_partial_messages: bool,

    /// Emit protocol bookkeeping (control_request/control_response) on stdout.
    #[arg(long = "verbose", default_value_t = false)]
    pub verbose: bool,

    /// Model id for the session.
    #[arg(long = "model")]
    pub model: Option<String>,

    /// Effort: low | medium | high | xhigh | max
    #[arg(long = "effort")]
    pub effort: Option<String>,

    /// Permission mode: default | dontAsk | acceptEdits | bypassPermissions | plan | auto
    #[arg(long = "permission-mode")]
    pub permission_mode: Option<String>,

    #[arg(long = "resume")]
    pub resume: Option<String>,

    #[arg(long = "continue", default_value_t = false)]
    pub continue_session: bool,

    #[arg(long = "fork-session", default_value_t = false)]
    pub fork_session: bool,

    #[arg(long = "no-session-persistence", default_value_t = false)]
    pub no_session_persistence: bool,

    #[arg(long = "session-id")]
    pub session_id: Option<String>,

    #[arg(long = "settings")]
    pub settings: Option<String>,

    #[arg(long = "plugin-dir")]
    pub plugin_dirs: Vec<String>,

    #[arg(long = "add-dir")]
    pub add_dirs: Vec<String>,

    #[arg(long = "max-turns")]
    pub max_turns: Option<u32>,

    #[arg(long = "max-budget-usd")]
    pub max_budget_usd: Option<f64>,

    #[arg(long = "cwd")]
    pub cwd: Option<String>,

    #[arg(long = "permission-prompt-tool", default_value = "stdio")]
    pub permission_prompt_tool: String,

    #[arg(long = "debug", default_value_t = false)]
    pub debug: bool,

    /// Do not load CLAUDE.md memory files.
    #[arg(long = "no-memory", default_value_t = false)]
    pub no_memory: bool,

    /// Do not load hooks from settings.
    #[arg(long = "no-hooks", default_value_t = false)]
    pub no_hooks: bool,

    /// Restrict to these comma-separated tool names.
    #[arg(long = "allowed-tools")]
    pub allowed_tools: Option<String>,

    /// Deny these comma-separated tool names.
    #[arg(long = "disallowed-tools")]
    pub disallowed_tools: Option<String>,
}
