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
    let pipeline = shell::parse_pipeline(command)?;
    let simple = &pipeline.left;
    let cmd = Cmd::from_argv(&simple.words)?;
    let cwd = cwd_of(input);
    let dir = crate::scripts::prefix_dir(&cwd, simple.prefix);
    let name = choose_filter(&cmd, dir.as_deref(), &cfg)?;
    if matches!(&pipeline.pipe, Some(p) if p.kind == shell::PipeKind::Grep)
        && !cfg.filter_enabled(filters::generic::NAME)
    {
        return None;
    }
    // Permission rules are evaluated against the rewritten command, so check
    // the original one against the user's own rules first (see permissions.rs).
    let mut verdict = rules.verdict(simple.rest);
    if let v @ (Verdict::Deny | Verdict::Ask) = rules.verdict(command) {
        // A deny/ask rule on the full command (with `cd`/env prefix) also counts.
        verdict = v;
    }
    let catch_all = |rule: &str| rule == "Bash" || rule == "Bash(*)";
    if let Some(pipe) = &pipeline.pipe {
        // Every stage must be allowed on its own, and no stage may be denied.
        match (rules.verdict(pipe.stage), &verdict) {
            (v @ (Verdict::Deny | Verdict::Ask), _) => verdict = v,
            (_, Verdict::Deny | Verdict::Ask) => {}
            (Verdict::Allow(r2), Verdict::Allow(r1)) => {
                if !catch_all(r1) {
                    verdict = Verdict::Allow(format!("{r1} and {r2}"));
                }
            }
            (_, Verdict::Allow(r1)) if catch_all(r1) => {}
            _ => verdict = Verdict::Unknown,
        }
    }
    // With a `cd`/env prefix, only a catch-all `Bash` rule is known to cover
    // the whole command line; narrower rules are not restated.
    if !simple.prefix.is_empty() || !simple.env.is_empty() {
        if let Verdict::Allow(rule) = &verdict {
            if !catch_all(rule) {
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
    let hk = shell::quote(hk_path);
    let (pre, env, rest) = (simple.prefix, simple.env, simple.rest);
    let rewritten = match &pipeline.pipe {
        // `hk run` merges the child's stderr itself; the `2>&1` stays on hk.
        None if pipeline.merged => {
            format!("{pre}{env}{hk} run --filter {name} --session {session} -- {rest} 2>&1")
        }
        None => format!("{pre}{env}{hk} run --filter {name} --session {session} -- {rest}"),
        // The pipe stays in the shell, so its exit status is the one Bash
        // would report anyway; hk prints the log path first *and* last.
        Some(p) if p.kind == shell::PipeKind::Slice => format!(
            "{pre}{env}{hk} run --filter {name} --session {session} --piped -- {rest} 2>&1 | {}",
            p.stage
        ),
        // grep must see the raw output: wrap the grep stage instead, and cap
        // what grep prints.
        Some(p) => format!(
            "{pre}{env}{rest} 2>&1 | {hk} run --filter {} --session {session} -- {}",
            filters::generic::NAME,
            p.stage
        ),
    };
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

/// Pick the filter for a command, or `None` to leave it alone. `dir` is the
/// directory the command runs in (for package scripts, Makefiles and
/// justfiles), when known.
pub fn choose_filter(cmd: &Cmd, dir: Option<&Path>, cfg: &Config) -> Option<&'static str> {
    use crate::scripts::{self, Decision};
    if filters::is_watch_mode(cmd) {
        return None;
    }
    let enabled = |n: &str| cfg.filter_enabled(n);
    // Built-in commands win over same-named scripts (`pnpm install` with an
    // `install` script is still an install).
    if let Some(f) = filters::find(cmd, &enabled) {
        return Some(f.name());
    }
    let has_script = |n: &str| dir.is_some_and(|d| scripts::package_script(d, n).is_some());
    if let Some(w) = scripts::wrapper(cmd, &has_script) {
        let decision = match dir {
            Some(d) => scripts::decide(&w, d),
            None => scripts::decide_unresolved(&w),
        };
        let resolved = match decision {
            Decision::Skip => return None,
            Decision::Resolved(c) => filters::find(&c, &enabled).map(|f| f.name()),
            Decision::Sniff | Decision::Unknown => None,
        };
        if resolved.is_some() {
            return resolved;
        }
        if scripts::decide_unresolved(&w) == Decision::Sniff && enabled(filters::script::NAME) {
            return Some(filters::script::NAME);
        }
        return generic_if_listed(cmd, cfg);
    }
    generic_if_listed(cmd, cfg)
}

/// `rewrite.generic_commands` in hk.toml: wrap these with the generic filter.
fn generic_if_listed(cmd: &Cmd, cfg: &Config) -> Option<&'static str> {
    (cfg.filter_enabled(filters::generic::NAME)
        && cfg.rewrite.generic_commands.contains(&cmd.program))
    .then_some(filters::generic::NAME)
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

    fn pre_with(cmd: &str, cwd: &Path, rules: &Rules, mode: &str) -> Option<String> {
        let input = json!({
            "session_id": "s",
            "cwd": cwd,
            "permission_mode": mode,
            "tool_name": "Bash",
            "tool_input": {"command": cmd}
        });
        let out = pre_tool_use(&input, "hk", rules)?;
        Some(
            out["hookSpecificOutput"]["updatedInput"]["command"]
                .as_str()?
                .to_string(),
        )
    }

    #[test]
    fn rewrites_pipelines_keeping_the_pipe() {
        assert_eq!(
            pre("cargo test 2>&1").as_deref(),
            Some("'/opt/hk/hk' run --filter cargo_test --session abc-123 -- cargo test 2>&1")
        );
        assert_eq!(
            pre("cargo test 2>&1 | tail -20").as_deref(),
            Some("'/opt/hk/hk' run --filter cargo_test --session abc-123 --piped -- cargo test 2>&1 | tail -20")
        );
        assert_eq!(
            pre("cd a && cargo test |& grep -E 'FAIL|panicked'").as_deref(),
            Some("cd a && cargo test 2>&1 | '/opt/hk/hk' run --filter generic --session abc-123 -- grep -E 'FAIL|panicked'")
        );
        assert_eq!(pre("cargo test 2>&1 | sort"), None);
        assert_eq!(pre("cargo test 2>/dev/null"), None);
        assert_eq!(pre("cargo build && cargo test"), None);
    }

    #[test]
    fn pipelines_respect_every_stage_rule() {
        let tmp = std::env::temp_dir();
        let r = |allow: &[&str], deny: &[&str]| Rules {
            allow: allow.iter().map(|s| s.to_string()).collect(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
            ask: vec![],
        };
        let c = "cargo test 2>&1 | tail -5";
        // A denied stage blocks the rewrite, even under a catch-all allow.
        assert!(pre_with(c, &tmp, &r(&["Bash"], &["Bash(tail:*)"]), "default").is_none());
        // Only the left side allowed: not restated, so no rewrite in default mode.
        assert!(pre_with(c, &tmp, &r(&["Bash(cargo test:*)"], &[]), "default").is_none());
        let both = r(&["Bash(cargo test:*)", "Bash(tail:*)"], &[]);
        let input = json!({"cwd": tmp, "tool_name": "Bash", "tool_input": {"command": c}});
        let out = pre_tool_use(&input, "hk", &both).unwrap();
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "allow");
    }

    #[test]
    fn resolves_script_wrappers() {
        let dir = std::env::temp_dir().join(format!("hk-hook-scripts-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("web")).unwrap();
        std::fs::write(
            dir.join("web/package.json"),
            r#"{"scripts":{"test":"vitest run","lint":"eslint .","dev":"vite","e2e":"playwright test --reporter=list","install":"node-gyp rebuild"}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("Makefile"),
            "test:\n\tpytest -q\nserve:\n\tpython -m http.server\n",
        )
        .unwrap();
        let all = Rules {
            allow: vec!["Bash".into()],
            ..Rules::default()
        };
        let f = |c: &str| {
            pre_with(c, &dir, &all, "default").map(|s| {
                s.split("--filter ")
                    .nth(1)
                    .unwrap()
                    .split(' ')
                    .next()
                    .unwrap()
                    .to_string()
            })
        };
        // Before v1.0 only `npm test` was wrapped, always with the vitest/jest filter.
        assert_eq!(f("cd web && npm test").as_deref(), Some("js_test"));
        assert_eq!(f("cd web && npm run lint").as_deref(), Some("lint"));
        assert_eq!(f("cd web && pnpm lint").as_deref(), Some("lint"));
        assert_eq!(f("cd web && npm run e2e").as_deref(), Some("playwright"));
        assert_eq!(f("cd web && npm run dev"), None);
        // A same-named script does not hide the built-in `pnpm install`.
        assert_eq!(f("cd web && pnpm install").as_deref(), Some("install"));
        assert_eq!(f("cd web && npm run build").as_deref(), Some("script"));
        assert_eq!(f("make test").as_deref(), Some("pytest"));
        assert_eq!(f("make serve"), None);
        assert_eq!(f("make"), None);
        assert_eq!(f("just test").as_deref(), Some("script"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generic_commands_still_wrap_unknown_targets() {
        let dir = std::env::temp_dir().join(format!("hk-hook-generic-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::write(
            dir.join(".claude/hk.toml"),
            "[rewrite]\ngeneric_commands = [\"make\"]\n",
        )
        .unwrap();
        let all = Rules {
            allow: vec!["Bash".into()],
            ..Rules::default()
        };
        let got = |c: &str| pre_with(c, &dir, &all, "default");
        assert!(got("make docs").unwrap().contains("--filter generic "));
        assert!(got("make").unwrap().contains("--filter generic "));
        assert!(got("make test").unwrap().contains("--filter script "));
        assert_eq!(got("make serve"), None); // never, even when listed
        let _ = std::fs::remove_dir_all(&dir);
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
