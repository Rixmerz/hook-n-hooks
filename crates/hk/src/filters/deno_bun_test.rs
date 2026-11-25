//! `deno test` and `bun test`.

use super::{Cmd, Filter};
use crate::config::Limits;
use crate::text::{count_before, push_capped};

pub struct DenoBunTest;

impl Filter for DenoBunTest {
    fn name(&self) -> &'static str {
        "deno_bun_test"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        matches!(cmd.program.as_str(), "deno" | "bun") && cmd.arg(0) == Some("test")
    }

    fn sniff(&self, output: &str) -> bool {
        output.lines().any(|l| {
            let t = l.trim();
            ((t.starts_with("ok |") || t.starts_with("FAILED |")) && t.contains(" passed"))
                || (t.starts_with("Ran ") && t.contains(" tests across "))
        })
    }

    fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String> {
        if let Some(s) = deno(output, exit, limits) {
            return Some(s);
        }
        bun(output, exit, limits)
    }
}

/// Deno summary: `ok | 1 passed | 0 failed (21ms)` / `FAILED | 1 passed | 1 failed (138ms)`.
fn deno(output: &str, exit: i32, limits: &Limits) -> Option<String> {
    let summary =
        output.lines().rev().map(str::trim).find(|l| {
            (l.starts_with("ok |") || l.starts_with("FAILED |")) && l.contains(" passed")
        })?;
    let passed = count_before(summary, " passed").unwrap_or(0);
    let failed = count_before(summary, " failed").unwrap_or(0);
    if failed == 0 && exit == 0 {
        return Some(format!("✓ {passed} passed"));
    }
    // ERRORS section: `name => ./file.ts:3:6` followed by `error: ...` lines.
    let mut fails = Vec::new();
    let mut in_errors = false;
    let mut cur: Option<(String, Vec<String>)> = None;
    for line in output.lines() {
        let t = line.trim();
        match t {
            "ERRORS" => {
                in_errors = true;
                continue;
            }
            "FAILURES" => {
                in_errors = false;
                if let Some(c) = cur.take() {
                    fails.push(c);
                }
                continue;
            }
            _ => {}
        }
        if !in_errors {
            continue;
        }
        if t.contains(" => ") && !t.starts_with("at ") {
            if let Some(c) = cur.take() {
                fails.push(c);
            }
            cur = Some((t.to_string(), Vec::new()));
        } else if let Some((_, body)) = cur.as_mut() {
            if !t.is_empty() && t != "^" && body.len() < limits.stack_lines + 3 {
                body.push(t.to_string());
            }
        }
    }
    if let Some(c) = cur.take() {
        fails.push(c);
    }
    let mut out = vec![format!("✗ {failed} failed, {passed} passed")];
    push_capped(&mut out, render(fails), limits.max_errors);
    Some(out.join("\n"))
}

/// Bun summary: ` 1 pass` / ` 1 fail` lines; failures end with `(fail) name`.
/// Bun prints the error *before* the `(fail)` line, so we buffer.
fn bun(output: &str, exit: i32, limits: &Limits) -> Option<String> {
    let mut passed = None;
    let mut failed = None;
    for line in output.lines() {
        let t = line.trim();
        if let Some(n) = t.strip_suffix(" pass").and_then(|n| n.parse::<u64>().ok()) {
            passed = Some(n);
        }
        if let Some(n) = t.strip_suffix(" fail").and_then(|n| n.parse::<u64>().ok()) {
            failed = Some(n);
        }
    }
    let passed = passed?;
    let failed = failed.unwrap_or(0);
    if failed == 0 && exit == 0 {
        return Some(format!("✓ {passed} passed"));
    }
    let mut fails = Vec::new();
    let mut buf: Vec<String> = Vec::new();
    for line in output.lines() {
        let t = line.trim();
        if let Some(name) = t.strip_prefix("(fail) ") {
            let body: Vec<String> = buf
                .iter()
                .filter(|l| {
                    l.starts_with("error:") || l.starts_with("at ") || l.starts_with(['-', '+'])
                })
                .take(limits.stack_lines + 3)
                .cloned()
                .collect();
            fails.push((name.to_string(), body));
            buf.clear();
        } else if t.starts_with("(pass) ") || t.ends_with(".test.ts:") || t.ends_with(".test.js:") {
            buf.clear();
        } else if !t.is_empty() {
            buf.push(t.to_string());
        }
    }
    let mut out = vec![format!("✗ {failed} failed, {passed} passed")];
    push_capped(&mut out, render(fails), limits.max_errors);
    Some(out.join("\n"))
}

fn render(items: Vec<(String, Vec<String>)>) -> Vec<String> {
    items
        .into_iter()
        .map(|(name, body)| {
            let mut s = format!("FAIL {name}");
            for b in body {
                s.push_str("\n  ");
                s.push_str(&b);
            }
            s
        })
        .collect()
}
