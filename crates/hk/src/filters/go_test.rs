//! `go test`.

use super::{build, Cmd, Filter};
use crate::config::Limits;
use crate::text::push_capped;

pub struct GoTest;

impl Filter for GoTest {
    fn name(&self) -> &'static str {
        "go_test"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        cmd.program == "go" && cmd.subcommand() == Some("test")
    }

    fn sniff(&self, output: &str) -> bool {
        output.lines().any(|l| {
            l.starts_with("ok  \t") || l.starts_with("FAIL\t") || l.starts_with("--- FAIL: ")
        })
    }

    fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String> {
        let mut ok_pkgs = 0;
        let mut fail_pkgs = Vec::new();
        let mut passed = 0;
        for line in output.lines() {
            let t = line.trim_end();
            if t.starts_with("ok ") || t.starts_with("ok\t") {
                ok_pkgs += 1;
            } else if let Some(r) = t.strip_prefix("FAIL\t").or_else(|| t.strip_prefix("FAIL ")) {
                fail_pkgs.push(r.split_whitespace().next().unwrap_or(r).to_string());
            } else if t.trim_start().starts_with("--- PASS:") {
                passed += 1;
            }
        }
        let tests = failures(output, limits.stack_lines);
        if exit == 0 && fail_pkgs.is_empty() && tests.is_empty() {
            if ok_pkgs == 0 {
                return None;
            }
            let counted = if passed > 0 {
                format!("{passed} passed, ")
            } else {
                String::new()
            };
            return Some(format!("✓ {counted}{ok_pkgs} package(s) ok"));
        }
        let mut out = vec![format!(
            "✗ {} test(s) failed in {} package(s), {ok_pkgs} package(s) ok",
            tests.len(),
            fail_pkgs.len()
        )];
        push_capped(&mut out, tests, limits.max_errors);
        // Build failures inside `go test` (`# pkg` + `file.go:l:c: msg`).
        let d = build::diagnostics(output);
        push_capped(&mut out, d.errors, limits.max_errors);
        if out.len() == 1 && exit != 0 {
            return None;
        }
        Some(out.join("\n"))
    }
}

fn failures(output: &str, stack: usize) -> Vec<String> {
    let mut res = Vec::new();
    let mut cur: Option<(String, Vec<String>)> = None;
    for line in output.lines() {
        let t = line.trim();
        if let Some(r) = t.strip_prefix("--- FAIL: ") {
            if let Some(c) = cur.take() {
                res.push(c);
            }
            let name = r.split(" (").next().unwrap_or(r).to_string();
            cur = Some((name, Vec::new()));
            continue;
        }
        if t.starts_with("--- ")
            || t.starts_with("=== ")
            || t == "FAIL"
            || t.starts_with("FAIL\t")
            || t.starts_with("ok ")
        {
            if let Some(c) = cur.take() {
                res.push(c);
            }
            continue;
        }
        if let Some((_, body)) = cur.as_mut() {
            if !t.is_empty() && body.len() < stack {
                body.push(t.to_string());
            }
        }
    }
    if let Some(c) = cur.take() {
        res.push(c);
    }
    res.into_iter()
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
