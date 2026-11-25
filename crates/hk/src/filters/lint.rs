//! Linters run directly: `cargo clippy`, `eslint`, `ruff check`,
//! `biome check|lint|ci`, `mypy`, `pyright`, `golangci-lint run`.
//!
//! Output: a count line, then one line per issue as `file:line: msg`,
//! de-duplicated (`(xN)`), errors before warnings, capped at
//! `limits.max_errors` with `+N more`. Machine-readable output formats
//! (`--output-format json`, `-f json`, `--outputjson`...) are never wrapped:
//! a script is parsing them.

use super::build::{self, dedup_count};
use super::{Cmd, Filter};
use crate::config::Limits;
use crate::text::push_capped;

pub struct Lint;

impl Filter for Lint {
    fn name(&self) -> &'static str {
        "lint"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        let sub = cmd.subcommand();
        let owned = match cmd.program.as_str() {
            "cargo" => sub == Some("clippy"),
            "eslint" | "mypy" | "dmypy" | "pyright" | "basedpyright" => true,
            "ruff" => sub == Some("check"),
            "biome" => matches!(sub, Some("check" | "lint" | "ci")),
            "golangci-lint" => sub == Some("run"),
            _ => false,
        };
        owned && !machine_format(cmd)
    }

    fn sniff(&self, output: &str) -> bool {
        let i = issues(output);
        !i.errors.is_empty() || !i.warnings.is_empty()
    }

    fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String> {
        let i = issues(output);
        if i.errors.is_empty() && i.warnings.is_empty() {
            // Clean (exit 0), or a failure we do not recognise (config error,
            // crash): the generic filter keeps all of that.
            return (exit == 0).then(|| "✓ lint clean".to_string());
        }
        let (n_err, n_warn) = (i.errors.len(), i.warnings.len());
        let mut all = dedup_count(i.errors);
        all.extend(dedup_count(i.warnings));
        let mark = if exit == 0 { "✓" } else { "✗" };
        let mut out = vec![format!(
            "{mark} lint: {n_err} error(s), {n_warn} warning(s)"
        )];
        push_capped(&mut out, all, limits.max_errors);
        Some(out.join("\n"))
    }
}

/// True when the command asks for a non-human output format.
pub fn machine_format(cmd: &Cmd) -> bool {
    const FLAGS: &[&str] = &[
        "-f",
        "--format",
        "--output-format",
        "--out-format",
        "--reporter",
        "--formatter",
        "--message-format",
        "--error-format",
    ];
    const HUMAN: &[&str] = &[
        "stylish",
        "unix",
        "compact",
        "concise",
        "full",
        "text",
        "human",
        "pretty",
        "short",
        "default",
        "colored-line-number",
        "line-number",
        "tab",
    ];
    let a = &cmd.args;
    for (i, arg) in a.iter().enumerate() {
        if matches!(arg.as_str(), "--outputjson" | "--json" | "-o" | "--output") {
            return true;
        }
        for f in FLAGS {
            let value = if arg == f {
                a.get(i + 1).map(String::as_str)
            } else {
                arg.strip_prefix(f).and_then(|r| r.strip_prefix('='))
            };
            if let Some(v) = value {
                if !HUMAN.contains(&v) {
                    return true;
                }
            }
        }
    }
    false
}

