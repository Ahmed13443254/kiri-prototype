// Session settings: load settings files, merge scopes.
//
// Precedence (lowest to highest): managed, explicit --settings, user,
// user-local, project, project-local.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

use crate::permissions::{Rule, RuleType};
use crate::session_state::SessionConfig;

#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub merged: Map<String, Value>,
    pub sources: Vec<(String, PathBuf)>,
}

pub fn user_settings_path() -> PathBuf {
    super::home().join("settings.json")
}

pub fn user_local_settings_path() -> PathBuf {
    super::home().join("settings.local.json")
}

pub fn project_settings_path(cwd: &Path) -> PathBuf {
    cwd.join(".kiri").join("settings.json")
}

pub fn project_local_settings_path(cwd: &Path) -> PathBuf {
    cwd.join(".kiri").join("settings.local.json")
}

pub fn managed_settings_path() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\ProgramData\kiri\managed-settings.json")
    } else if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/kiri/managed-settings.json")
    } else {
        PathBuf::from("/etc/kiri/managed-settings.json")
    }
}

fn read_json(p: &Path) -> Option<Value> {
    let s = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&s).ok()
}

/// Where an always-allow click writes. The three destinations the permission
/// prompt offers map onto three distinct files, so a project-wide choice and
/// a this-machine-only choice never collapse into one:
///   projectSettings -> <cwd>/.kiri/settings.json        (shared with the team)
///   localSettings   -> <cwd>/.kiri/settings.local.json  (this machine only)
///   userSettings    -> ~/.kiri/settings.json            (every project)
/// `managed` is not offered from the GUI because the user cannot write it.
pub fn destination_path(dest: &str, cwd: &Path) -> Option<PathBuf> {
    match dest {
        "projectSettings" => Some(project_settings_path(cwd)),
        "localSettings" => Some(project_local_settings_path(cwd)),
        "userSettings" => Some(user_settings_path()),
        _ => None,
    }
}

