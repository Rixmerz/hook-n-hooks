//! `cargo test` (libtest output).

use super::{build, Cmd, Filter};
use crate::config::Limits;
use crate::text::{count_before, push_capped};

pub struct CargoTest;

impl Filter for CargoTest {
    fn name(&self) -> &'static str {
        "cargo_test"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        cmd.program == "cargo" && matches!(cmd.subcommand(), Some("test" | "t"))
    }

    fn sniff(&self, output: &str) -> bool {
        output
            .lines()
            .any(|l| l.trim_start().starts_with("test result: "))
    }

    fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String> {
        let (mut passed, mut failed, mut ignored, mut seen) = (0, 0, 0, false);
        for line in output.lines() {
            if let Some(rest) = line.trim().strip_prefix("test result: ") {
                seen = true;
                passed += count_before(rest, " passed").unwrap_or(0);
                failed += count_before(rest, " failed").unwrap_or(0);
                ignored += count_before(rest, " ignored").unwrap_or(0);
            }
        }
        if !seen {
            // Usually a compile error: summarise it like a build.
            let d = build::diagnostics(output);
            if exit != 0 && !d.errors.is_empty() {
                return Some(d.render(exit, limits.max_errors));
            }
            return None;
        }
        let ign = if ignored > 0 {
            format!(", {ignored} ignored")
        } else {
            String::new()
        };
        if failed == 0 && exit == 0 {
            return Some(format!("✓ {passed} passed{ign}"));
        }
        let mut out = vec![format!("✗ {failed} failed, {passed} passed{ign}")];
        push_capped(
            &mut out,
            failures(output, limits.stack_lines),
            limits.max_errors,
        );
        if failed == 0 {
            // Tests passed but cargo still failed (e.g. a doc-test build error).
            let d = build::diagnostics(output);
            push_capped(&mut out, d.errors, limits.max_errors);
        }
        Some(out.join("\n"))
    }
}

/// One entry per `---- name stdout ----` block: name plus the first
/// `stack` meaningful lines (panic location, assertion, left/right).
fn failures(output: &str, stack: usize) -> Vec<String> {
    let mut res = Vec::new();
    let mut cur: Option<(String, Vec<String>)> = None;
    for line in output.lines() {
        let t = line.trim();
        if let Some(name) = t.strip_prefix("---- ").and_then(|r| {
            r.strip_suffix(" stdout ----")
                .or_else(|| r.strip_suffix(" stderr ----"))
        }) {
            if let Some(c) = cur.take() {
                res.push(render(c));
            }
            cur = Some((name.to_string(), Vec::new()));
            continue;
        }
        if t == "failures:" || t.starts_with("test result:") {
            if let Some(c) = cur.take() {
                res.push(render(c));
            }
            continue;
        }
        if let Some((_, body)) = cur.as_mut() {
            if t.is_empty() || t.starts_with("note: run with `RUST_BACKTRACE") {
                continue;
            }
            if body.len() < stack {
                // Drop the thread id: `thread 'x' (1234) panicked at` -> `panicked at`.
                let l = match t.find(" panicked at ") {
                    Some(i) if t.starts_with("thread ") => format!("panicked at {}", &t[i + 13..]),
                    _ => t.to_string(),
                };
                body.push(l);
            }
        }
    }
    if let Some(c) = cur.take() {
        res.push(render(c));
    }
    res
}

fn render((name, body): (String, Vec<String>)) -> String {
    let mut s = format!("FAIL {name}");
    for b in body {
        s.push_str("\n  ");
        s.push_str(&b);
    }
    s
}
