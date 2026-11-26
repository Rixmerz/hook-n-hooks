//! PostToolUse for MCP tools: shrink large results with `updatedToolOutput`.
//!
//! Claude Code requires the replacement to keep the tool's output shape, so
//! the result is walked as JSON and only long strings are rewritten (a JSON
//! string becomes its shape summary, other text keeps its head and tail).
//! The full result goes to a local log (0600, secrets masked) whose path is
//! added to the first shortened string.

use crate::config::Config;
use crate::text::head_tail;
use serde_json::Value;
use std::io::Write;

/// Glob-ish match: `*` at the end matches any suffix.
fn matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(p) => name.starts_with(p),
        None => pattern == name,
    }
}

pub fn post_tool_use(input: &Value, cfg: &Config) -> Option<Value> {
    let tool = input.get("tool_name")?.as_str()?;
    if !tool.starts_with("mcp__") || !cfg.mcp.enabled {
        return None;
    }
    if cfg.mcp.skip.iter().any(|p| matches(p, tool)) {
        return None;
    }
    let resp = input.get("tool_response")?;
    let raw = serde_json::to_string(resp).ok()?;
    let raw_chars = raw.chars().count();
    if raw_chars <= cfg.mcp.max_chars {
        return None;
    }
    let log = save_log(tool, resp);
    let mut first = true;
    let budget = cfg.mcp.max_chars;
    let shrunk = walk(resp, budget, &mut |s: &str| {
        let mut t = shrink_text(s, cfg);
        if first {
            if let Some(p) = &log {
                t = format!("[hk] MCP result shortened; full result: {p}\n{t}");
            }
            first = false;
        }
        t
    });
    let out_chars = serde_json::to_string(&shrunk).ok()?.chars().count();
    if out_chars >= raw_chars {
        return None;
    }
    let session = input
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let cwd = input
        .get("cwd")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let _ = crate::stats::append(
        &crate::paths::stats_file(),
        &crate::stats::Record {
            ts: crate::paths::now(),
            session,
            filter: tool.chars().take(60).collect(),
            raw: raw_chars,
            out: out_chars,
            project: crate::paths::project_root(&cwd).display().to_string(),
            exit: None,
            ms: None,
            kind: "mcp".into(),
        },
    );
    Some(serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedToolOutput": shrunk,
        }
    }))
}

/// Rewrite every string longer than a share of the budget; keep the shape.
fn walk(v: &Value, budget: usize, f: &mut dyn FnMut(&str) -> String) -> Value {
    let limit = (budget / 4).max(500);
    match v {
        Value::String(s) if s.chars().count() > limit => Value::String(f(s)),
        Value::Array(a) => Value::Array(a.iter().map(|x| walk(x, budget, f)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), walk(x, budget, f)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn shrink_text(s: &str, cfg: &Config) -> String {
    let t = s.trim_start();
    if t.starts_with('{') || t.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<Value>(t) {
            let small = crate::filters::json::shrink(&v, 0);
            if let Ok(p) = serde_json::to_string_pretty(&small) {
                if p.len() < s.len() {
                    return format!("[hk] JSON shape (arrays cut to 3 items, totals kept):\n{p}");
                }
            }
        }
    }
    if let Some(t) = head_tail(
        s,
        cfg.limits.generic_max_lines,
        cfg.limits.head,
        cfg.limits.tail,
    ) {
        return t;
    }
    // One huge line: cut by characters.
    let keep = cfg.mcp.max_chars / 2;
    let total = s.chars().count();
    if total <= keep {
        return s.to_string();
    }
    let head: String = s.chars().take(keep).collect();
    format!("{head}… [hk] +{} chars", total - keep)
}

fn save_log(tool: &str, resp: &Value) -> Option<String> {
    let dir = crate::paths::logs_dir();
    crate::paths::create_private_dir(&dir).ok()?;
    let safe: String = tool
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(60)
        .collect();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let path = dir.join(format!("{nanos}-{}-mcp-{safe}.log", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&path).ok()?;
    let body = serde_json::to_string_pretty(resp).ok()?;
    for l in body.lines() {
        writeln!(f, "{}", crate::redact::line(l)).ok()?;
    }
    Some(path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shrinks_large_text_keeping_shape() {
        let dir = std::env::temp_dir().join(format!("hk-mcp-{}", std::process::id()));
        std::env::set_var("HK_DATA_DIR", &dir);
        let big: Vec<Value> = (0..2000).map(|i| json!({"id": i, "v": "x"})).collect();
        let text = serde_json::to_string(&big).unwrap();
        let input = json!({
            "tool_name": "mcp__idp__k8s",
            "tool_response": [{"type": "text", "text": text}],
        });
        let out = post_tool_use(&input, &Config::default()).unwrap();
        let o = &out["hookSpecificOutput"]["updatedToolOutput"];
        assert_eq!(o[0]["type"], "text");
        let t = o[0]["text"].as_str().unwrap();
        assert!(t.contains("full result:"), "{t}");
        assert!(t.contains("+1997 more (total 2000)"), "{t}");
        let small = json!({"tool_name": "mcp__x__y", "tool_response": "short"});
        assert!(post_tool_use(&small, &Config::default()).is_none());
        let bash = json!({"tool_name": "Bash", "tool_response": text});
        assert!(post_tool_use(&bash, &Config::default()).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
