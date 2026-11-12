// Shell tools: Grep, Bash, BashOutput, TaskStop.
//
// Bash runs one process and buffers its output (a turn tool call is
// request/response, so it may not return until the process is done). With
// background=true it registers a task whose output is streamed into the GUI's
// task panel and which TaskStop can kill. Either way the task table is the only
// registry of running work; nothing here keeps an unreadable child alive.

use crate::exec::{arg_bool, arg_str, arg_u64, skip_dir};
use crate::runtime::{BgTask, Runtime};
use crate::tools::{clip_lines, ToolResult, MAX_SEARCH_RESULTS};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncReadExt;

pub async fn grep(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let Some(pattern) = arg_str(input, "pattern") else {
        return ToolResult::err("Grep: pattern is required");
    };
    let base = input
        .get("path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| rt.cwd());
    let mode = input
        .get("output_mode")
        .and_then(Value::as_str)
        .unwrap_or("files_with_matches");
    let case_insensitive = arg_bool(input, "-i").unwrap_or(false);
    let line_numbers = arg_bool(input, "-n").unwrap_or(false);
    let after = arg_u64(input, "-A").unwrap_or(0) as usize;
    let before = arg_u64(input, "-B").unwrap_or(0) as usize;

    let re = match regex::RegexBuilder::new(&pattern)
        .case_insensitive(case_insensitive)
        .build()
    {
        Ok(r) => r,
        Err(e) => return ToolResult::err(format!("Grep: bad regex: {e}")),
    };

    let mut matcher: Option<globset::GlobMatcher> = None;
    if let Some(g) = input.get("glob").and_then(Value::as_str) {
        let joined = if Path::new(g).is_absolute() {
            g.to_string()
        } else {
            base.join(g).to_string_lossy().to_string()
        };
        match globset::GlobBuilder::new(&joined)
            .literal_separator(false)
            .build()
        {
            Ok(gb) => matcher = Some(gb.compile_matcher()),
            Err(e) => return ToolResult::err(format!("Grep: bad glob: {e}")),
        }
    }

    let mut files_hit: Vec<String> = Vec::new();
    let mut content_lines: Vec<String> = Vec::new();
    let mut count_total = 0usize;

    'outer: for entry in walkdir::WalkDir::new(&base)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let n = e.file_name().to_string_lossy();
            !(e.file_type().is_dir() && skip_dir(&n))
        })
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let p = entry.path();
        if let Some(m) = &matcher {
            if !m.is_match(p) {
                continue;
            }
        }
        let Ok(text) = std::fs::read_to_string(p) else {
            continue;
        };
        if text.len() > 2 * 1024 * 1024 || text.contains('\u{0}') {
            continue;
        }
        let mut file_hit = false;
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !re.is_match(line) {
                continue;
            }
            file_hit = true;
            count_total += 1;
            if mode == "content" && content_lines.len() < MAX_SEARCH_RESULTS {
                let start = i.saturating_sub(before);
                let end = (i + after + 1).min(lines.len());
                for (j, l) in lines[start..end].iter().enumerate() {
                    let n = start + j + 1;
                    if line_numbers {
                        content_lines.push(format!("{}:{}:{}", p.display(), n, l));
                    } else {
                        content_lines.push(format!("{}:{}", p.display(), l));
                    }
                }
            }
        }
        if file_hit && mode != "count" {
            files_hit.push(p.to_string_lossy().to_string());
        }
        if content_lines.len() >= MAX_SEARCH_RESULTS || files_hit.len() >= MAX_SEARCH_RESULTS {
            break 'outer;
        }
    }

    match mode {
        "content" => {
            if content_lines.is_empty() {
                ToolResult::text("No matches.")
            } else {
                ToolResult::text(content_lines.join("\n"))
            }
        }
        "count" => ToolResult::text(format!("{count_total}")),
        _ => {
            if files_hit.is_empty() {
                ToolResult::text("No files found.")
            } else {
                ToolResult::text(files_hit.join("\n"))
            }
        }
    }
}

fn shell_command(shell: &str, command: &str) -> tokio::process::Command {
    if shell == "bash" {
        let mut c = tokio::process::Command::new("bash");
        c.arg("-lc").arg(command);
        c
    } else {
        let mut c = tokio::process::Command::new("powershell");
        c.arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-Command")
            .arg(command);
        c
    }
}

