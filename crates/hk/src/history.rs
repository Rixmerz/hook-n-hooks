//! Compare a test run with the previous run of the same command in the same
//! project: which failures are new and which were fixed. Iterating on a fix
//! then shows the delta instead of the same summary again.
//!
//! One small file per (project, command) under `<data>/history/`, holding the
//! `FAIL ...` headers of the last run.

use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

fn file_for(project: &Path, argv: &[String]) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    project.hash(&mut h);
    argv.hash(&mut h);
    crate::paths::data_dir()
        .join("history")
        .join(format!("{:016x}.txt", h.finish()))
}

/// Failure names in a compacted test summary (`FAIL <name>` lines).
pub fn failures(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("FAIL "))
        .map(|n| n.trim().to_string())
        .collect()
}

fn is_test_summary(text: &str) -> bool {
    let first = text.lines().next().unwrap_or("");
    first.starts_with('✓') || first.starts_with('✗')
}

/// Record this run and describe how it differs from the previous one. `None`
/// for non-test output, a first run, or no change in the failure set.
pub fn compare(project: &Path, argv: &[String], text: &str, filter: &str) -> Option<String> {
    if !is_test_summary(text) || filter == crate::filters::generic::NAME {
        return None;
    }
    let path = file_for(project, argv);
    let now = failures(text);
    let before: Option<BTreeSet<String>> = std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.lines().map(String::from).collect());
    if let Some(dir) = path.parent() {
        let _ = crate::paths::create_private_dir(dir);
    }
    let body: Vec<&str> = now.iter().map(String::as_str).collect();
    let _ = std::fs::write(&path, body.join("\n"));
    describe(&before?, &now)
}

pub fn describe(before: &BTreeSet<String>, now: &BTreeSet<String>) -> Option<String> {
    let new: Vec<&String> = now.difference(before).collect();
    let fixed: Vec<&String> = before.difference(now).collect();
    if new.is_empty() && fixed.is_empty() {
        return (!now.is_empty()).then(|| "[hk] vs last run: same failures".to_string());
    }
    let mut parts = Vec::new();
    if !new.is_empty() {
        parts.push(format!("{} new", new.len()));
    }
    if !fixed.is_empty() {
        parts.push(format!("{} fixed", fixed.len()));
    }
    let mut out = format!("[hk] vs last run: {}", parts.join(", "));
    for n in new.iter().take(10) {
        out.push_str(&format!("\n  + {n}"));
    }
    for f in fixed.iter().take(10) {
        out.push_str(&format!("\n  - {f}"));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_delta() {
        let set = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        let d = describe(&set(&["a", "b"]), &set(&["b", "c"])).unwrap();
        assert!(d.starts_with("[hk] vs last run: 1 new, 1 fixed"));
        assert!(d.contains("+ c") && d.contains("- a"));
        assert_eq!(describe(&set(&[]), &set(&[])), None);
        assert_eq!(
            failures("✗ 1 failed\nFAIL a > b\n  expected"),
            set(&["a > b"])
        );
    }
}