#[derive(Debug, Default)]
pub struct Issues {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Extract issues from any of the supported linters' human formats.
pub fn issues(output: &str) -> Issues {
    let mut out = Issues::default();
    // rustc/clippy diagnostics have their own multi-line shape.
    let rust = build::rustc_diagnostics(output);
    out.errors.extend(rust.errors);
    out.warnings.extend(rust.warnings);

    let lines: Vec<&str> = output.lines().collect();
    let mut eslint_file: Option<&str> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        // ruff full format: `F401 [*] msg` then ` --> file:line:col`.
        if let Some(loc) = lines
            .get(i + 1)
            .and_then(|n| n.trim_start().strip_prefix("--> "))
        {
            if is_rule_code(t.split_whitespace().next().unwrap_or("")) {
                let msg = t.replacen(" [*]", "", 1);
                out.errors.push(format!("{}: {msg}", trim_col(loc.trim())));
                i += 2;
                continue;
            }
        }
        // biome: `file:line:col rule [FIXABLE] ━━━━` then `  × msg` / `  ! msg`.
        if t.contains("━━") {
            let mut parts = t.split_whitespace();
            let loc = parts.next().unwrap_or("");
            let rule = parts.next().unwrap_or("");
            if let Some(loc) = file_line(loc) {
                let body = lines[i + 1..]
                    .iter()
                    .map(|l| l.trim())
                    .find(|l| !l.is_empty());
                if let Some(b) = body {
                    let entry = |m: &str| format!("{loc}: {} ({rule})", m.trim());
                    if let Some(m) = b.strip_prefix("× ") {
                        out.errors.push(entry(m));
                    } else if let Some(m) = b.strip_prefix("! ") {
                        out.warnings.push(entry(m));
                    }
                }
            }
            i += 1;
            continue;
        }
        // eslint stylish: an unindented file path, then indented
        // `  line:col  error|warning  message  rule` rows.
        if !line.starts_with(char::is_whitespace) && !t.is_empty() && !t.contains(' ') {
            eslint_file = Some(t);
        }
        if line.starts_with(char::is_whitespace) {
            if let (Some(file), Some((ln, sev, msg))) = (eslint_file, eslint_row(t)) {
                let entry = format!("{file}:{ln}: {msg}");
                if sev == "error" {
                    out.errors.push(entry);
                } else {
                    out.warnings.push(entry);
                }
                i += 1;
                continue;
            }
        }
        // `file:line[:col]: msg` (ruff concise, mypy, golangci-lint, eslint
        // -f unix) and `file:line:col - error: msg` (pyright).
        if let Some((loc, msg)) = located(t) {
            if !is_rust_location(line) {
                let (sev, msg) = severity(msg);
                match sev {
                    Sev::Error => out.errors.push(format!("{loc}: {msg}")),
                    Sev::Warning => out.warnings.push(format!("{loc}: {msg}")),
                    Sev::Note => {}
                }
            }
        }
        i += 1;
    }
    out
}

/// rustc prints ` --> src/lib.rs:3:5` and `::: file` lines; already handled.
fn is_rust_location(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("--> ") || t.starts_with("::: ")
}

enum Sev {
    Error,
    Warning,
    Note,
}

fn severity(msg: &str) -> (Sev, &str) {
    let m = msg.trim();
    for (p, s) in [
        ("error: ", Sev::Error),
        ("warning: ", Sev::Warning),
        ("note: ", Sev::Note),
        ("information: ", Sev::Note),
    ] {
        if let Some(r) = m.strip_prefix(p) {
            return (s, r);
        }
    }
    (Sev::Error, m)
}

/// `F401`, `E501`, `PLR0913`, `UP006`: a ruff rule code.
fn is_rule_code(s: &str) -> bool {
    let letters = s.chars().take_while(|c| c.is_ascii_uppercase()).count();
    letters >= 1 && s.len() > letters && s[letters..].chars().all(|c| c.is_ascii_digit())
}

/// `a.ts:1:7` or `C:\x\a.ts:1:7` -> `a.ts:1`.
fn file_line(loc: &str) -> Option<String> {
    let (path, rest) = split_path(loc)?;
    let mut nums = rest.split(':');
    let ln = nums.next()?;
    if ln.is_empty() || !ln.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if !nums.all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    Some(format!("{path}:{ln}"))
}

/// Split `path:rest` at the first `:` that is not a Windows drive colon.
fn split_path(s: &str) -> Option<(&str, &str)> {
    let skip = if s.len() > 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic() {
        2
    } else {
        0
    };
    let idx = s[skip..].find(':')? + skip;
    let path = &s[..idx];
    if path.is_empty() || path.contains(char::is_whitespace) || !path.contains('.') {
        return None;
    }
    Some((path, &s[idx + 1..]))
}

