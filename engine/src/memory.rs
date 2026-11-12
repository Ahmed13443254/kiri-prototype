// CLAUDE.md discovery: walk up from cwd, nearest file last so it wins.
// `claudeMdExcludes` and `--add-dir` are honoured.

use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::session_state::home;
use crate::session_state::Settings;

#[derive(Clone)]
pub struct MemoryFile {
    pub path: PathBuf,
    pub scope: &'static str,
    pub body: String,
}

const NAMES: &[&str] = &["CLAUDE.md", "KIRI.md", "AGENTS.md"];

pub fn discover(
    cfg: &crate::session_state::SessionConfig,
    settings: &Settings,
) -> Vec<MemoryFile> {
    let mut out = Vec::new();
    if cfg.no_memory {
        return out;
    }

    let excludes: Vec<String> = string_list(settings.get("claudeMdExcludes"));
    let is_excluded = |p: &Path| {
        let s = p.to_string_lossy();
        excludes
            .iter()
            .any(|e| crate::permissions::glob_match(e, &s))
    };

    // user memory
    for name in NAMES {
        let p = home().join(name);
        if p.is_file() && !is_excluded(&p) {
            if let Ok(body) = std::fs::read_to_string(&p) {
                out.push(MemoryFile {
                    path: p,
                    scope: "user",
                    body,
                });
            }
            break;
        }
    }

    // walk up from cwd to root; nearest is appended last so it wins.
    let mut dir = Some(cfg.cwd.as_path());
    let mut chain: Vec<PathBuf> = Vec::new();
    while let Some(d) = dir {
        chain.push(d.to_path_buf());
        dir = d.parent();
    }
    chain.reverse();

    for d in chain {
        for name in NAMES {
            let p = d.join(name);
            if p.is_file() && !is_excluded(&p) {
                if let Ok(body) = std::fs::read_to_string(&p) {
                    out.push(MemoryFile {
                        path: p,
                        scope: "project",
                        body,
                    });
                }
                break;
            }
        }
        for name in NAMES {
            let p = d.join(".kiri").join(name);
            if p.is_file() && !is_excluded(&p) {
                if let Ok(body) = std::fs::read_to_string(&p) {
                    out.push(MemoryFile {
                        path: p,
                        scope: "local",
                        body,
                    });
                }
                break;
            }
        }
    }

    // --add-dir roots contribute their own project memory
    for d in &cfg.add_dirs {
        for name in NAMES {
            let p = d.join(name);
            if p.is_file() && !is_excluded(&p) {
                if let Ok(body) = std::fs::read_to_string(&p) {
                    out.push(MemoryFile {
                        path: p,
                        scope: "add_dir",
                        body,
                    });
                }
                break;
            }
        }
    }

    out
}

fn string_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The memory block injected into the system prompt. Each file is labelled with
/// the path it came from so the model (and the GUI's context meter) never
/// confuses a user-level instruction with a project one.
pub fn render(files: &[MemoryFile]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n\n<memory>\n");
    for f in files {
        s.push_str(&format!(
            "\n<file scope=\"{}\" path=\"{}\">\n{}\n</file>\n",
            f.scope,
            f.path.display(),
            f.body.trim_end()
        ));
    }
    s.push_str("\n</memory>");
    s
}
