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
    // v1.0: script wrappers (output signature decides the filter)
    ("npm_test", "pass", "npm test", 0),
    ("npm_run_lint", "fail", "npm run lint", 1),
    ("make_test", "pass", "make test", 0),
    // v1.0: linters
    ("clippy", "fail", "cargo clippy", 101),
    ("ruff", "fail", "ruff check .", 1),
    ("ruff", "concise", "ruff check --output-format concise .", 1),
    ("ruff", "pass", "ruff check .", 0),
    ("mypy", "fail", "mypy a.py b.py", 1),
    ("eslint", "fail", "npx eslint .", 1),
    ("biome", "fail", "npx biome lint .", 1),
    ("pyright", "fail", "pyright", 1),
    ("golangci_lint", "fail", "golangci-lint run ./...", 1),
    // v1.0: git
    ("git_diff", "long", "git diff 1d914f7 f3a035f", 0),
    ("git_show", "long", "git show f3a035f", 0),
    ("git_log", "long", "git log", 0),
    // v1.0: docker
    ("docker_build", "pass", "docker build -t app .", 0),
    ("docker_build", "fail", "docker build -t app .", 1),
    (
        "docker_build",
        "buildkit_fail",
        "docker buildx build -t api .",
        1,
    ),
    ("docker_compose", "up", "docker compose up -d --build", 0),
    // v1.0: other ecosystems
    ("mvn", "test_pass", "mvn -B test", 0),
    ("mvn", "test_fail", "mvn -B test", 1),
    ("mvn", "compile_fail", "mvn -B compile", 1),
    ("gradle", "fail", "./gradlew test", 1),
    ("gradle", "compile_fail", "./gradlew build", 1),
    ("dotnet_build", "fail", "dotnet build", 1),
    ("dotnet_test", "fail", "dotnet test", 1),
    ("rspec", "fail", "bundle exec rspec", 1),
    ("phpunit", "fail", "vendor/bin/phpunit", 2),
    ("mix_test", "fail", "mix test", 2),
    (
        "playwright",
        "fail",
        "npx playwright test --reporter=list",
        1,
    ),
    // v1.0: listings
    ("find", "long", "find .", 0),
    ("ls_r", "long", "ls -R", 0),
    ("du", "long", "du -h .", 0),
    ("tree", "long", "tree -L 3", 0),
    // v1.0: logs and network
    ("docker_logs", "long", "docker logs worker", 0),
    ("kubectl_logs", "long", "kubectl logs deploy/api", 0),
    ("journalctl", "long", "journalctl -u api --no-pager", 0),
    ("curl", "verbose", "curl -v https://example.com", 0),
];

/// Fixtures written by hand, in the tool's exact format, because the tool
/// was not installed where the fixtures were captured. Every other fixture
/// is a real captured output (paths and author names anonymised).
const CRAFTED: &[(&str, &str)] = &[
    ("pyright", "fail"),
    ("golangci_lint", "fail"),
    ("docker_build", "buildkit_fail"),
    ("docker_compose", "up"),
    ("gradle", "fail"),
    ("gradle", "compile_fail"),
    ("dotnet_build", "fail"),
    ("dotnet_test", "fail"),
    ("rspec", "fail"),
    ("phpunit", "fail"),
    ("mix_test", "fail"),
    ("playwright", "fail"),
    ("tree", "long"),
    ("kubectl_logs", "long"),
    ("journalctl", "long"),
];

#[test]
fn crafted_fixtures_are_listed_cases() {
    for c in CRAFTED {
        assert!(
            CASES.iter().any(|x| (x.0, x.1) == *c),
            "crafted fixture {c:?} has no entry in CASES"
        );
    }
}

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
        // Same choice as the hook; wrappers (`npm test`, `make test`) get `script`.
        let filter = hk::hook::choose_filter(&cmd, None, &cfg).and_then(filters::by_name);
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