/// Resolve when the session (or this subagent's task) is cancelled, so a
/// foreground command can be killed promptly instead of running to timeout.
async fn wait_for_cancel(rt: &Arc<Runtime>) {
    loop {
        if rt.is_cancelled() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub async fn bash(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let Some(command) = arg_str(input, "command") else {
        return ToolResult::err("Bash: command is required");
    };
    let timeout_ms = arg_u64(input, "timeout").unwrap_or(120_000).min(600_000);
    let background = arg_bool(input, "background").unwrap_or(false);
    let want_shell = input
        .get("shell")
        .and_then(Value::as_str)
        .unwrap_or(if cfg!(windows) { "powershell" } else { "bash" })
        .to_string();
    let cwd = rt.cwd();

    if background {
        return background_start(rt, &command, &cwd, &want_shell).await;
    }

    let mut cmd = shell_command(&want_shell, &command);
    cmd.current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd.env("KIRI", "1");

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ToolResult::err(format!("Bash: spawn failed: {e}")),
    };

    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();

    let out_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut s) = stdout.take() {
            let _ = s.read_to_end(&mut buf).await;
        }
        buf
    });
    let err_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut s) = stderr.take() {
            let _ = s.read_to_end(&mut buf).await;
        }
        buf
    });

    let status = tokio::select! {
        st = child.wait() => match st {
            Ok(s) => s,
            Err(e) => return ToolResult::err(format!("Bash: {e}")),
        },
        _ = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return ToolResult::err(format!(
                "Bash: command exceeded {} ms and was killed. Raise timeout, or use background=true.",
                timeout_ms
            ));
        }
        // The GUI's interrupt must stop the child, not wait out its timeout.
        _ = wait_for_cancel(rt) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return ToolResult::err("Bash: interrupted");
        }
    };

    let out = out_task.await.unwrap_or_default();
    let err = err_task.await.unwrap_or_default();
    let so = String::from_utf8_lossy(&out);
    let se = String::from_utf8_lossy(&err);

    let mut body = String::new();
    if !so.trim().is_empty() {
        body.push_str(&so);
    }
    if !se.trim().is_empty() {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(&se);
    }
    if body.trim().is_empty() {
        body.push_str("(no output)");
    }
    let code = status.code().unwrap_or(-1);
    if code != 0 {
        return ToolResult {
            content: vec![json!({
                "type": "text",
                "text": format!("{}\n\n[exit code {}]", clip_lines(&body, 2000), code)
            })],
            is_error: true,
        };
    }
    ToolResult::text(clip_lines(&body, 2000))
}

