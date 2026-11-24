//! Claude Code hook handlers. Every handler fails open: on any unexpected
//! input it prints nothing and exits 0, so a bug in hk can never block a tool
//! call. Stdout carries only the JSON object (or, for SessionStart, plain
//! context text), as the hooks protocol requires.

use crate::config::Config;
use crate::filters::{self, Cmd};
use crate::permissions::{Rules, Verdict};
use crate::shell;
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};

pub fn read_input() -> Option<Value> {
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s).ok()?;
    serde_json::from_str(&s).ok()
}

fn cwd_of(input: &Value) -> PathBuf {
    input
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Keep only characters that are safe to embed unquoted in a command line.
fn sanitize_session(s: &str) -> String {
    let s: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect();
    if s.is_empty() {
        "unknown".into()
    } else {
        s
    }
}

/// PreToolUse(Bash): rewrite `cmd` to `<hk> run --filter F --session S -- cmd`
/// when a filter owns it. Returns the JSON to print, if any.
pub fn pre_tool_use(input: &Value, hk_path: &str, rules: &Rules) -> Option<Value> {
    if input.get("tool_name")?.as_str()? != "Bash" {
        return None;
    }
    let tool_input = input.get("tool_input")?.as_object()?;
    if tool_input
        .get("run_in_background")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let command = tool_input.get("command")?.as_str()?;
    if command.contains(" run --filter ") && command.contains("hk") {
        return None; // already wrapped
    }
    let cfg = Config::load(&cwd_of(input));
    if !cfg.rewrite.enabled {
        return None;
    }
    let simple = shell::parse(command)?;
    let cmd = Cmd::from_argv(&simple.words)?;
    if filters::is_watch_mode(&cmd) {
        return None;
    }
    let name = match filters::find(&cmd, &|n| cfg.filter_enabled(n)) {
        Some(f) => f.name(),
        None if cfg.filter_enabled(filters::generic::NAME)
            && cfg.rewrite.generic_commands.contains(&cmd.program) =>
        {
            filters::generic::NAME
        }
        None => return None,
    };
    // Permission rules are evaluated against the rewritten command, so check
    // the original one against the user's own rules first (see permissions.rs).
    let mut verdict = rules.verdict(simple.rest);
    if let v @ (Verdict::Deny | Verdict::Ask) = rules.verdict(command) {
        // A deny/ask rule on the full command (with `cd`/env prefix) also counts.
        verdict = v;
    }
    // With a `cd`/env prefix, only a catch-all `Bash` rule is known to cover
    // the whole command line; narrower rules are not restated.
    if !simple.prefix.is_empty() || !simple.env.is_empty() {
        if let Verdict::Allow(rule) = &verdict {
            if rule != "Bash" && rule != "Bash(*)" {
                verdict = Verdict::Unknown;
            }
        }
    }
    let mode = input
        .get("permission_mode")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let decision = match verdict {
        Verdict::Deny | Verdict::Ask => return None,
        Verdict::Allow(rule) => Some(rule),
        Verdict::Unknown => {
            if !matches!(mode, "bypassPermissions" | "auto") && cfg.rewrite.unapproved != "rewrite"
            {
                return None;
            }
            None
        }
    };
    let session = sanitize_session(
        input
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or(""),
    );
    let rewritten = format!(
        "{}{}{} run --filter {name} --session {session} -- {}",
        simple.prefix,
        simple.env,
        shell::quote(hk_path),
        simple.rest
    );
    // `updatedInput` replaces the whole input object: echo every other field.
    let mut updated = tool_input.clone();
    updated.insert("command".into(), Value::String(rewritten));
    let mut out = json!({
        "hookEventName": "PreToolUse",
        "updatedInput": Value::Object(updated),
    });
    if let Some(rule) = decision {
        // Restates the user's own allow rule for the original command; hk
        // never allows anything the user's rules did not.
        out["permissionDecision"] = json!("allow");
        out["permissionDecisionReason"] =
            json!(format!("hk: original command allowed by your rule {rule}"));
    }
    Some(json!({ "hookSpecificOutput": out }))
}

/// PostToolUse(Edit|Write|MultiEdit): format + autofix the touched file.
pub fn post_tool_use(input: &Value) -> Option<Value> {
    let tool = input.get("tool_name")?.as_str()?;
    if !matches!(tool, "Edit" | "Write" | "MultiEdit") {
        return None;
    }
    let file = input.get("tool_input")?.get("file_path")?.as_str()?;
    let cwd = cwd_of(input);
    let cfg = Config::load(&cwd);
    if !cfg.format.enabled {
        return None;
    }
    let path = if Path::new(file).is_absolute() {
        PathBuf::from(file)
    } else {
        cwd.join(file)
    };
    let ctx = crate::format::process(&path, &cfg.limits)?;
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": ctx,
        }
    }))
}

