//! `dotnet build` and `dotnet test` (also `msbuild`).
//!
//! Build: MSBuild diagnostics `File.cs(10,5): error CS1002: msg [proj]`
//! become `File.cs:10: error CS1002: msg`, de-duplicated (MSBuild repeats
//! them in its summary). Test: the `Passed!`/`Failed!` summary plus each
//! failing test with its message and first stack frame.

use super::build::dedup_count;
use super::{Cmd, Filter};
use crate::config::Limits;
use crate::text::{count_before, fail_block, push_capped};

pub struct Dotnet;

impl Filter for Dotnet {
    fn name(&self) -> &'static str {
        "dotnet"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        match cmd.program.as_str() {
            "dotnet" => {
                matches!(cmd.subcommand(), Some("build" | "test" | "msbuild"))
                    && !cmd
                        .args
                        .iter()
                        .any(|a| a == "--list-tests" || a.starts_with("--logger:json"))
            }
            "msbuild" => true,
            _ => false,
        }
    }

    fn sniff(&self, output: &str) -> bool {
        output.lines().any(|l| {
            let t = l.trim();
            t == "Build succeeded."
                || t == "Build FAILED."
                || t.starts_with("Passed!  -")
                || t.starts_with("Failed!  -")
        })
    }

    fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String> {
        let lines: Vec<&str> = output.lines().collect();
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        for l in &lines {
            if let Some((entry, is_err)) = msbuild_diag(l.trim()) {
                if is_err {
                    errors.push(entry);
                } else {
                    warnings.push(entry);
                }
            }
        }
        let test_summary = lines
            .iter()
            .map(|l| l.trim())
            .filter(|l| l.starts_with("Passed!") || l.starts_with("Failed!"))
            .collect::<Vec<_>>();
        let build_result = lines
            .iter()
            .map(|l| l.trim())
            .find(|l| *l == "Build succeeded." || *l == "Build FAILED.");
        if test_summary.is_empty() && build_result.is_none() && errors.is_empty() {
            return None;
        }
        let fails = test_failures(&lines, limits.stack_lines);
        let mut out = Vec::new();
        let mark = if exit == 0 { "✓" } else { "✗" };
        if !test_summary.is_empty() {
            let failed: u64 = test_summary
                .iter()
                .filter_map(|s| count_after(s, "Failed:"))
                .sum();
            let passed: u64 = test_summary
                .iter()
                .filter_map(|s| count_after(s, "Passed:"))
                .sum();
            let skipped: u64 = test_summary
                .iter()
                .filter_map(|s| count_after(s, "Skipped:"))
                .sum();
            out.push(format!(
                "{mark} dotnet test: {failed} failed, {passed} passed, {skipped} skipped"
            ));
        } else {
            out.push(format!(
                "{mark} dotnet build: {} error(s), {} warning(s)",
                dedup_len(&errors).max(count_summary(&lines, " Error(s)")),
                dedup_len(&warnings).max(count_summary(&lines, " Warning(s)"))
            ));
        }
        let mut items = dedup_count(strip_counts(errors));
        items.extend(fails);
        items.extend(dedup_count(strip_counts(warnings)));
        push_capped(&mut out, items, limits.max_errors);
        Some(out.join("\n"))
    }
}

fn dedup_len(v: &[String]) -> usize {
    let mut s: Vec<&String> = v.iter().collect();
    s.sort();
    s.dedup();
    s.len()
}

/// MSBuild prints every diagnostic twice (inline and in the summary); the
/// `(x2)` that would add is noise, so duplicates are dropped outright.
fn strip_counts(v: Vec<String>) -> Vec<String> {
    crate::text::dedup(v)
}

/// `    3 Warning(s)` in the build summary.
fn count_summary(lines: &[&str], word: &str) -> usize {
    lines
        .iter()
        .rev()
        .find_map(|l| count_before(l, word))
        .unwrap_or(0) as usize
}

/// `Failed:     1,` -> 1
fn count_after(s: &str, key: &str) -> Option<u64> {
    let idx = s.find(key)? + key.len();
    let digits: String = s[idx..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// `/a/File.cs(10,5): error CS1002: ; expected [/a/p.csproj]`.
fn msbuild_diag(t: &str) -> Option<(String, bool)> {
    let (is_err, idx) = match (t.find("): error "), t.find("): warning ")) {
        (Some(i), _) => (true, i),
        (None, Some(i)) => (false, i),
        _ => return None,
    };
    let open = t[..idx].rfind('(')?;
    let line = t[open + 1..idx].split(',').next()?;
    let mut msg = &t[idx + 3..];
    if let Some(p) = msg.rfind(" [") {
        if msg.ends_with(']') {
            msg = &msg[..p];
        }
    }
    Some((format!("{}:{line}: {}", &t[..open], msg.trim()), is_err))
}

/// `  Failed Name [12 ms]` blocks: `Error Message:` and `Stack Trace:` lines.
fn test_failures(lines: &[&str], stack: usize) -> Vec<String> {
    let mut res = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        if let Some(name) = t.strip_prefix("Failed ").filter(|r| r.ends_with(']')) {
            let name = name.rsplit_once(" [").map_or(name, |(n, _)| n);
            let mut body = Vec::new();
            let mut j = i + 1;
            while j < lines.len() {
                let b = lines[j].trim();
                if b.starts_with("Failed ") || b.starts_with("Passed ") || b.starts_with("Failed!")
                {
                    break;
                }
                let frame = b.starts_with("at ") && b.contains(" in ");
                let keep = !b.is_empty()
                    && b != "Error Message:"
                    && b != "Stack Trace:"
                    && (!b.starts_with("at ") || frame);
                if keep && body.len() < stack {
                    body.push(b.to_string());
                }
                j += 1;
            }
            res.push(fail_block(name, &body));
            i = j;
            continue;
        }
        i += 1;
    }
    res
}
