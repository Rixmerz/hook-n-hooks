//! `pytest` (also via `python -m pytest`, `uv run pytest`, `poetry run pytest`).

use super::{Cmd, Filter};
use crate::config::Limits;
use crate::text::{count_before, push_capped};

pub struct Pytest;

impl Filter for Pytest {
    fn name(&self) -> &'static str {
        "pytest"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        matches!(cmd.program.as_str(), "pytest" | "py.test")
    }

    fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String> {
        // Final line: `==== 1 failed, 2 passed in 0.09s ====` (or `-q`: `1 failed, 2 passed in 0.09s`).
        let summary = output
            .lines()
            .rev()
            .map(|l| l.trim().trim_matches('=').trim())
            .find(|l| {
                l.contains(" in ")
                    && (l.contains("passed")
                        || l.contains("failed")
                        || l.contains("error")
                        || l.contains("no tests ran"))
            })?
            .to_string();
        let failed = count_before(&summary, " failed").unwrap_or(0)
            + count_before(&summary, " error").unwrap_or(0);
        if failed == 0 && exit == 0 {
            let counts = summary.split(" in ").next().unwrap_or(&summary);
            return Some(format!("✓ {counts}"));
        }
        let mut out = vec![format!("✗ {summary}")];
        push_capped(
            &mut out,
            failures(output, limits.stack_lines),
            limits.max_errors,
        );
        Some(out.join("\n"))
    }
}

/// Parse the `FAILURES` / `ERRORS` sections: one block per `___ name ___`
/// header, keeping `>` (failing line), `E ` (assertion) and `file:line:` lines.
fn failures(output: &str, stack: usize) -> Vec<String> {
    let mut res = Vec::new();
    let mut in_section = false;
    let mut cur: Option<(String, Vec<String>)> = None;
    let flush = |cur: &mut Option<(String, Vec<String>)>, res: &mut Vec<String>| {
        if let Some((name, body)) = cur.take() {
            let mut s = format!("FAIL {name}");
            for b in body {
                s.push_str("\n  ");
                s.push_str(&b);
            }
            res.push(s);
        }
    };
    for line in output.lines() {
        let t = line.trim();
        if t.starts_with("===") {
            flush(&mut cur, &mut res);
            in_section = t.contains(" FAILURES ") || t.contains(" ERRORS ");
            continue;
        }
        if !in_section {
            continue;
        }
        if t.starts_with("___") && t.ends_with("___") {
            flush(&mut cur, &mut res);
            let name = t.trim_matches('_').trim().to_string();
            cur = Some((name, Vec::new()));
            continue;
        }
        if let Some((_, body)) = cur.as_mut() {
            let keep = t.starts_with('>')
                || t.starts_with("E ")
                || (t.contains(".py:") && !t.starts_with("def "));
            if keep && body.len() < stack {
                body.push(t.to_string());
            }
        }
    }
    flush(&mut cur, &mut res);
    res
}
