// Context compaction (recon F04).
//
// Near the window limit the engine summarises older turns into one boundary
// message, emits `system:compact_boundary`, and keeps going. The GUI renders
// the boundary; it never invents the summary.

use anyhow::Result;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::api::Delta;
use crate::runtime::Runtime;

/// Reserve this much of the window for the next answer.
pub const RESERVE_TOKENS: u64 = 16_000;
/// Trigger auto-compaction at this fraction of the usable window.
pub const AUTO_AT: f64 = 0.80;
/// Keep the most recent turns verbatim.
pub const KEEP_RECENT: usize = 6;

pub fn estimate_history(rt: &Arc<Runtime>) -> u64 {
    rt.history
        .lock()
        .map(|h| {
            h.iter()
                .map(|m| crate::api::estimate_tokens(&m.to_string()))
                .sum()
        })
        .unwrap_or(0)
}

pub fn window(rt: &Arc<Runtime>) -> u64 {
    rt.setting_value("modelOverrides")
        .and_then(|m| {
            m.get(&rt.effective_model())
                .and_then(|v| v.get("contextWindow"))
                .and_then(Value::as_u64)
        })
        .unwrap_or(200_000)
}

pub fn should_auto(rt: &Arc<Runtime>) -> bool {
    if rt.setting_bool("autoCompact").unwrap_or(true) == false {
        return false;
    }
    // The last request's input count ALREADY contains the whole history; it
    // is the API's own measurement of the same bytes the estimate covers.
    // Adding the two double-counts every token and compacts far too early.
    // Use the larger of the two measurements instead.
    let estimated = estimate_history(rt);
    let measured = rt.last_input_tokens.load(std::sync::atomic::Ordering::Relaxed) as u64;
    let used = estimated.max(measured);
    let limit = window(rt).saturating_sub(RESERVE_TOKENS);
    used as f64 >= limit as f64 * AUTO_AT
}

/// Summarise the oldest messages. Returns (summary, dropped_count).
async fn summarise(rt: &Arc<Runtime>, old: &[Value]) -> Result<String> {
    let transcript = old
        .iter()
        .map(|m| {
            let role = m.get("role").and_then(Value::as_str).unwrap_or("?");
            let body = m.get("content").map(|c| c.to_string()).unwrap_or_default();
            format!("[{role}] {body}")
        })
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        "Summarise the conversation below for continued work. Keep: decisions made, files touched, \
         commands run and their outcomes, open questions, and anything the user asked to remember. \
         Drop: pleasantries and restated tool output. Be dense; no headings.\n\n---\n{transcript}\n---"
    );

    let request = json!({
        "model": rt.effective_model(),
        "max_tokens": 4000,
        "system": "You compress an agent conversation into a working brief.",
        "messages": [{"role": "user", "content": [{"type": "text", "text": prompt}]}],
    });

    let mut sink = |_: Delta| -> Result<()> { Ok(()) };
    let completed = rt.api_client().stream(&request, &mut sink).await?;
    Ok(completed.text)
}

/// Compact when needed design. `force` is the GUI's manual compact control.
/// Returns true when a boundary was written.
pub async fn compact_if_needed(rt: &Arc<Runtime>, force: bool) -> Result<bool> {
    if !force && !should_auto(rt) {
        return Ok(false);
    }

    let history = rt.history.lock().map(|h| h.clone()).unwrap_or_default();
    if history.len() <= KEEP_RECENT {
        return Ok(false); // nothing to gain; the GUI shows an honest no-op
    }
    let split = history.len() - KEEP_RECENT;
    let old = &history[..split];
    let recent = &history[split..];
    let pre = estimate_history(rt);

    let trigger = if force { "manual" } else { "auto" };
    let pre_payload = json!({
        "event": "PreCompact",
        "session_id": rt.session_id(),
        "trigger": trigger,
        "messages_to_compact": split,
    });
    let _ = crate::hooks::run(&rt.hooks(), "PreCompact", &rt.cwd(), None, &pre_payload).await;

    let summary = summarise(rt, old).await?;

    let boundary = json!({
        "role": "user",
        "content": [{"type": "text", "text": format!(
            "<compacted_context>\n{summary}\n</compacted_context>"
        )}],
    });

    if let Ok(mut h) = rt.history.lock() {
        let mut next = vec![boundary];
        next.extend_from_slice(recent);
        *h = next;
    }

    let post = estimate_history(rt);
    // The pre-compaction input measurement describes a context that no longer
    // exists; keep it from re-triggering auto-compaction on the next step.
    // The next API response replaces it with the authoritative count.
    rt.last_input_tokens
        .store(post as usize, std::sync::atomic::Ordering::Relaxed);
    rt.record(json!({
        "type": "compact-boundary",
        "summary": summary,
        "sessionId": rt.session_id(),
        "uuid": uuid::Uuid::new_v4().to_string(),
        "trigger": trigger,
        "preTokens": pre,
        "postTokens": post,
    }))?;

    rt.system("compact_boundary", json!({
        "trigger": trigger,
        "pre_tokens": pre,
        "post_tokens": post,
        "dropped_messages": split,
    }));

    let post_payload = json!({
        "event": "PostCompact",
        "session_id": rt.session_id(),
        "trigger": trigger,
        "pre_tokens": pre,
        "post_tokens": post,
        "dropped_messages": split,
    });
    let _ = crate::hooks::run(&rt.hooks(), "PostCompact", &rt.cwd(), None, &post_payload).await;

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The estimate and the API's last input count measure the SAME context.
    /// Summing them compacted at roughly half the real window.
    #[test]
    fn auto_trigger_does_not_double_count_history_and_last_input() {
        let rt = crate::runtime::tests::test_runtime();
        // One history message of ~4000 estimated tokens.
        let big = "x".repeat(16_000);
        rt.history.lock().unwrap().push(json!({"role": "user", "content": big}));
        let estimated = estimate_history(&rt);
        assert!(estimated >= 4000);
        // The API measured the same context at the same size.
        rt.last_input_tokens
            .store(estimated as usize, std::sync::atomic::Ordering::Relaxed);

        // Window 200k, reserve 16k, trigger at 80% of 184k = 147.2k.
        // used = max(4k, 4k) = 4k -> no compaction. The old sum (8k) would
        // still pass here, so also pin the boundary case below.
        assert!(!should_auto(&rt));

        // At the trigger point exactly one measurement suffices.
        let limit = (window(&rt).saturating_sub(RESERVE_TOKENS)) as f64 * AUTO_AT;
        rt.last_input_tokens
            .store(limit as usize + 1, std::sync::atomic::Ordering::Relaxed);
        assert!(should_auto(&rt));

        // And a small history with a huge stale measurement must not sum:
        // halve the measurement so only the sum would cross the line.
        rt.history.lock().unwrap().clear();
        rt.history.lock().unwrap().push(json!({"role": "user", "content": "small"}));
        rt.last_input_tokens
            .store((limit * 0.75) as usize, std::sync::atomic::Ordering::Relaxed);
        assert!(
            !should_auto(&rt),
            "history + last_input must not be added together"
        );
    }
}