/// Persist one rule into the `permissions.allow` list of its destination file.
///
/// This is the difference between a choice and a promise: `push_rule` only
/// moves the current process, so before this existed the user answered the same
/// prompt after every restart oil. The write is a read-modify-write of one JSON
/// file, not a database transaction, because the file is the engine's own
/// settings surface and must stay hand-editable.
///
/// A rule already present is a no-op, so a double-click cannot duplicate it.
pub fn persist_rule(dest: &str, cwd: &Path, rule: &Rule) -> Result<PathBuf, String> {
    let path = destination_path(dest, cwd)
        .ok_or_else(|| format!("unknown rule destination: {dest}"))?;

    let mut file: Value = match read_json(&path) {
        Some(v) => v,
        None => json!({}),
    };
    if !file.is_object() {
        file = json!({});
    }
    // A settings file that is not an object is destroyed here rather than
    // silently kept beside our rules; `permissions` must be the only owner of
    // the key it writes.
    let obj = file.as_object_mut().expect("checked above");
    let perms = obj
        .entry("permissions".to_string())
        .or_insert_with(|| json!({}));
    if !perms.is_object() {
        *perms = json!({});
    }
    let perms = perms.as_object_mut().expect("checked above");
    let allow = perms
        .entry("allow".to_string())
        .or_insert_with(|| json!([]));
    if !allow.is_array() {
        *allow = json!([]);
    }

    let spelling = rule.to_settings_string();
    let list = allow.as_array_mut().expect("checked above");
    if list.iter().any(|v| v.as_str() == Some(spelling.as_str())) {
        return Ok(path);
    }
    list.push(Value::String(spelling));

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("settings dir {}: {e}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

/// Deep-merge `over` into `base` (objects merge, everything else replaces).
fn merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                merge(b.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (b, o) => *b = o.clone(),
    }
}

impl Settings {
    pub fn load(cfg: &SessionConfig) -> Self {
        let mut merged = Map::new();
        let mut sources = Vec::new();

        fn add_layer(
            merged: &mut Map<String, Value>,
            sources: &mut Vec<(String, PathBuf)>,
            name: &str,
            path: PathBuf,
        ) {
            let Some(v) = read_json(&path) else { return };
            let mut cur = Value::Object(std::mem::take(merged));
            merge(&mut cur, &v);
            *merged = cur.as_object().cloned().unwrap_or_default();
            sources.push((name.to_string(), path));
        }

        add_layer(&mut merged, &mut sources, "managed", managed_settings_path());

        for p in cfg.settings_paths.clone() {
            add_layer(&mut merged, &mut sources, "explicit", p);
        }

        add_layer(&mut merged, &mut sources, "user", user_settings_path());
        add_layer(
            &mut merged,
            &mut sources,
            "user_local",
            user_local_settings_path(),
        );
        add_layer(
            &mut merged,
            &mut sources,
            "project",
            project_settings_path(&cfg.cwd),
        );
        add_layer(
            &mut merged,
            &mut sources,
            "project_local",
            project_local_settings_path(&cfg.cwd),
        );

        if let Ok(m) = std::env::var("KIRI_MODEL") {
            merged.insert("model".into(), Value::String(m));
        }
        if let Ok(m) = std::env::var("KIRI_MODEL_KEY") {
            merged.insert("apiKey".into(), Value::String(m));
        }

        Settings { merged, sources }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.merged.get(key)
    }

    pub fn str(&self, key: &str) -> Option<String> {
        self.merged
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        self.merged.get(key).and_then(Value::as_bool)
    }

    pub fn u64(&self, key: &str) -> Option<u64> {
        self.merged.get(key).and_then(Value::as_u64)
    }

    pub fn i64(&self, key: &str) -> Option<i64> {
        self.merged.get(key).and_then(Value::as_i64)
    }

    pub fn array(&self, key: &str) -> Vec<Value> {
        self.merged
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    pub fn effective_model(&self, fallback: &str) -> String {
        self.str("model").unwrap_or_else(|| fallback.to_string())
    }

    pub fn effective_effort(&self, fallback: &str) -> String {
        self.str("effort").unwrap_or_else(|| fallback.to_string())
    }

    /// Effective model endpoint. Environment wins over the settings file,
    /// matching the host's env injection (the key vault arrives as
    /// ANTHROPIC_API_KEY; a probe or supervisor sets KIRI_BASE_URL).
    pub fn base_url(&self) -> String {
        std::env::var("KIRI_BASE_URL")
            .or_else(|_| std::env::var("ANTHROPIC_BASE_URL"))
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.str("baseURL"))
            .unwrap_or_else(|| crate::api::DEFAULT_BASE.to_string())
    }

    /// Effective model key. Never logged, never snapshotted: only the
    /// `key_present` boolean leaves the engine.
    pub fn api_key(&self) -> String {
        std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            // `merged["apiKey"]` already carries the KIRI_MODEL_KEY env
            // override applied in `load`, so env still outranks the file.
            .or_else(|| self.str("apiKey"))
            .unwrap_or_default()
    }

    /// Permission rules from settings. The pipeline itself decides which kind
    /// outranks which; this vector is ordered by scope precedence.
    pub fn rules(&self) -> Vec<Rule> {
        let mut out = Vec::new();
        let perms = self
            .merged
            .get("permissions")
            .cloned()
            .unwrap_or_else(|| json!({}));

        for (kind, key) in [
            (RuleType::Allow, "allow"),
            (RuleType::Deny, "deny"),
            (RuleType::Ask, "ask"),
        ] {
            if let Some(list) = perms.get(key).and_then(Value::as_array) {
                for item in list {
                    if let Some(s) = item.as_str() {
                        if let Some(r) = parse_rule_string(&kind, s) {
                            out.push(r);
                        }
                    } else if let Some(mut r) = Rule::from_json(item) {
                        r.rule_type = kind.clone();
                        out.push(r);
                    }
                }
            }
        }
        out
    }

    pub fn hooks(&self) -> Vec<Value> {
        self.array("hooks")
    }

    pub fn enable_all_project_mcp(&self) -> bool {
        self.bool("enableAllProjectMcpServers").unwrap_or(false)
    }

    /// Settings snapshot reported by `initialize`, so the GUI shows what is
    /// actually in force without re-reading the disk itself.
    pub fn snapshot(&self) -> Value {
        json!({
            "sources": self
                .sources
                .iter()
                .map(|(n, p)| json!({"name": n, "path": p.to_string_lossy()}))
                .collect::<Vec<_>>(),
            "model": self.str("model"),
            "effort": self.str("effort"),
            "baseURL": self.str("baseURL"),
            "theme": self.str("theme"),
            "modelOverrides": self.get("modelOverrides"),
        })
    }
}

/// `Bash(npm test:*)` / `Read(*)` / `Edit` -> a Rule. The parenthesised part is
/// the pattern; a rule without parens matches every input to that tool.
pub(crate) fn parse_rule_string(kind: &RuleType, s: &str) -> Option<Rule> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (tool, pattern) = match s.split_once('(') {
        Some((t, rest)) => {
            let rest = rest.strip_suffix(')').unwrap_or(rest).trim();
            // The settings spelling `Bash(npm test:*)` is a labelled glob: the
            // `:*` suffix means "this prefix, then anything". It is not the glob
            // itself, so the label is removed and a single `*` put in its place.
            // Substituting `:` with `*` instead would yield `npm test**`, which
            // `glob_match` reads as two wildcards and which then fails to match
            // `npm test --watch`. This is the exact inverse of
            // `Rule::to_settings_string`, and the round trip is covered by a
            // test in `permissions`.
            let pat = match rest.strip_suffix(":*") {
                Some(prefix) => format!("{prefix}*"),
                None => rest.to_string(),
            };
            (t.trim().to_string(), Some(pat))
        }
        None => (s.to_string(), None),
    };
    Some(Rule {
        rule_type: kind.clone(),
        tool,
        pattern,
        // A rule read out of a settings file is scoped to that file. The layer
        // name is filled in by the caller; `Settings::load` is only the reader.
        scope: "settings".into(),
        origin: "user".into(),
        enabled: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{Rule, RuleType};

    fn a_rule() -> Rule {
        Rule {
            rule_type: RuleType::Allow,
            tool: "Bash".into(),
            pattern: Some("npm test*".into()),
            scope: "local".into(),
            origin: "always_allow".into(),
            enabled: true,
        }
    }

    #[test]
    fn destination_maps_the_three_offered_scopes() {
        let cwd = Path::new("C:/work/proj");
        let project = destination_path("projectSettings", cwd).unwrap();
        let local = destination_path("localSettings", cwd).unwrap();
        let user = destination_path("userSettings", cwd).unwrap();
        // The three destinations are three distinct files: a project-wide
        // rule must not land in the machine-local file (or vice versa).
        assert_ne!(project, local);
        assert_ne!(project, user);
        assert_ne!(local, user);
        assert!(project.ends_with("settings.json"));
        assert!(local.ends_with("settings.local.json"));
        assert!(project.starts_with(cwd));
        assert!(local.starts_with(cwd));
        // `managed` is not offered from the GUI and must not be guessed at.
        assert!(destination_path("managedSettings", cwd).is_none());
    }

    /// An always-allow click with destination=projectSettings must persist
    /// into the shared project file, not the local one.
    #[test]
    fn persist_honors_the_destination_file() {
        let dir = std::env::temp_dir().join(format!("kiri-dest-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let rule = a_rule();
        let p = persist_rule("projectSettings", &dir, &rule).expect("write");
        assert_eq!(p, project_settings_path(&dir));
        assert!(!project_local_settings_path(&dir).exists());
        let l = persist_rule("localSettings", &dir, &rule).expect("write");
        assert_eq!(l, project_local_settings_path(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Write, read back through the same parser the engine starts with, and
    /// check that a second identical write is a no-op -- the double-click case.
    #[test]
    fn persist_then_reload_sees_the_rule_once() {
        let dir = std::env::temp_dir().join(format!("kiri-settings-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let rule = a_rule();
        let p = persist_rule("projectSettings", &dir, &rule).expect("write");
        assert!(p.exists(), "{} not written", p.display());

        let raw = std::fs::read_to_string(&p).unwrap();
        let v: Value = serde_json::from_str(&raw).expect("valid json");
        let first = v["permissions"]["allow"].as_array().unwrap().len();
        assert_eq!(first, 1);

        // A double-click must not duplicate the rule.
        persist_rule("projectSettings", &dir, &rule).expect("rewrite");
        let raw = std::fs::read_to_string(&p).unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["permissions"]["allow"].as_array().unwrap().len(), 1);
        assert_eq!(v["permissions"]["allow"][0], "Bash(npm test:*)");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The end the user cares about: click always-allow, kill the app, open it
    /// again, and the tool does not ask. This walks a fresh `SessionConfig`
    /// through load -> rules -> the written file, so a regression in the merge
    /// order or in the settings key spelling fails here, not in the app.
    #[test]
    fn a_written_rule_survives_a_reload() {
        let dir = std::env::temp_dir().join(format!("kiri-reload-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let rule = a_rule();
        let written = persist_rule("projectSettings", &dir, &rule).expect("write");
        assert!(written.exists());

        let cfg = crate::session_state::SessionConfig {
            cwd: dir.clone(),
            model: "m".into(),
            effort: "high".into(),
            mode: crate::permissions::Mode::Default,
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
            session_id: "test".into(),
            one_shot_prompt: None,
            api_key: String::new(),
            base_url: String::new(),
        };
        let reloaded = Settings::load(&cfg);
        let rules = reloaded.rules();

        assert!(
            rules.iter().any(|r| r.tool == "Bash"
                && r.pattern.as_deref() == Some("npm test*")
                && matches!(r.rule_type, RuleType::Allow)),
            "reload saw {rules:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

}
