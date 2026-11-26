//! Filter registry.
//!
//! A filter is a small module exposing a type that implements [`Filter`]:
//! `detect` decides from the command line whether it owns the command, and
//! `filter` turns the captured (ANSI-stripped) output into a compact summary.
//! Returning `None` from `filter` means "I could not recognise this output";
//! the runner then falls back to the generic head/tail filter, so a filter
//! never has to be perfect to be safe.
//!
//! To add a tool: create a module, implement `Filter`, add it to [`registry`],
//! and add fixtures under `tests/fixtures/<tool>/`. See CONTRIBUTING.md.

use crate::config::Limits;

pub mod angular;
pub mod build;
pub mod cargo_test;
pub mod deno_bun_test;
pub mod docker;
pub mod dotnet;
pub mod generic;
pub mod git;
pub mod go_test;
pub mod install;
pub mod js_test;
pub mod json;
pub mod jvm;
pub mod lint;
pub mod listing;
pub mod logs;
pub mod pytest;
pub mod script;
pub mod table;
pub mod test_runners;

/// A parsed command line: the effective program and its arguments, after
/// removing wrappers such as `npx`, `uv run`, `python -m` or `pnpm exec`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
}

impl Cmd {
    pub fn from_argv(argv: &[String]) -> Option<Cmd> {
        let mut rest: &[String] = argv;
        loop {
            let (first, tail) = rest.split_first()?;
            let prog = program_name(first);
            let next = tail.first().map(String::as_str);
            rest = match (prog.as_str(), next) {
                ("npx" | "bunx" | "pnpx", _) => skip_flags(tail),
                ("pnpm" | "yarn" | "npm" | "bun", Some("exec" | "x")) => skip_flags(&tail[1..]),
                ("uv" | "poetry" | "pipenv" | "hatch" | "rye", Some("run")) => {
                    skip_flags(&tail[1..])
                }
                ("python" | "python3" | "py", Some("-m")) => &tail[1..],
                ("bundle", Some("exec")) => skip_flags(&tail[1..]),
                // `hide exec NAME... -- 'cmd args'`: the inner command decides.
                ("hide", Some("exec")) => {
                    let i = tail.iter().position(|a| a == "--")?;
                    let inner = &tail[i + 1..];
                    if let [one] = inner {
                        if one.contains(char::is_whitespace) {
                            let words = crate::shell::split_words(one)?;
                            return Cmd::from_argv(&words);
                        }
                    }
                    inner
                }
                ("env" | "time" | "nice", _) => {
                    let n = tail
                        .iter()
                        .take_while(|a| a.starts_with('-') || a.contains('='))
                        .count();
                    &tail[n..]
                }
                _ => {
                    return Some(Cmd {
                        program: prog,
                        args: tail.to_vec(),
                    })
                }
            };
        }
    }

    pub fn arg(&self, i: usize) -> Option<&str> {
        self.args.get(i).map(String::as_str)
    }

    pub fn has_arg(&self, a: &str) -> bool {
        self.args.iter().any(|x| x == a)
    }

    /// First argument that is not a flag (the subcommand for cargo/go/npm...).
    pub fn subcommand(&self) -> Option<&str> {
        self.args
            .iter()
            .map(String::as_str)
            .find(|a| !a.starts_with('-') && !a.starts_with('+'))
    }
}

fn skip_flags(args: &[String]) -> &[String] {
    let n = args.iter().take_while(|a| a.starts_with('-')).count();
    &args[n..]
}

/// `C:\tools\cargo.exe` -> `cargo`, `./node_modules/.bin/jest` -> `jest`.
pub fn program_name(p: &str) -> String {
    let base = p.rsplit(['/', '\\']).next().unwrap_or(p);
    let lower = base.to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".ps1"] {
        if let Some(s) = lower.strip_suffix(ext) {
            return s.to_string();
        }
    }
    lower
}

pub trait Filter: Sync {
    /// Stable name used in stats, logs and `[filters]` in hk.toml.
    fn name(&self) -> &'static str;
    fn detect(&self, cmd: &Cmd) -> bool;
    fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String>;
    /// True when `output` carries this tool's signature (a summary line, a
    /// diagnostic format). Used by the `script` filter, which runs behind
    /// `npm run`, `make` and friends and cannot tell from the command line.
    fn sniff(&self, _output: &str) -> bool {
        false
    }
}