/// SessionStart: plain-text repo map (Claude Code adds SessionStart stdout to context).
pub fn session_start(input: &Value) -> Option<String> {
    let cwd = cwd_of(input);
    let cfg = Config::load(&cwd);
    if !cfg.session_start.enabled {
        return None;
    }
    Some(crate::repomap::render(&cwd))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_decision_only_restates_user_rules() {
        let input = |mode: &str| json!({"permission_mode": mode, "tool_name": "Bash", "tool_input": {"command": "cargo test"}});
        let allow = Rules {
            allow: vec!["Bash(cargo test:*)".into()],
            ..Rules::default()
        };
        let out = pre_tool_use(&input("default"), "hk", &allow).unwrap();
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "allow");
        // No matching rule: skipped by default, rewritten without a decision in bypass mode.
        let none = Rules::default();
        assert!(pre_tool_use(&input("default"), "hk", &none).is_none());
        let out = pre_tool_use(&input("bypassPermissions"), "hk", &none).unwrap();
        assert!(out["hookSpecificOutput"]
            .get("permissionDecision")
            .is_none());
    }

    fn pre(cmd: &str) -> Option<String> {
        let input = json!({
            "session_id": "abc-123",
            "cwd": std::env::temp_dir(),
            "tool_name": "Bash",
            "tool_input": {"command": cmd, "description": "d", "timeout": 1000}
        });
        let rules = Rules {
            allow: vec!["Bash".into()],
            deny: vec!["Bash(npm install:*)".into()],
            ask: vec![],
        };
        let out = pre_tool_use(&input, "/opt/hk/hk", &rules)?;
        let ui = &out["hookSpecificOutput"]["updatedInput"];
        assert_eq!(ui["description"], "d");
        assert_eq!(ui["timeout"], 1000);
        Some(ui["command"].as_str()?.to_string())
    }

    #[test]
    fn rewrites_known_commands() {
        assert_eq!(
            pre("cargo test").as_deref(),
            Some("'/opt/hk/hk' run --filter cargo_test --session abc-123 -- cargo test")
        );
        assert_eq!(
            pre("cd web && CI=1 npx vitest run").as_deref(),
            Some("cd web && CI=1 '/opt/hk/hk' run --filter js_test --session abc-123 -- npx vitest run")
        );
    }

    #[test]
    fn leaves_other_commands_alone() {
        assert_eq!(pre("ls -la"), None);
        assert_eq!(pre("npm install left-pad"), None); // deny rule must keep working
        assert_eq!(pre("cargo test | tail -5"), None);
        assert_eq!(pre("npx vitest"), None); // watch mode
        assert_eq!(
            pre("'/opt/hk/hk' run --filter cargo_test --session x -- cargo test"),
            None
        );
    }

    #[test]
    fn ignores_background_and_other_tools() {
        let bg = json!({"tool_name": "Bash", "tool_input": {"command": "cargo test", "run_in_background": true}});
        assert!(pre_tool_use(&bg, "hk", &Rules::default()).is_none());
        let other = json!({"tool_name": "Read", "tool_input": {"file_path": "x"}});
        assert!(pre_tool_use(&other, "hk", &Rules::default()).is_none());
        assert!(pre_tool_use(&json!("garbage"), "hk", &Rules::default()).is_none());
    }
}