/// Parse `file:line[:col]: msg` or `file:line:col - msg`.
fn located(t: &str) -> Option<(String, &str)> {
    let (path, rest) = split_path(t)?;
    let ln_end = rest.find(|c: char| !c.is_ascii_digit())?;
    if ln_end == 0 {
        return None;
    }
    let ln = &rest[..ln_end];
    let mut r = &rest[ln_end..];
    // optional `:col`
    if let Some(c) = r.strip_prefix(':') {
        let col_end = c.find(|ch: char| !ch.is_ascii_digit()).unwrap_or(c.len());
        if col_end > 0 {
            r = &c[col_end..];
        }
    }
    let msg = r
        .strip_prefix(": ")
        .or_else(|| r.strip_prefix(" - "))?
        .trim();
    if msg.is_empty() {
        return None;
    }
    Some((format!("{path}:{ln}"), msg))
}

/// `1:7   error    'a' is assigned a value but never used       no-unused-vars`
fn eslint_row(t: &str) -> Option<(&str, &str, String)> {
    let mut it = t.splitn(2, char::is_whitespace);
    let pos = it.next()?;
    let (ln, col) = pos.split_once(':')?;
    if ln.is_empty()
        || !ln.chars().all(|c| c.is_ascii_digit())
        || !col.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let rest = it.next()?.trim_start();
    let (sev, rest) = rest.split_once(char::is_whitespace)?;
    if sev != "error" && sev != "warning" {
        return None;
    }
    let rest = rest.trim();
    // The rule id is separated from the message by two or more spaces.
    let msg = match rest.rfind("  ") {
        Some(idx) => format!("{} ({})", rest[..idx].trim_end(), rest[idx..].trim()),
        None => rest.to_string(),
    };
    Some((ln, sev, msg))
}

/// `src/lib.rs:8:42` -> `src/lib.rs:8`
fn trim_col(loc: &str) -> String {
    file_line(loc).unwrap_or_else(|| loc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(s: &str) -> Cmd {
        let v: Vec<String> = s.split_whitespace().map(String::from).collect();
        Cmd::from_argv(&v).unwrap()
    }

    #[test]
    fn detects_and_skips_machine_formats() {
        assert!(Lint.detect(&cmd("ruff check .")));
        assert!(Lint.detect(&cmd("ruff check --output-format concise")));
        assert!(!Lint.detect(&cmd("ruff check --output-format json")));
        assert!(!Lint.detect(&cmd("ruff format")));
        assert!(Lint.detect(&cmd("npx eslint src")));
        assert!(!Lint.detect(&cmd("eslint -f json src")));
        assert!(!Lint.detect(&cmd("pyright --outputjson")));
        assert!(!Lint.detect(&cmd("cargo clippy --message-format=json")));
        assert!(Lint.detect(&cmd("golangci-lint run ./...")));
        assert!(Lint.detect(&cmd("biome check .")));
    }

    #[test]
    fn parses_line_formats() {
        let out = "\
  /w/a.py:3:5 - error: \"x\" is not defined (reportUndefinedVariable)
  /w/a.py:4:1 - warning: Import \"os\" is not accessed (reportUnusedImport)
main.go:12:2: ineffectual assignment to err (ineffassign)
C:\\w\\b.py:7: error: Name \"y\" is not defined  [name-defined]
b.py:9: note: See https://mypy.rtfd.io
";
        let i = issues(out);
        assert_eq!(
            i.errors,
            vec![
                "/w/a.py:3: \"x\" is not defined (reportUndefinedVariable)",
                "main.go:12: ineffectual assignment to err (ineffassign)",
                "C:\\w\\b.py:7: Name \"y\" is not defined  [name-defined]",
            ]
        );
        assert_eq!(i.warnings.len(), 1);
    }
}
