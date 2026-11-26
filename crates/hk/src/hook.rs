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

/// PreToolUse(Bash): rewrite each part of the command line that a filter
/// owns to `<hk> run --filter F --session S -- part`. Lists (`a && b; c`) are
/// rewritten part by part; parts nobody owns stay verbatim. Returns the JSON
/// to print, if any.
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
    if command.contains("hk:raw") {
        return None; // `# hk:raw`: the user wants the raw output
    }
    let cwd = cwd_of(input);
    let cfg = Config::load(&cwd);
    if !cfg.rewrite.enabled {
        return None;
    }
    let plan = plan_rewrite(command, &cwd, &cfg)?;

    // Permission rules are evaluated against the rewritten command, so check
    // the original one against the user's own rules first (see permissions.rs).
    let catch_all = |rule: &str| rule == "Bash" || rule == "Bash(*)";
    let mut verdict = Verdict::Unknown;
    let mut rules_used: Vec<String> = Vec::new();
    let mut all_allowed = true;
    for stage in &plan.stages {
        match rules.verdict(stage) {
            Verdict::Deny | Verdict::Ask => return None,
            Verdict::Allow(r) => {
                if !rules_used.contains(&r) {
                    rules_used.push(r);
                }
            }
            Verdict::Unknown => all_allowed = false,
        }
    }
    match rules.verdict(command) {
        Verdict::Deny | Verdict::Ask => return None,
        Verdict::Allow(r) if catch_all(&r) => verdict = Verdict::Allow(r),
        _ => {}
    }
    // A single plain stage may restate a narrow rule; with a list, `cd` or an
    // env prefix only a catch-all `Bash` rule is known to cover the line.
    if matches!(verdict, Verdict::Unknown) && all_allowed && plan.single_plain {
        verdict = Verdict::Allow(rules_used.join(" and "));
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
    let rewritten = plan.render(&hk, &session);
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

/// How one part of a command line is rewritten.
struct Wrap<'a> {
    filter: &'static str,
    env: &'a str,
    rest: &'a str,
    merged: bool,
    pipe: Option<shell::Pipe<'a>>,
}

enum Piece<'a> {
    Verbatim(&'a str),
    Wrapped {
        lead: &'a str,
        wrap: Wrap<'a>,
        trail: &'a str,
    },
}

/// The rewrite of a whole command line, before the hk path and session are known.
pub struct Plan<'a> {
    pieces: Vec<(Piece<'a>, &'a str)>,
    /// Every command of the line, for the permission check.
    stages: Vec<&'a str>,
    /// One part, no `cd`, no env prefix (a pipe is fine: each stage is checked).
    single_plain: bool,
    /// Filters picked, in order (for `hk explain`).
    pub filters: Vec<&'static str>,
}

impl Plan<'_> {
    fn render(&self, hk: &str, session: &str) -> String {
        let mut s = String::new();
        for (piece, sep) in &self.pieces {
            match piece {
                Piece::Verbatim(t) => s.push_str(t),
                Piece::Wrapped { lead, wrap, trail } => {
                    s.push_str(lead);
                    s.push_str(&render_wrap(wrap, hk, session));
                    s.push_str(trail);
                }
            }
            s.push_str(sep);
        }
        s
    }
}

fn render_wrap(w: &Wrap, hk: &str, session: &str) -> String {
    let (env, rest, name) = (w.env, w.rest, w.filter);
    let base = format!("{env}{hk} run --filter {name} --session {session}");
    match &w.pipe {
        // `hk run` merges the child's stderr itself; the `2>&1` stays on hk.
        None if w.merged => format!("{base} -- {rest} 2>&1"),
        None => format!("{base} -- {rest}"),
        // The pipe stays in the shell, so its exit status is the one Bash
        // would report anyway; hk prints the log path first *and* last.
        Some(p) if p.kind == shell::PipeKind::Slice && w.merged => {
            format!("{base} --piped -- {rest} 2>&1 | {}", p.stage)
        }
        // No `2>&1`: tail/head only ever saw stdout, so keep it that way.
        Some(p) if p.kind == shell::PipeKind::Slice => {
            format!("{base} --piped --stdout-only -- {rest} | {}", p.stage)
        }
        // grep must see the raw output: wrap the grep stage instead, and cap
        // what grep prints.
        Some(p) => {
            let merge = if w.merged { " 2>&1" } else { "" };
            format!(
                "{env}{rest}{merge} | {hk} run --filter {} --session {session} -- {}",
                filters::generic::NAME,
                p.stage
            )
        }
    }
}

