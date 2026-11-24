//! Build and typecheck output: `cargo build|check|clippy`, `tsc`, `go build|vet`.
//!
//! Output: one line per diagnostic as `file:line: msg`, de-duplicated, errors
//! before warnings, capped at `limits.max_errors` with `+N more`.

use super::{Cmd, Filter};
use crate::config::Limits;
use crate::text::push_capped;

pub struct Build;

impl Filter for Build {
    fn name(&self) -> &'static str {
        "build"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        match cmd.program.as_str() {
            "cargo" => matches!(
                cmd.subcommand(),
                Some("build" | "b" | "check" | "c" | "clippy")
            ),
            "tsc" | "vue-tsc" => !cmd.has_arg("--watch") && !cmd.has_arg("-w"),
            "go" => matches!(cmd.subcommand(), Some("build" | "vet")),
            _ => false,
        }
    }

    fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String> {
        let diags = diagnostics(output);
        if diags.errors.is_empty() && diags.warnings.is_empty() {
            if exit == 0 {
                return Some("✓ build ok".to_string());
            }
            // Failed, but nothing we recognise: let the generic filter keep it all.
            return None;
        }
        Some(diags.render(exit, limits.max_errors))
    }
}

#[derive(Debug, Default)]
pub struct Diagnostics {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl Diagnostics {
    pub fn render(self, exit: i32, cap: usize) -> String {
        let (n_err, n_warn) = (self.errors.len(), self.warnings.len());
        let errors = dedup_count(self.errors);
        let warnings = dedup_count(self.warnings);
        let mut out = Vec::new();
        let head = if exit == 0 {
            "✓ build ok"
        } else {
            "✗ build failed"
        };
        out.push(format!("{head}: {n_err} error(s), {n_warn} warning(s)"));
        let mut all = errors;
        all.extend(warnings);
        push_capped(&mut out, all, cap);
        out.join("\n")
    }
}

/// Order-preserving de-duplication that keeps a repeat count: `msg (x3)`.
fn dedup_count(items: Vec<String>) -> Vec<String> {
    let mut order: Vec<(String, usize)> = Vec::new();
    for it in items {
        match order.iter_mut().find(|(s, _)| *s == it) {
            Some((_, n)) => *n += 1,
            None => order.push((it, 1)),
        }
    }
    order
        .into_iter()
        .map(|(s, n)| if n > 1 { format!("{s} (x{n})") } else { s })
        .collect()
}

/// Extract rustc, tsc and go diagnostics from build output.
pub fn diagnostics(output: &str) -> Diagnostics {
    let mut d = Diagnostics::default();
    let lines: Vec<&str> = output.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        // rustc: `error[E0308]: msg` / `warning: msg`, location on a following ` --> ` line.
        if let Some((sev, msg)) = rustc_header(line) {
            let loc = lines[i + 1..]
                .iter()
                .take(3)
                .find_map(|l| l.trim_start().strip_prefix("--> "))
                .map(|l| trim_col(l.trim()));
            if let Some(loc) = loc {
                let entry = format!("{loc}: {sev}: {msg}");
                if sev.starts_with("error") {
                    d.errors.push(entry);
                } else {
                    d.warnings.push(entry);
                }
            } else if sev.starts_with("error") && !is_rustc_summary(msg) {
                d.errors.push(format!("{sev}: {msg}"));
            }
            i += 1;
            continue;
        }
        // tsc: `src/a.ts(1,7): error TS2322: msg` or pretty `src/a.ts:1:7 - error TS2322: msg`.
        if let Some(entry) = tsc_line(line) {
            if entry.contains(": warning") {
                d.warnings.push(entry);
            } else {
                d.errors.push(entry);
            }
            i += 1;
            continue;
        }
        // go: `./b.go:2:23: msg` (also used by vet).
        if let Some(entry) = go_line(line) {
            d.errors.push(entry);
        }
        i += 1;
    }
    d
}

fn rustc_header(line: &str) -> Option<(&str, &str)> {
    for sev in ["error", "warning"] {
        if let Some(rest) = line.strip_prefix(sev) {
            // `error[E0308]: msg` or `error: msg`
            let (code_end, rest2) = if rest.starts_with('[') {
                let close = rest.find(']')?;
                (sev.len() + close + 1, &rest[close + 1..])
            } else {
                (sev.len(), rest)
            };
            let msg = rest2.strip_prefix(": ")?;
            return Some((&line[..code_end], msg));
        }
    }
    None
}

fn is_rustc_summary(msg: &str) -> bool {
    msg.starts_with("could not compile")
        || msg.starts_with("aborting due to")
        || msg.contains("previous error")
        || msg.starts_with("test failed")
}

/// `src/lib.rs:8:42` -> `src/lib.rs:8`
fn trim_col(loc: &str) -> String {
    let parts: Vec<&str> = loc.rsplitn(3, ':').collect();
    if parts.len() == 3 && parts[0].chars().all(|c| c.is_ascii_digit()) {
        format!("{}:{}", parts[2], parts[1])
    } else {
        loc.to_string()
    }
}

fn tsc_line(line: &str) -> Option<String> {
    let l = line.trim();
    if let Some(idx) = l.find("): error TS").or_else(|| l.find("): warning TS")) {
        let open = l[..idx].rfind('(')?;
        let file = &l[..open];
        let line_no = l[open + 1..idx].split(',').next()?;
        let msg = &l[idx + 3..];
        return Some(format!("{file}:{line_no}: {msg}"));
    }
    if let Some(idx) = l.find(" - error TS") {
        let loc = trim_col(&l[..idx]);
        return Some(format!("{loc}: {}", &l[idx + 3..]));
    }
    None
}

fn go_line(line: &str) -> Option<String> {
    // Indented `file.go:4: msg` lines are `t.Errorf` output, not compiler errors.
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let mut parts = line.splitn(4, ':');
    let file = parts.next()?;
    let ln = parts.next()?;
    let third = parts.next()?;
    if !file.ends_with(".go") || ln.is_empty() || !ln.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let msg = if third.chars().all(|c| c.is_ascii_digit()) {
        parts.next()?.trim()
    } else {
        third.trim()
    };
    Some(format!("{}:{ln}: {msg}", file.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_columns() {
        assert_eq!(trim_col("src/lib.rs:8:42"), "src/lib.rs:8");
        assert_eq!(trim_col("C:\\x\\lib.rs:8:42"), "C:\\x\\lib.rs:8");
    }

    #[test]
    fn caps_diagnostics() {
        let out: String = (0..30)
            .map(|i| format!("./a.go:{i}:1: undefined: x{i}\n"))
            .collect();
        let s = Build.filter(&out, 1, &Limits::default()).unwrap();
        assert!(s.contains("+10 more"));
        assert_eq!(s.lines().count(), 22);
    }
}
