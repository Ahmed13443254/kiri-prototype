// Worktree isolation: git worktree add / remove, used by the Agent tool and by
// EnterWorktree / ExitWorktree.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    pub repo: PathBuf,
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("git not available")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn is_repo(dir: &Path) -> bool {
    git(dir, &["rev-parse", "--is-inside-work-tree"])
        .map(|o| o == "true")
        .unwrap_or(false)
}

/// Create a worktree next to the repository, on a fresh branch off HEAD.
pub fn create(repo: &Path, label: &str) -> Result<(Worktree, PathBuf)> {
    if !is_repo(repo) {
        bail!("not_a_git_repository: {}", repo.display());
    }
    let root = git(repo, &["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(root);
    let safe: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let branch = format!("kiri/{safe}-{}", uuid::Uuid::new_v4().simple());
    let base = crate::session_state::home().join("worktrees");
    std::fs::create_dir_all(&base)?;
    let name = branch.replace('/', "-");
    let path = base.join(name);
    // A branch name is unique per run; the path is the worktree's identity.

    git(&root, &["worktree", "add", "-b", &branch, &path.to_string_lossy(), "HEAD"])?;
    Ok((
        Worktree {
            path: path.clone(),
            branch,
            repo: root,
        },
        path,
    ))
}

pub fn remove(wt: &Worktree) -> Result<()> {
    git(&wt.repo, &["worktree", "remove", "--force", &wt.path.to_string_lossy()])?;
    let _ = git(&wt.repo, &["branch", "-D", &wt.branch]);
    Ok(())
}

pub fn diff(wt: &Worktree) -> Result<String> {
    git(&wt.path, &["add", "-A"])?;
    git(&wt.path, &["diff", "--cached", "--stat"])
}

pub fn list(repo: &Path) -> Result<Vec<String>> {
    let root = git(repo, &["rev-parse", "--show-toplevel"])?;
    let out = git(Path::new(root.trim()), &["worktree", "list"])?;
    Ok(out.lines().map(str::to_string).collect())
}