/// Decide, part by part, what to wrap. `None` when nothing would change.
pub fn plan_rewrite<'a>(command: &'a str, cwd: &Path, cfg: &Config) -> Option<Plan<'a>> {
    let parts = shell::split_chain(command)?;
    let mut dir = Some(cwd.to_path_buf());
    let mut pieces = Vec::new();
    let mut stages = Vec::new();
    let mut filters_used = Vec::new();
    let n_parts = parts.iter().filter(|p| !p.text.trim().is_empty()).count();
    let mut single_plain = n_parts == 1;
    for part in parts {
        let body = part.text.trim();
        let lead = &part.text[..part.text.len() - part.text.trim_start().len()];
        let trail = &part.text[part.text.trim_end().len()..];
        if body.is_empty() {
            pieces.push((Piece::Verbatim(part.text), part.sep));
            continue;
        }
        let wrap = shell::parse_pipeline(body).and_then(|p| {
            for st in std::iter::once(p.left.rest).chain(p.pipe.as_ref().map(|x| x.stage)) {
                stages.push(st);
            }
            if !p.left.env.is_empty() {
                single_plain = false;
            }
            // `cd DIR` as its own part: later parts run there.
            if p.left.words.first().map(String::as_str) == Some("cd") {
                dir = match (&dir, p.left.words.get(1)) {
                    (Some(d), Some(t))
                        if p.left.words.len() == 2 && !t.starts_with('~') && !t.contains('$') =>
                    {
                        Some(d.join(t))
                    }
                    _ => None,
                };
                single_plain = false;
                return None;
            }
            if p.left.env.contains("HK_RAW=1") {
                return None;
            }
            let cmd = Cmd::from_argv(&p.left.words)?;
            let name = choose_filter(&cmd, dir.as_deref(), cfg)?;
            if matches!(&p.pipe, Some(x) if x.kind == shell::PipeKind::Grep)
                && !cfg.filter_enabled(filters::generic::NAME)
            {
                return None;
            }
            Some(Wrap {
                filter: name,
                env: p.left.env,
                rest: p.left.rest,
                merged: p.merged,
                pipe: p.pipe,
            })
        });
        if wrap.is_none() && shell::parse_pipeline(body).is_none() {
            // Something we cannot read: still check it as a whole.
            stages.push(body);
        }
        match wrap {
            Some(w) => {
                filters_used.push(w.filter);
                pieces.push((
                    Piece::Wrapped {
                        lead,
                        wrap: w,
                        trail,
                    },
                    part.sep,
                ));
            }
            None => pieces.push((Piece::Verbatim(part.text), part.sep)),
        }
    }
    if filters_used.is_empty() {
        return None;
    }
    Some(Plan {
        pieces,
        stages,
        single_plain,
        filters: filters_used,
    })
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
        && (cfg.rewrite.generic_commands.contains(&cmd.program)
            || cfg.limits.commands.contains_key(&cmd.program)))
    .then_some(filters::generic::NAME)
}

/// PostToolUse: format touched files, shorten large MCP results, and record
/// large Bash outputs hk did not wrap.
pub fn post_tool_use(input: &Value) -> Option<Value> {
    let tool = input.get("tool_name")?.as_str()?;
    let cwd = cwd_of(input);
    let cfg = Config::load(&cwd);
    match tool {
        "Edit" | "Write" | "MultiEdit" => post_edit(input, tool, &cwd, &cfg),
        "Bash" => {
            record_miss(input, &cwd, &cfg);
            None
        }
        t if t.starts_with("mcp__") => crate::mcp::post_tool_use(input, &cfg),
        _ => None,
    }
}

