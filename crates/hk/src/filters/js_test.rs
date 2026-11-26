//! Vitest and Jest. Behind `npm test` and other package scripts the hook
//! resolves the script first (see `scripts.rs`), or the `script` filter
//! recognises the output.

use super::{Cmd, Filter};
use crate::config::Limits;
use crate::text::{count_before, push_capped};

pub struct JsTest;

impl Filter for JsTest {
    fn name(&self) -> &'static str {
        "js_test"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        matches!(cmd.program.as_str(), "vitest" | "jest")
    }

    fn sniff(&self, output: &str) -> bool {
        output.lines().any(|l| {
            let t = l.trim_start();
            t.starts_with("Test Files ") || t.starts_with("Test Suites:")
        })
    }

    fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String> {
        // Vitest: `      Tests  1 failed | 1 passed (2)`; Jest: `Tests:       1 failed, 1 passed, 2 total`.
        let summary = output.lines().rev().find_map(|l| {
            let t = l.trim();
            t.strip_prefix("Tests:")
                .or_else(|| t.strip_prefix("Tests "))
                .map(|r| r.trim().to_string())
        })?;
        let passed = count_before(&summary, " passed").unwrap_or(0);
        let failed = count_before(&summary, " failed").unwrap_or(0);
        let skipped = count_before(&summary, " skipped").unwrap_or(0);
        let skip = if skipped > 0 {
            format!(", {skipped} skipped")
        } else {
            String::new()
        };
        if failed == 0 && exit == 0 {
            return Some(format!("✓ {passed} passed{skip}"));
        }
        let mut out = vec![format!("✗ {failed} failed, {passed} passed{skip}")];
        push_capped(
            &mut out,
            failures(output, limits.stack_lines),
            limits.max_errors,
        );
        Some(out.join("\n"))
    }
}

/// Failure blocks start with ` FAIL  file > name` (vitest) or `● name` (jest).
/// Keep the header, the assertion message, diff lines and the first
/// `at`/location lines, up to `stack` + 3 lines per failure.
fn failures(output: &str, stack: usize) -> Vec<String> {
    let mut res: Vec<String> = Vec::new();
    let mut cur: Option<(String, Vec<String>)> = None;
    let budget = stack + 3;
    for line in output.lines() {
        let t = line.trim();
        let header = t
            .strip_prefix("FAIL ")
            .filter(|r| r.contains(" > "))
            .map(|r| r.trim().to_string())
            .or_else(|| t.strip_prefix("● ").map(|r| r.trim().to_string()));
        if let Some(name) = header {
            if let Some(c) = cur.take() {
                res.push(render(c));
            }
            cur = Some((name, Vec::new()));
            continue;
        }
        // End of a failure block.
        if t.starts_with("Test Files") || t.starts_with("Test Suites:") || t.starts_with("⎯⎯") {
            if let Some(c) = cur.take() {
                res.push(render(c));
            }
            continue;
        }
        if let Some((_, body)) = cur.as_mut() {
            let noise = t.is_empty()
                || t.starts_with('|')
                || t.chars().next().is_some_and(|c| c.is_ascii_digit()) && t.contains('|');
            // Always keep the first stack location, even past the budget.
            let first_at = t.starts_with("at ") && !body.iter().any(|b| b.starts_with("at "));
            if !noise && (body.len() < budget || first_at) {
                body.push(t.to_string());
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
