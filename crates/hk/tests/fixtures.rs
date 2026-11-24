//! Snapshot tests over real captured outputs in `tests/fixtures/<tool>/`.
//!
//! Each fixture is `<tool>/<case>.txt`; the table below maps it to the command
//! that produced it and the exit code it returned. Run `cargo insta review`
//! after changing a filter.

use hk::config::Config;
use hk::filters::{self, Cmd};
use hk::run::compact;

/// (tool dir, case, command line used for detection, exit code)
const CASES: &[(&str, &str, &str, i32)] = &[
    ("cargo_test", "pass", "cargo test", 0),
    ("cargo_test", "fail", "cargo test", 101),
    ("cargo_build", "pass", "cargo build", 0),
    ("cargo_build", "fail", "cargo build", 101),
    ("pytest", "pass", "pytest", 0),
    ("pytest", "fail", "pytest", 1),
    ("vitest", "pass", "npx vitest run", 0),
    ("vitest", "fail", "npx vitest run", 1),
    ("jest", "pass", "npx jest --ci", 0),
    ("jest", "fail", "npx jest --ci", 1),
    ("go_test", "pass", "go test ./...", 0),
    ("go_test", "fail", "go test ./...", 1),
    ("go_build", "fail", "go build ./...", 1),
    ("deno_test", "pass", "deno test", 0),
    ("deno_test", "fail", "deno test", 1),
    ("bun_test", "pass", "bun test", 0),
    ("bun_test", "fail", "bun test", 1),
    ("tsc", "pass", "npx tsc --noEmit a.ts", 0),
    ("tsc", "fail", "npx tsc --noEmit a.ts", 2),
    ("npm", "pass", "npm install request@2.88.2", 0),
    ("pnpm", "pass", "pnpm install", 0),
    ("pnpm", "fail", "pnpm install", 1),
    ("uv", "pass", "uv pip install requests", 0),
];

fn fixture(tool: &str, case: &str) -> String {
    let p = format!(
        "{}/tests/fixtures/{tool}/{case}.txt",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

#[test]
fn every_fixture_dir_is_covered() {
    let dir = format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"));
    for e in std::fs::read_dir(dir).unwrap() {
        let tool = e.unwrap().file_name().to_string_lossy().into_owned();
        assert!(
            CASES.iter().any(|c| c.0 == tool),
            "fixture dir `{tool}` has no entry in CASES"
        );
    }
}

#[test]
fn snapshots() {
    let cfg = Config::default();
    for &(tool, case, cmdline, exit) in CASES {
        let raw = fixture(tool, case);
        let argv: Vec<String> = cmdline.split_whitespace().map(String::from).collect();
        let cmd = Cmd::from_argv(&argv).unwrap();
        let filter = filters::find(&cmd, &|_| true);
        assert!(
            filter.is_some(),
            "{tool}/{case}: no filter detects `{cmdline}`"
        );
        let c = compact(filter, &raw, exit, &cfg);
        // Same result with Windows line endings.
        let crlf = compact(filter, &raw.replace('\n', "\r\n"), exit, &cfg);
        assert_eq!(c.text, crlf.text, "{tool}/{case}: CRLF changes the result");
        assert!(
            c.text.chars().count() <= raw.chars().count(),
            "{tool}/{case}: output grew"
        );
        // What the specific filter produced, before the runner's
        // "never longer than the input" fallback.
        let direct = filter
            .unwrap()
            .filter(&hk::text::clean(&raw), exit, &cfg.limits)
            .unwrap_or_else(|| "<none: falls back to generic>".into());
        let report = format!(
            "used: {}\nraw chars: {}\ncompact chars: {}\n--- final output\n{}\n--- {} filter output\n{}",
            c.filter,
            raw.chars().count(),
            c.text.chars().count(),
            c.text,
            filter.unwrap().name(),
            direct
        );
        insta::assert_snapshot!(format!("{tool}__{case}"), report);
    }
}

#[test]
fn generic_truncates_long_output() {
    let cfg = Config::default();
    let raw: String = (1..=500)
        .map(|i| format!("\u{1b}[32mline {i}\u{1b}[0m\n"))
        .collect();
    let c = compact(None, &raw, 0, &cfg);
    assert_eq!(c.filter, "generic");
    assert!(c.changed);
    assert_eq!(c.text.lines().count(), 61);
    assert!(c.text.contains("[hk] ... 440 lines omitted ..."));
    assert!(c.text.starts_with("line 1\n"));
    assert!(c.text.ends_with("line 500"));
}

#[test]
fn generic_keeps_short_output() {
    let c = compact(None, "hello\nworld\n", 0, &Config::default());
    assert_eq!(c.text, "hello\nworld");
    assert!(!c.changed);
}

#[test]
fn unrecognised_failure_falls_back_to_generic() {
    let argv: Vec<String> = ["cargo", "test"].iter().map(|s| s.to_string()).collect();
    let f = filters::find(&Cmd::from_argv(&argv).unwrap(), &|_| true);
    let c = compact(
        f,
        "Segmentation fault (core dumped)\n",
        101,
        &Config::default(),
    );
    assert_eq!(c.filter, "generic");
    assert!(c.text.contains("Segmentation fault"));
}