fn post_edit(input: &Value, tool: &str, cwd: &Path, cfg: &Config) -> Option<Value> {
    if !cfg.format.enabled {
        return None;
    }
    let ti = input.get("tool_input")?;
    let file = ti.get("file_path")?.as_str()?;
    let path = if Path::new(file).is_absolute() {
        PathBuf::from(file)
    } else {
        cwd.join(file)
    };
    if cfg.format.when == "stop" {
        touched::add(&session_of(input), &path);
        return None;
    }
    let edited = match tool {
        "Write" => crate::format::Edited::Whole,
        "MultiEdit" => crate::format::Edited::Snippets(
            ti.get("edits")?
                .as_array()?
                .iter()
                .filter_map(|e| e.get("new_string")?.as_str().map(String::from))
                .collect(),
        ),
        _ => crate::format::Edited::Snippets(vec![ti.get("new_string")?.as_str()?.to_string()]),
    };
    let ctx = crate::format::process(&path, cfg, &edited)?;
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": ctx,
        }
    }))
}

fn session_of(input: &Value) -> String {
    sanitize_session(
        input
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or(""),
    )
}

/// Stop: with `format.when = "stop"`, format every file touched this turn.
pub fn stop(input: &Value) -> Option<Value> {
    let cwd = cwd_of(input);
    let cfg = Config::load(&cwd);
    if !cfg.format.enabled || cfg.format.when != "stop" {
        return None;
    }
    let session = session_of(input);
    let mut notes = Vec::new();
    for p in touched::take(&session) {
        if let Some(n) = crate::format::process(&p, &cfg, &crate::format::Edited::Whole) {
            notes.push(n);
        }
    }
    (!notes.is_empty()).then(|| json!({ "systemMessage": notes.join("\n") }))
}

/// Files edited since the last Stop, per session (for `format.when = "stop"`).
mod touched {
    use std::path::{Path, PathBuf};

    fn file(session: &str) -> PathBuf {
        crate::paths::data_dir()
            .join("touched")
            .join(format!("{session}.txt"))
    }

    pub fn add(session: &str, path: &Path) {
        let f = file(session);
        if let Some(d) = f.parent() {
            let _ = crate::paths::create_private_dir(d);
        }
        let mut list = std::fs::read_to_string(&f).unwrap_or_default();
        let p = path.display().to_string();
        if !list.lines().any(|l| l == p) {
            list.push_str(&p);
            list.push('\n');
            let _ = std::fs::write(&f, list);
        }
    }

    pub fn take(session: &str) -> Vec<PathBuf> {
        let f = file(session);
        let list = std::fs::read_to_string(&f).unwrap_or_default();
        let _ = std::fs::remove_file(&f);
        list.lines().map(PathBuf::from).collect()
    }
}

/// Why a command line is not wrapped, or which filters would wrap it.
pub fn explain(command: &str, cwd: &Path, cfg: &Config) -> String {
    if command.contains("hk:raw") {
        return "not wrapped: `hk:raw` asks for raw output".into();
    }
    if !cfg.rewrite.enabled {
        return "not wrapped: rewrite.enabled = false".into();
    }
    if let Some(plan) = plan_rewrite(command, cwd, cfg) {
        return format!(
            "wrapped with: {} (if your permission rules allow it, or in auto/bypass mode)",
            plan.filters.join(", ")
        );
    }
    let Some(parts) = shell::split_chain(command) else {
        return "not wrapped: heredoc, background `&`, comment or unbalanced quotes".into();
    };
    let mut reasons = Vec::new();
    for part in parts {
        let body = part.text.trim();
        if body.is_empty() {
            continue;
        }
        let r = match shell::parse_pipeline(body) {
            None => "redirection, subshell, `$(...)` or an unsupported pipe".to_string(),
            Some(p) => match Cmd::from_argv(&p.left.words) {
                None => "empty command".into(),
                Some(c) if filters::is_watch_mode(&c) => "watch/interactive mode".into(),
                Some(c) => format!("no filter for `{}`", c.program),
            },
        };
        reasons.push(format!("`{body}`: {r}"));
    }
    format!("not wrapped:\n  {}", reasons.join("\n  "))
}