async fn background_start(
    rt: &Arc<Runtime>,
    command: &str,
    cwd: &Path,
    shell: &str,
) -> ToolResult {
    let id = format!("bash-{}", uuid::Uuid::new_v4().simple());
    let mut cmd = shell_command(shell, command);
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("KIRI", "1");

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ToolResult::err(format!("Bash: spawn failed: {e}")),
    };

    let status_cell = Arc::new(Mutex::new("running".to_string()));
    let output_cell = Arc::new(Mutex::new(String::new()));
    let stop_cell = Arc::new(AtomicBool::new(false));

    rt.add_task(BgTask {
        id: id.clone(),
        kind: "bash".into(),
        subject: command.lines().next().unwrap_or(command).to_string(),
        status: status_cell.clone(),
        output: output_cell.clone(),
        stop: stop_cell.clone(),
        started_at: chrono::Utc::now(),
    });

    let rt2 = rt.clone();
    let id2 = id.clone();
    let status2 = status_cell.clone();
    let stop2 = stop_cell.clone();

    tokio::spawn(async move {
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let mut out_done = stdout.is_none();
        let mut err_done = stderr.is_none();
        let mut exited: Option<std::process::ExitStatus> = None;
        let mut exit_at: Option<std::time::Instant> = None;

        loop {
            if stop2.load(Ordering::SeqCst) && exited.is_none() {
                let _ = child.start_kill();
            }
            let mut progressed = false;
            // Chunked `read` is cancellation-safe: a timeout cannot lose a
            // partial line the way a cancelled `read_line` did. The task
            // table owns the output cell; this is the single append.
            if !out_done {
                if let Some(r) = stdout.as_mut() {
                    let mut buf = [0u8; 4096];
                    match tokio::time::timeout(Duration::from_millis(50), r.read(&mut buf)).await {
                        Ok(Ok(0)) => out_done = true,
                        Ok(Ok(n)) => {
                            progressed = true;
                            rt2.append_task_output(&id2, &String::from_utf8_lossy(&buf[..n]));
                        }
                        Ok(Err(_)) => out_done = true,
                        Err(_) => {}
                    }
                }
            }
            if !err_done {
                if let Some(r) = stderr.as_mut() {
                    let mut buf = [0u8; 4096];
                    match tokio::time::timeout(Duration::from_millis(50), r.read(&mut buf)).await {
                        Ok(Ok(0)) => err_done = true,
                        Ok(Ok(n)) => {
                            progressed = true;
                            rt2.append_task_output(&id2, &String::from_utf8_lossy(&buf[..n]));
                        }
                        Ok(Err(_)) => err_done = true,
                        Err(_) => {}
                    }
                }
            }
            if exited.is_none() {
                match child.try_wait() {
                    Ok(Some(st)) => {
                        exited = Some(st);
                        exit_at = Some(std::time::Instant::now());
                    }
                    Ok(None) => {}
                    Err(_) => {
                        if let Ok(mut g) = status2.lock() {
                            *g = "failed".to_string();
                        }
                        break;
                    }
                }
            }
            if stop2.load(Ordering::SeqCst) && exited.is_some() {
                if let Ok(mut g) = status2.lock() {
                    *g = "stopped".to_string();
                }
                break;
            }
            if let Some(st) = exited {
                // The child is gone; drain what it left in the pipes before
                // closing the task (bounded, so a grandchild holding the pipe
                // cannot pin this loop forever).
                let drained = out_done && err_done;
                let grace_over = exit_at
                    .map(|t| t.elapsed() > Duration::from_secs(2))
                    .unwrap_or(true);
                if drained || grace_over {
                    let s = if st.success() { "completed" } else { "failed" };
                    if let Ok(mut g) = status2.lock() {
                        *g = s.to_string();
                    }
                    break;
                }
            }
            if !progressed {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    });

    ToolResult::blocks(vec![
        json!({"type": "text", "text": "started_in_background"}),
        json!({"type": "text", "text": id}),
    ])
}

pub async fn bash_output(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let Some(id) = arg_str(input, "shell_id") else {
        return ToolResult::err("BashOutput: shell_id is required (Bash returned it)");
    };
    let filter = input.get("pattern").and_then(Value::as_str);
    let (text, status) = {
        let Ok(m) = rt.tasks.lock() else {
            return ToolResult::err("BashOutput: task table unavailable");
        };
        let Some(t) = m.get(id.as_str()) else {
            return ToolResult::err(format!("BashOutput: no such task: {id}"));
        };
        (
            t.output.lock().map(|g| g.clone()).unwrap_or_default(),
            t.status(),
        )
    };
    let mut out = text;
    if let Some(pat) = filter {
        match regex::Regex::new(pat) {
            Ok(re) => {
                out = out
                    .lines()
                    .filter(|l| re.is_match(l))
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            Err(e) => return ToolResult::err(format!("BashOutput: bad pattern: {e}")),
        }
    }
    ToolResult::text(format!("status: {status}\n\n{out}"))
}

pub async fn task_stop(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let Some(id) = arg_str(input, "task_id") else {
        return ToolResult::err("TaskStop: task_id is required");
    };
    if rt.stop_task(&id) {
        ToolResult::text(format!("stopping {id}"))
    } else {
        ToolResult::err(format!("TaskStop: no such task: {id}"))
    }
}

pub async fn run(rt: &Arc<Runtime>, name: &str, input: &Value) -> ToolResult {
    match name {
        "Grep" => grep(rt, input).await,
        "Bash" => bash(rt, input).await,
        "BashOutput" => bash_output(rt, input).await,
        "TaskStop" => task_stop(rt, input).await,
        other => ToolResult::err(format!("unknown_tool: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sleep_command(secs: u32) -> String {
        if cfg!(windows) {
            format!("Start-Sleep -Seconds {secs}")
        } else {
            format!("sleep {secs}")
        }
    }

    fn echo_command(marker: &str) -> String {
        if cfg!(windows) {
            format!("Write-Output {marker}")
        } else {
            format!("echo {marker}")
        }
    }

    /// The GUI's interrupt must kill the foreground child promptly instead of
    /// letting it run to its timeout.
    #[tokio::test]
    async fn foreground_bash_stops_on_interrupt() {
        let rt = crate::runtime::tests::test_runtime();
        let input = json!({"command": sleep_command(30), "timeout": 60000});
        let rt2 = rt.clone();
        let handle = tokio::spawn(async move { bash(&rt2, &input).await });
        tokio::time::sleep(Duration::from_millis(400)).await;
        rt.cancel();
        let out = tokio::time::timeout(Duration::from_secs(8), handle)
            .await
            .expect("interrupted Bash must return promptly, not run to timeout")
            .unwrap();
        assert!(out.is_error);
        let text = out.content[0]["text"].as_str().unwrap_or("");
        assert!(text.contains("interrupted"), "unexpected result: {text}");
    }

    /// Each output line is appended once (the task table owns the cell).
    #[tokio::test]
    async fn background_output_is_not_duplicated() {
        let rt = crate::runtime::tests::test_runtime();
        let res = bash(
            &rt,
            &json!({"command": echo_command("marker-xyz"), "background": true}),
        )
        .await;
        assert!(!res.is_error, "{:?}", res.content);
        let id = res.content[1]["text"].as_str().unwrap().to_string();

        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let st = rt.task_status(&id).unwrap_or_default();
            if st == "completed" || st == "failed" || std::time::Instant::now() > deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let out = rt
            .tasks
            .lock()
            .unwrap()
            .get(&id)
            .unwrap()
            .output
            .lock()
            .unwrap()
            .clone();
        assert_eq!(
            out.matches("marker-xyz").count(),
            1,
            "background output was duplicated: {out:?}"
        );
    }

    /// TaskStop's flag must actually kill a background shell.
    #[tokio::test]
    async fn task_stop_kills_background_shell() {
        let rt = crate::runtime::tests::test_runtime();
        let res = bash(
            &rt,
            &json!({"command": sleep_command(30), "background": true}),
        )
        .await;
        assert!(!res.is_error);
        let id = res.content[1]["text"].as_str().unwrap().to_string();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(rt.stop_task(&id));

        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        let mut status = String::new();
        while std::time::Instant::now() < deadline {
            status = rt.task_status(&id).unwrap_or_default();
            if status == "stopped" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(status, "stopped", "TaskStop did not stop the background shell");
    }
}