/// All specific filters, in priority order. The generic filter is not listed:
/// it is the fallback the runner applies itself.
pub fn registry() -> &'static [&'static dyn Filter] {
    &[
        &cargo_test::CargoTest,
        &pytest::Pytest,
        &js_test::JsTest,
        &go_test::GoTest,
        &deno_bun_test::DenoBunTest,
        &test_runners::Rspec,
        &test_runners::Phpunit,
        &test_runners::MixTest,
        &test_runners::Playwright,
        &angular::Angular,
        &jvm::Jvm,
        &dotnet::Dotnet,
        &build::Build,
        &lint::Lint,
        &install::Install,
        &git::Git,
        &docker::Docker,
        &logs::Logs,
        &listing::Listing,
        &json::Json,
        &table::Table,
        &script::Script,
    ]
}

/// Find the filter that owns `cmd`, honouring per-filter toggles.
pub fn find(cmd: &Cmd, enabled: &dyn Fn(&str) -> bool) -> Option<&'static dyn Filter> {
    registry()
        .iter()
        .copied()
        .find(|f| enabled(f.name()) && f.detect(cmd))
}

pub fn by_name(name: &str) -> Option<&'static dyn Filter> {
    registry().iter().copied().find(|f| f.name() == name)
}

/// True when the command runs in watch/interactive mode and must not be
/// wrapped (the wrapper buffers output until exit).
pub fn is_watch_mode(cmd: &Cmd) -> bool {
    // Bare `vitest` (no `run`) is watch mode in a TTY; be conservative.
    let bare_vitest =
        cmd.program == "vitest" && !cmd.has_arg("run") && !cmd.args.iter().any(|a| a == "--run");
    is_explicit_watch(cmd) || bare_vitest
}

/// Watch/serve flags and subcommands. Unlike [`is_watch_mode`], bare `vitest`
/// is allowed: behind a package script (`"test": "vitest"`) with no TTY and
/// no `--watch`, vitest runs once and exits (checked with vitest 5).
pub fn is_explicit_watch(cmd: &Cmd) -> bool {
    let watchy = cmd.args.iter().any(|a| {
        a == "--watch" || a == "-w" || a.starts_with("--watch=") || a == "--watchAll" || a == "--ui"
    });
    let bare_dev = matches!(cmd.subcommand(), Some("dev" | "serve" | "start" | "watch"));
    watchy || bare_dev
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(s: &str) -> Cmd {
        let v: Vec<String> = s.split_whitespace().map(String::from).collect();
        Cmd::from_argv(&v).unwrap()
    }

    #[test]
    fn unwraps_runners() {
        assert_eq!(cmd("npx vitest run").program, "vitest");
        assert_eq!(cmd("uv run pytest -q").program, "pytest");
        assert_eq!(cmd("python3 -m pytest").program, "pytest");
        assert_eq!(cmd("pnpm exec jest").program, "jest");
        assert_eq!(cmd("C:\\bin\\cargo.exe test").program, "cargo");
    }

    #[test]
    fn detects_watch_mode() {
        assert!(is_watch_mode(&cmd("vitest")));
        assert!(!is_watch_mode(&cmd("vitest run")));
        assert!(is_watch_mode(&cmd("jest --watch")));
        assert!(!is_watch_mode(&cmd("cargo test")));
    }

    #[test]
    fn registry_routes_commands() {
        let all = |_: &str| true;
        let name = |s: &str| find(&cmd(s), &all).map(|f| f.name());
        assert_eq!(name("cargo test --lib"), Some("cargo_test"));
        assert_eq!(name("cargo build --release"), Some("build"));
        assert_eq!(name("pytest -x"), Some("pytest"));
        assert_eq!(name("npx vitest run"), Some("js_test"));
        assert_eq!(name("go test ./..."), Some("go_test"));
        assert_eq!(name("deno test"), Some("deno_bun_test"));
        assert_eq!(name("npx tsc --noEmit"), Some("build"));
        assert_eq!(name("pnpm install"), Some("install"));
        assert_eq!(name("ls -la"), None);
        let no_pytest = |n: &str| n != "pytest";
        assert!(find(&cmd("pytest"), &no_pytest).is_none());
    }
}