/// Short machine reason for `hk misses`.
fn miss_reason(command: &str, cwd: &Path, cfg: &Config) -> String {
    if command.contains("hk:raw") {
        return "raw".into();
    }
    if plan_rewrite(command, cwd, cfg).is_some() {
        return "permissions".into();
    }
    let Some(parts) = shell::split_chain(command) else {
        return "unparsable".into();
    };
    for part in parts.iter().rev() {
        let body = part.text.trim();
        if body.is_empty() {
            continue;
        }
        return match shell::parse_pipeline(body) {
            None => "shape".into(),
            Some(p) => match Cmd::from_argv(&p.left.words) {
                Some(c) if filters::is_watch_mode(&c) => "watch".into(),
                Some(c) => format!("no-filter:{}", c.program),
                None => "empty".into(),
            },
        };
    }
    "empty".into()
}

fn record_miss(input: &Value, cwd: &Path, cfg: &Config) {
    if !cfg.misses.enabled {
        return;
    }
    let Some(command) = input
        .get("tool_input")
        .and_then(|t| t.get("command"))
        .and_then(Value::as_str)
    else {
        return;
    };
    if command.contains(" run --filter ") {
        return; // wrapped by hk: already counted by `hk run`
    }
    let resp = input.get("tool_response");
    let size = |k: &str| {
        resp.and_then(|r| r.get(k))
            .and_then(Value::as_str)
            .map_or(0, |s| s.chars().count())
    };
    let raw = match resp {
        Some(Value::String(s)) => s.chars().count(),
        _ => size("stdout") + size("stderr"),
    };
    if raw < cfg.misses.min_chars {
        return;
    }
    let reason = miss_reason(command, cwd, cfg);
    let program = shell::split_chain(command)
        .and_then(|p| p.last().and_then(|x| shell::split_words(x.text.trim())))
        .and_then(|w| w.into_iter().find(|x| !x.contains('=')))
        .map(|w| crate::filters::program_name(&w))
        .unwrap_or_default();
    let _ = crate::stats::append(
        &crate::paths::stats_file(),
        &crate::stats::Record {
            ts: crate::paths::now(),
            session: session_of(input),
            filter: program,
            raw,
            out: raw,
            project: crate::paths::project_root(cwd).display().to_string(),
            exit: None,
            ms: None,
            kind: format!("miss:{reason}"),
        },
    );
}

/// SessionStart: plain-text repo map (Claude Code adds SessionStart stdout to context).
pub fn session_start(input: &Value) -> Option<String> {
    let cwd = cwd_of(input);
    let cfg = Config::load(&cwd);
    let mut notes = String::new();
    let root = std::env::var_os("CLAUDE_PLUGIN_ROOT").map(PathBuf::from);
    for n in [
        crate::doctor::version_note(root.as_deref()),
        crate::doctor::unseen_note(),
    ]
    .into_iter()
    .flatten()
    {
        notes.push_str(&n);
        notes.push('\n');
    }
    if !cfg.session_start.enabled {
        return (!notes.is_empty()).then_some(notes);
    }
    let short = input.get("source").and_then(Value::as_str) == Some("compact");
    Some(crate::repomap::render_with(&cwd, short) + &notes)
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
        assert_eq!(
            pre("cargo test | tail -5").as_deref(),
            Some("'/opt/hk/hk' run --filter cargo_test --session abc-123 --piped --stdout-only -- cargo test | tail -5")
        );
        assert_eq!(
            pre("cargo build && cargo test; ls").as_deref(),
            Some("'/opt/hk/hk' run --filter build --session abc-123 -- cargo build && '/opt/hk/hk' run --filter cargo_test --session abc-123 -- cargo test; ls")
        );
        assert_eq!(pre("cargo test # hk:raw"), None);
        assert_eq!(pre("HK_RAW=1 cargo test"), None);
        assert_eq!(
            pre("hide exec TOKEN -- 'cargo test'").as_deref(),
            Some("'/opt/hk/hk' run --filter cargo_test --session abc-123 -- hide exec TOKEN -- 'cargo test'")
        );
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
