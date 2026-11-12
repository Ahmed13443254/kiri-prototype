// Permission pipeline: six modes, six-step evaluation, always-allow rules.
//
// Evaluation order (verified recon): hooks -> deny rules -> ask rules ->
// mode -> allow rules -> callback. A hook `allow` does not override a deny
// rule; deny is checked before allow and before the mode.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    Default,
    DontAsk,
    AcceptEdits,
    BypassPermissions,
    Plan,
    Auto,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "default" => Mode::Default,
            "dontAsk" => Mode::DontAsk,
            "acceptEdits" => Mode::AcceptEdits,
            "bypassPermissions" => Mode::BypassPermissions,
            "plan" => Mode::Plan,
            "auto" => Mode::Auto,
            other => bail!("unknown permission mode: {other}"),
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Default => "default",
            Mode::DontAsk => "dontAsk",
            Mode::AcceptEdits => "acceptEdits",
            Mode::BypassPermissions => "bypassPermissions",
            Mode::Plan => "plan",
            Mode::Auto => "auto",
        }
    }

    pub fn all() -> &'static [&'static str] {
        &[
            "default",
            "dontAsk",
            "acceptEdits",
            "bypassPermissions",
            "plan",
            "auto",
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
    Ask,
    AllowSession,
    AllowAlways,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Read,
    Write,
    Bash,
    Web,
    Agent,
    Mcp,
}

impl Risk {
    pub fn as_str(&self) -> &'static str {
        match self {
            Risk::Read => "read",
            Risk::Write => "write",
            Risk::Bash => "bash",
            Risk::Web => "web",
            Risk::Agent => "agent",
            Risk::Mcp => "mcp",
        }
    }

    pub fn of_tool(name: &str) -> Risk {
        match name {
            "Read" | "Glob" | "Grep" | "NotebookRead" => Risk::Read,
            "Write" | "Edit" | "NotebookEdit" => Risk::Write,
            "Bash" | "BashOutput" => Risk::Bash,
            "WebFetch" | "WebSearch" => Risk::Web,
            "Agent" | "Task" => Risk::Agent,
            _ if name.starts_with("mcp__") => Risk::Mcp,
            _ => Risk::Read,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleType {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub rule_type: RuleType,
    pub tool: String,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default = "default_scope")]
    pub scope: String,
    #[serde(default = "default_origin")]
    pub origin: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_scope() -> String {
    "local".into()
}
fn default_origin() -> String {
    "user".into()
}
fn default_true() -> bool {
    true
}

impl Rule {
    pub fn from_json(v: &Value) -> Option<Rule> {
        serde_json::from_value(v.clone()).ok()
    }

    pub fn matches(&self, tool: &str, input: &Value) -> bool {
        if !self.enabled {
            return false;
        }
        if !tool_matches(&self.tool, tool) {
            return false;
        }
        match &self.pattern {
            None => true,
            Some(p) if p.trim().is_empty() || p == "*" => true,
            Some(p) => glob_match(p, &rule_text_for(tool, input)),
        }
    }
}

fn rule_text_for(tool: &str, input: &Value) -> String {
    let field = match tool {
        "Bash" => "command",
        "Read" | "Write" | "Edit" | "NotebookEdit" | "NotebookRead" => "file_path",
        "Glob" => "pattern",
        "Grep" => "pattern",
        "WebFetch" => "url",
        _ => return input.to_string(),
    };
    input
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Tool namespacing: `mcp__server__tool`, plain tool names, and `*`.
pub fn tool_matches(rule_tool: &str, tool: &str) -> bool {
    if rule_tool == "*" || rule_tool == tool {
        return true;
    }
    // `mcp__server` matches `mcp__server__tool`.
    if rule_tool.starts_with("mcp__") && tool.starts_with(rule_tool) {
        return tool[rule_tool.len()..].starts_with("__");
    }
    // `mcp__server__*` is the explicit wildcard spelling.
    if let Some(prefix) = rule_tool.strip_suffix("__*") {
        return tool == prefix || tool.starts_with(&format!("{prefix}__"));
    }
    false
}

/// Minimal glob: `*` any run, `?` one char. Deliberately not a regex - a user
/// pattern must never be able to match more than its own spelling.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let mut pi = 0;
    let mut ti = 0;
    let mut star: Option<(usize, usize)> = None;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

pub struct Verdict {
    pub decision: Decision,
    pub reason: String,
    pub rule: Option<Rule>,
}

/// Six-step evaluation. `hook` is step 1's answer (None = no hook opinion).
/// Deny rules outrank everything, including a hook allow.
///
/// `in_workspace` says whether a read-risk call's target resolves inside the
/// session cwd or one of its add_dirs; reads outside those roots must ask.
pub fn evaluate(
    mode: Mode,
    risk: Risk,
    tool: &str,
    input: &Value,
    rules: &[Rule],
    hook: Option<Decision>,
    in_workspace: bool,
) -> Verdict {
    // 1. hooks
    match hook {
        Some(Decision::Deny) => {
            return Verdict {
                decision: Decision::Deny,
                reason: "hook".into(),
                rule: None,
            }
        }
        Some(Decision::Allow) => {
            // A hook allow does not bypass a deny rule; step 2 still runs.
            if let Some(r) = rules
                .iter()
                .find(|r| matches!(r.rule_type, RuleType::Deny) && r.matches(tool, input))
            {
                return Verdict {
                    decision: Decision::Deny,
                    reason: "deny_rule".into(),
                    rule: Some(r.clone()),
                };
            }
            return Verdict {
                decision: Decision::Allow,
                reason: "hook".into(),
                rule: None,
            };
        }
        _ => {}
    }

    // 2. deny rules
    if let Some(r) = rules
        .iter()
        .find(|r| matches!(r.rule_type, RuleType::Deny) && r.matches(tool, input))
    {
        return Verdict {
            decision: Decision::Deny,
            reason: "deny_rule".into(),
            rule: Some(r.clone()),
        };
    }

    // 3. ask rules (they escalate even in bypassPermissions)
    if let Some(r) = rules
        .iter()
        .find(|r| matches!(r.rule_type, RuleType::Ask) && r.matches(tool, input))
    {
        return Verdict {
            decision: Decision::Ask,
            reason: "ask_rule".into(),
            rule: Some(r.clone()),
        };
    }

    // 4. mode
    match mode {
        Mode::BypassPermissions => {
            return Verdict {
                decision: Decision::Allow,
                reason: "mode:bypassPermissions".into(),
                rule: None,
            }
        }
        Mode::DontAsk => {
            // dontAsk never prompts, but safe reads inside the workspace (and
            // read-only shell commands) still run; everything else is denied.
            let safe_read = matches!(risk, Risk::Read) && in_workspace;
            let safe_bash = matches!(risk, Risk::Bash)
                && input
                    .get("command")
                    .and_then(Value::as_str)
                    .map(bash_is_read_only)
                    .unwrap_or(false);
            if safe_read || safe_bash {
                return Verdict {
                    decision: Decision::Allow,
                    reason: "mode:dontAsk".into(),
                    rule: None,
                };
            }
            return Verdict {
                decision: Decision::Deny,
                reason: "mode:dontAsk".into(),
                rule: None,
            };
        }
        Mode::AcceptEdits if matches!(risk, Risk::Write) => {
            return Verdict {
                decision: Decision::Allow,
                reason: "mode:acceptEdits".into(),
                rule: None,
            }
        }
        Mode::Plan if matches!(risk, Risk::Read) && in_workspace => {
            return Verdict {
                decision: Decision::Allow,
                reason: "mode:plan".into(),
                rule: None,
            }
        }
        _ => {}
    }

    // 5. allow rules
    if let Some(r) = rules
        .iter()
        .find(|r| matches!(r.rule_type, RuleType::Allow) && r.matches(tool, input))
    {
        return Verdict {
            decision: Decision::Allow,
            reason: "allow_rule".into(),
            rule: Some(r.clone()),
        };
    }

    // Read-only tools inside the workspace (cwd + add_dirs) never prompt in
    // `default`; a read aimed outside those roots falls through to the
    // callback so the user decides.
    if matches!(risk, Risk::Read) && in_workspace && matches!(mode, Mode::Default | Mode::Auto) {
        return Verdict {
            decision: Decision::Allow,
            reason: "read_only".into(),
            rule: None,
        };
    }

    // A Bash call whose command only reads does not prompt either. The check is
    // on the command text, not the tool name, so it stays a whitelist: the
    // moment a redirect, pipe, substitution or mutating verb appears it falls
    // through to the callback and the user is asked.
    if matches!(risk, Risk::Bash) && matches!(mode, Mode::Default | Mode::Auto) {
        if let Some(command) = input.get("command").and_then(Value::as_str) {
            if bash_is_read_only(command) {
                return Verdict {
                    decision: Decision::Allow,
                    reason: "read_only_bash".into(),
                    rule: None,
                };
            }
        }
    }

    // 6. callback
    Verdict {
        decision: Decision::Ask,
        reason: "callback".into(),
        rule: None,
    }
}

/// Rule derived from an always-allow decision. The engine never decides where
/// it lands; the GUI sends the destination.
pub fn rule_from_suggestion(tool: &str, input: &Value, destination: &str) -> Rule {
    let pattern = match tool {
        // `npm test --watch` -> `npm test*`: the click says "this command and
        // the arguments I will type next it", not "this exact line".
        "Bash" => input
            .get("command")
            .and_then(Value::as_str)
            .map(first_word_spec)
            .map(|prefix| format!("{prefix}*")),
        "Read" | "Write" | "Edit" | "NotebookEdit" => input
            .get("file_path")
            .and_then(Value::as_str)
            .map(|p| Path::new(p).to_string_lossy().to_string()),
        _ => None,
    };
    Rule {
        rule_type: RuleType::Allow,
        tool: tool.to_string(),
        pattern,
        scope: scope_for_destination(destination),
        origin: "always_allow".into(),
        enabled: true,
    }
}

impl Rule {
    /// The `permissions.allow` spelling of this rule, e.g. `Bash(npm test:*)`.
    ///
    /// `pattern` is held internally as a glob (`*` = any run) because that is
    /// what `matches` runs. The settings file spells a *trailing* wildcard with
    /// the `:*` label, so the two forms have to be converted at the boundary and
    /// nowhere else.
    ///
    /// The conversion is a bijection for every pattern `parse_rule_string` can
    /// produce:
    ///
    /// | internal        | settings         |
    /// | --------------- | ---------------- |
    /// | `*` (match all) | `Bash`           |
    /// | `npm test`      | `Bash(npm test)` |
    /// | `npm test*`     | `Bash(npm test:*)` |
    /// | `dir/**`        | `Write(dir/**)`  |
    ///
    /// Only a single trailing `*` gets the label. `dir/**` keeps its literal
    /// spelling because a `/` before the star is not a command boundary, and
    /// labelling it would make the next parse strip the label and return
    /// `dir/**` with one star too few.
    pub fn to_settings_string(&self) -> String {
        let Some(p) = self.pattern.as_deref() else {
            return self.tool.clone();
        };
        let p = p.trim();
        if p.is_empty() || p == "*" {
            return self.tool.clone();
        }
        match p.strip_suffix('*') {
            Some(prefix) if !prefix.ends_with('*') && !prefix.is_empty() => {
                format!("{}({}:*)", self.tool, prefix)
            }
            _ => format!("{}({})", self.tool, p),
        }
    }
}

fn scope_for_destination(d: &str) -> String {
    match d {
        "userSettings" | "global" => "user".into(),
        "projectSettings" => "project".into(),
        "localSettings" => "local".into(),
        _ => "session".into(),
    }
}

/// `npm test -- --runInBand` -> `npm test` (the command prefix, not args).
fn first_word_spec(cmd: &str) -> String {
    let head = cmd
        .split(['|', ';', '&', '\n'])
        .next()
        .unwrap_or(cmd)
        .trim();
    let words: Vec<&str> = head.split_whitespace().collect();
    if words.len() <= 1 {
        head.to_string()
    } else {
        format!("{} {}", words[0], words[1])
    }
}

/// Suggestions the GUI renders as chips. Each is a real rule body, not copy.
pub fn suggestions(tool: &str, input: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    if tool == "Bash" {
        if let Some(cmd) = input.get("command").and_then(Value::as_str) {
            let w = first_word_spec(cmd);
            out.push(json!({
                "rule": format!("Bash({w}:*)"),
                "label": format!("السماح بأوامر {w} دائمًا"),
                "destination": "localSettings"
            }));
        }
    }
    if matches!(tool, "Write" | "Edit" | "NotebookEdit" | "Read") {
        if let Some(p) = input.get("file_path").and_then(Value::as_str) {
            if let Some(dir) = Path::new(p).parent() {
                let dir = dir.to_string_lossy();
                out.push(json!({
                    "rule": format!("{tool}({dir}/**)"),
                    "label": format!("السماح بـ {tool} داخل {dir}"),
                    "destination": "projectSettings"
                }));
            }
        }
    }
    out
}

/// Read-only shell commands do not prompt in `default` (recon: "read-only
/// bash"). Anything with a redirection, a pipe, a substitution or a mutating
/// verb does. The check is a conservative whitelist: when uncertain, answer
/// false so the call falls through to the user prompt.
pub fn bash_is_read_only(cmd: &str) -> bool {
    let c = cmd.trim();
    if c.is_empty() {
        return false;
    }
    // Shell metacharacters that can chain, redirect or substitute a second
    // command. Backticks and `$(...)`/`${...}`/`@(...)` hide execution inside
    // an otherwise innocent `echo`; newlines hide a second line entirely.
    if c
        .chars()
        .any(|ch| matches!(ch, '>' | '<' | '|' | '&' | ';' | '`' | '\n' | '\r'))
    {
        return false;
    }
    if c.contains("$(") || c.contains("${") || c.contains("@(") {
        return false;
    }
    const READ_ONLY: &[&str] = &[
        "ls", "cat", "pwd", "head", "tail", "wc", "grep", "rg", "find", "which", "echo",
        "git status", "git log", "git diff", "git show", "git branch", "node --version",
        "npm --version", "cargo --version", "rustc --version", "printenv", "stat", "file",
        "tree", "du", "df", "date", "whoami", "env",
    ];
    let Some(head) = READ_ONLY
        .iter()
        .find(|r| c == **r || c.starts_with(&format!("{r} ")))
    else {
        return false;
    };
    let args = c[head.len()..].trim();
    let tokens: Vec<&str> = args.split_whitespace().collect();

    match *head {
        // `find . -delete` / `-exec rm {} +` mutate through a "read" verb.
        "find" => {
            if tokens.iter().any(|t| {
                matches!(
                    *t,
                    "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir"
                        | "-fprint" | "-fprint0" | "-fprintf"
                )
            }) {
                return false;
            }
        }
        // `git branch -D x` deletes; `--output=` writes a file.
        "git branch" => {
            if tokens.iter().any(|t| {
                matches!(
                    *t,
                    "-d" | "-D" | "--delete" | "-m" | "-M" | "--move"
                        | "-c" | "-C" | "--copy" | "-f" | "--force"
                        | "--edit-description" | "--set-upstream"
                        | "--set-upstream-to" | "--unset-upstream"
                ) || t.starts_with("--set-upstream-to=")
                    || t.starts_with("-u")
            }) {
                return false;
            }
        }
        "git status" | "git log" | "git diff" | "git show" => {
            if tokens
                .iter()
                .any(|t| *t == "--output" || t.starts_with("--output="))
            {
                return false;
            }
        }
        // `date -s` sets the system clock.
        "date" => {
            if tokens
                .iter()
                .any(|t| *t == "-s" || *t == "--set" || t.starts_with("--set="))
            {
                return false;
            }
        }
        // `env A=1` prints the environment; `env A=1 rm -rf x` runs rm.
        // Only bare assignments (and plain print flags) stay read-only.
        "env" => {
            for t in &tokens {
                if t.starts_with('-') {
                    if matches!(*t, "-S" | "--split-string") || t.starts_with("--split-string=") {
                        return false;
                    }
                    continue;
                }
                if !t.contains('=') {
                    return false;
                }
            }
        }
        _ => {}
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rule(rule_type: RuleType, tool: &str, pattern: Option<&str>) -> Rule {
        Rule {
            rule_type,
            tool: tool.to_string(),
            pattern: pattern.map(str::to_string),
            scope: "project".into(),
            origin: "user".into(),
            enabled: true,
        }
    }

    fn bash(cmd: &str, mode: Mode, rules: &[Rule], hook: Option<Decision>) -> Verdict {
        evaluate(mode, Risk::Bash, "Bash", &json!({ "command": cmd }), rules, hook, true)
    }

    #[test]
    fn deny_beats_hook_allow() {
        let rules = vec![rule(RuleType::Deny, "Bash", Some("rm*"))];
        let v = bash("rm -rf /", Mode::BypassPermissions, &rules, Some(Decision::Allow));
        assert_eq!(v.decision, Decision::Deny);
        assert_eq!(v.reason, "deny_rule");
    }

    #[test]
    fn ask_beats_bypass_permissions() {
        let rules = vec![rule(RuleType::Ask, "Bash", Some("git push*"))];
        let v = bash("git push origin main", Mode::BypassPermissions, &rules, None);
        assert_eq!(v.decision, Decision::Ask);
        assert_eq!(v.reason, "ask_rule");
    }

    #[test]
    fn read_only_bash_does_not_prompt() {
        for cmd in ["ls", "cat src/main.tsx", "git status", "git log --oneline"] {
            let v = bash(cmd, Mode::Default, &[], None);
            assert_eq!(v.decision, Decision::Allow, "{cmd}: {}", v.reason);
        }
    }

    #[test]
    fn mutating_bash_prompts() {
        for cmd in [
            "rm -rf build",
            "npm install",
            "echo x > f",
            "ls | grep y",
            "git commit -m x",
        ] {
            let v = bash(cmd, Mode::Default, &[], None);
            assert_eq!(v.decision, Decision::Ask, "{cmd}: {}", v.reason);
        }
    }

    /// Substitutions, destructive flags and command-running `env` forms must
    /// never ride the read-only whitelist into an auto-allow.
    #[test]
    fn disguised_mutations_prompt() {
        for cmd in [
            "echo $(rm -rf build)",
            "echo `rm -rf build`",
            "echo ${HOME}",
            "cat @(Get-Process)",
            "find . -delete",
            "find . -exec rm {} +",
            "find . -execdir shred {} +",
            "git branch -D feature",
            "git branch --delete feature",
            "git diff --output=patch.txt",
            "env rm -rf build",
            "env -i rm -rf build",
            "env A=1 rm -rf build",
            "date -s 2020-01-01",
            "ls\nrm -rf build",
            "cat foo < bar",
        ] {
            let v = bash(cmd, Mode::Default, &[], None);
            assert_eq!(v.decision, Decision::Ask, "{cmd:?}: {}", v.reason);
        }
        // The honest read-only forms still pass.
        for cmd in ["env", "env A=1 B=2", "git branch --list", "find . -name x", "date"] {
            assert!(bash_is_read_only(cmd), "{cmd:?} should stay read-only");
        }
    }

    #[test]
    fn deny_beats_read_only_bash() {
        let rules = vec![rule(RuleType::Deny, "Bash", Some("ls*"))];
        let v = bash("ls", Mode::Default, &rules, None);
        assert_eq!(v.decision, Decision::Deny);
    }

    /// Reads outside cwd/add_dirs must ask; inside they auto-allow.
    #[test]
    fn read_auto_allow_is_bounded_by_the_workspace() {
        let inside = evaluate(
            Mode::Default, Risk::Read, "Read",
            &json!({"file_path": "src/main.rs"}), &[], None, true,
        );
        assert_eq!(inside.decision, Decision::Allow);
        let outside = evaluate(
            Mode::Default, Risk::Read, "Read",
            &json!({"file_path": "../../secrets.txt"}), &[], None, false,
        );
        assert_eq!(outside.decision, Decision::Ask);
        assert_eq!(outside.reason, "callback");
    }

    /// dontAsk never prompts: safe reads run, everything else is denied.
    #[test]
    fn dont_ask_allows_safe_reads_and_denies_the_rest() {
        let read = evaluate(
            Mode::DontAsk, Risk::Read, "Read",
            &json!({"file_path": "a.txt"}), &[], None, true,
        );
        assert_eq!(read.decision, Decision::Allow);
        let outside = evaluate(
            Mode::DontAsk, Risk::Read, "Read",
            &json!({"file_path": "/etc/passwd"}), &[], None, false,
        );
        assert_eq!(outside.decision, Decision::Deny);
        let write = evaluate(
            Mode::DontAsk, Risk::Write, "Write",
            &json!({"file_path": "a.txt"}), &[], None, true,
        );
        assert_eq!(write.decision, Decision::Deny);
        let ro_bash = bash("git status", Mode::DontAsk, &[], None);
        assert_eq!(ro_bash.decision, Decision::Allow);
        let mut_bash = bash("npm install", Mode::DontAsk, &[], None);
        assert_eq!(mut_bash.decision, Decision::Deny);
    }

    #[test]
    fn rule_round_trips_through_settings_string() {
        let r = rule(RuleType::Allow, "Bash", Some("npm test*"));
        assert_eq!(r.to_settings_string(), "Bash(npm test:*)");

        let bare = rule(RuleType::Allow, "Read", None);
        assert_eq!(bare.to_settings_string(), "Read");

        // The inverse lives where settings are parsed; the round trip itself is
        // covered by the settings tests.
    }
}
