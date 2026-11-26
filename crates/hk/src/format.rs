//! PostToolUse format + autofix of exactly one touched file.
//!
//! Each tool runs only if it is installed (project `node_modules/.bin` first
//! for JS tools, then `PATH`). Only the touched file is passed to tools. The
//! report contains a one-line "formatted" note (so Claude knows its view of
//! the file is stale) and the issues the tools could not fix, capped.

use crate::config::{Config, Format};
use crate::text::{clean, push_capped};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What the edit touched, to tell its lines from the rest of the file.
pub enum Edited {
    /// `Write`: the whole file is the edit.
    Whole,
    /// `Edit`/`MultiEdit`: the new strings.
    Snippets(Vec<String>),
}

/// Path fragments never formatted: lockfiles, generated code, migrations,
/// fixtures and snapshots, whose exact bytes matter.
const ALWAYS_SKIP: &[&str] = &[
    "/migrations/",
    "/fixtures/",
    "/__snapshots__/",
    "/vendor/",
    "/node_modules/",
    "/generated/",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    ".min.js",
];

pub fn skipped(path: &Path, fmt: &Format) -> bool {
    let p = path.to_string_lossy().replace('\\', "/");
    ALWAYS_SKIP.iter().any(|s| p.contains(s)) || fmt.exclude.iter().any(|s| p.contains(s.as_str()))
}

/// Format (and, when enabled, autofix) one touched file. Returns the note
/// for Claude, if any.
pub fn process(path: &Path, cfg: &Config, edited: &Edited) -> Option<String> {
    let fmt = &cfg.format;
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if !path.is_file() || skipped(path, fmt) {
        return None;
    }
    let before = std::fs::read(path).ok()?;
    let dir = path.parent().unwrap_or(Path::new("."));
    // JS tools resolve `.prettierignore` and configs from the project root.
    let root = crate::paths::project_root(dir);
    let mut tools: Vec<&str> = Vec::new();
    let mut issues: Vec<String> = Vec::new();

    match ext.as_str() {
        "rs" => {
            if rustfmt_stdin(path, dir, &before) {
                tools.push("rustfmt");
            }
        }
        "go" => {
            if let Some(gofmt) = which("gofmt", dir) {
                let (ok, out) = run(&gofmt, &["-w".as_ref(), path.as_os_str()], dir);
                tools.push("gofmt");
                if !ok {
                    issues.extend(lines(&out));
                }
            }
        }
        "py" | "pyi" => {
            if let Some(ruff) = which("ruff", dir) {
                if fmt.fix {
                    let (ok, out) = run(
                        &ruff,
                        &[
                            "check".as_ref(),
                            "--fix".as_ref(),
                            "--quiet".as_ref(),
                            "--output-format".as_ref(),
                            "concise".as_ref(),
                            path.as_os_str(),
                        ],
                        dir,
                    );
                    if !ok {
                        issues.extend(lines(&out).into_iter().filter(|l| l.contains(':')));
                    }
                }
                // Format after fixing, so removed imports leave no blank lines.
                run(
                    &ruff,
                    &["format".as_ref(), "--quiet".as_ref(), path.as_os_str()],
                    dir,
                );
                tools.push("ruff");
            }
        }
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "mts" | "cts" | "json" | "css" | "scss"
        | "vue" | "svelte" | "html" => {
            js(path, &root, &ext, fmt, &mut tools, &mut issues);
        }
        _ => return None,
    }

    let after = std::fs::read(path).ok()?;
    let mut out = Vec::new();
    let name = path.display();
    if after != before {
        let outside = lines_outside_edit(&before, &after, edited);
        if outside > 0 && fmt.only_clean {
            // The file was not formatted before this edit: formatting it now
            // would rewrite lines nobody touched and dirty the diff.
            let _ = std::fs::write(path, &before);
            out.push(format!(
                "hk: not formatting {name}: {} would change {outside} line(s) outside your edit (format.only_clean)",
                tools.join(", ")
            ));
        } else if outside > 0 {
            out.push(format!(
                "hk: formatted {name} ({}), including {outside} line(s) outside your edit; re-read before editing again",
                tools.join(", ")
            ));
        } else {
            out.push(format!(
                "hk: formatted {name} ({}); re-read before editing again",
                tools.join(", ")
            ));
        }
    }
    if !issues.is_empty() {
        out.push(format!("hk: {} unfixed issue(s) in {name}:", issues.len()));
        push_capped(&mut out, issues, cfg.limits.max_errors);
    }
    (!out.is_empty()).then(|| out.join("\n"))
}

/// How many lines the formatter changed outside the edited lines. Compares
/// the common prefix and suffix of the two versions (a hull of the change,
/// which errs towards "outside") with the lines the new strings occupy.
pub fn lines_outside_edit(before: &[u8], after: &[u8], edited: &Edited) -> usize {
    let b = String::from_utf8_lossy(before);
    let a = String::from_utf8_lossy(after);
    let bl: Vec<&str> = b.lines().collect();
    let al: Vec<&str> = a.lines().collect();
    let pre = bl.iter().zip(&al).take_while(|(x, y)| x == y).count();
    let max_suf = bl.len().min(al.len()) - pre;
    let suf = bl
        .iter()
        .rev()
        .zip(al.iter().rev())
        .take(max_suf)
        .take_while(|(x, y)| x == y)
        .count();
    // Changed lines of the original: [pre, bl.len() - suf).
    let (lo, hi) = (pre, bl.len() - suf);
    if lo >= hi {
        // Pure insertion between two lines: inside if next to an edit.
        return usize::from(!near_edit(&b, lo, lo + 1, edited));
    }
    match edited {
        Edited::Whole => 0,
        Edited::Snippets(snips) => {
            let ranges = edit_ranges(&b, snips);
            (lo..hi)
                .filter(|l| !ranges.iter().any(|(s, e)| *l + 1 >= *s && *l <= *e + 1))
                .count()
        }
    }
}

fn near_edit(text: &str, lo: usize, hi: usize, edited: &Edited) -> bool {
    match edited {
        Edited::Whole => true,
        Edited::Snippets(s) => edit_ranges(text, s)
            .iter()
            .any(|(a, b)| hi + 1 >= *a && lo <= *b + 1),
    }
}

/// Line ranges (inclusive) where each snippet sits in `text`.
fn edit_ranges(text: &str, snips: &[String]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for s in snips {
        let s = s.trim_matches('\n');
        if s.is_empty() {
            continue;
        }
        let mut from = 0;
        while let Some(i) = text[from..].find(s) {
            let start = from + i;
            let first = text[..start].matches('\n').count();
            let last = first + s.matches('\n').count();
            out.push((first, last));
            from = start + s.len().max(1);
        }
    }
    out
}

fn js(
    path: &Path,
    root: &Path,
    ext: &str,
    fmt: &Format,
    tools: &mut Vec<&str>,
    issues: &mut Vec<String>,
) {
    let p = path.as_os_str();
    let dir = path.parent().unwrap_or(root);
    if let Some(biome) = which("biome", dir) {
        // `format` only changes layout; `check --write` also applies lint fixes.
        let args: Vec<&std::ffi::OsStr> = if fmt.fix {
            vec![
                "check".as_ref(),
                "--write".as_ref(),
                "--reporter=summary".as_ref(),
                p,
            ]
        } else {
            vec!["format".as_ref(), "--write".as_ref(), p]
        };
        let (ok, out) = run(&biome, &args, root);
        tools.push("biome");
        if !ok {
            issues.extend(
                lines(&out)
                    .into_iter()
                    .filter(|l| !l.trim().is_empty())
                    .take(40),
            );
        }
        return;
    }
    if let Some(prettierd) = which("prettierd", dir) {
        if prettierd_stdin(&prettierd, path, root) {
            tools.push("prettierd");
        }
    } else if let Some(prettier) = which("prettier", dir) {
        run(
            &prettier,
            &[
                "--write".as_ref(),
                "--log-level".as_ref(),
                "warn".as_ref(),
                p,
            ],
            root,
        );
        tools.push("prettier");
    }
    // eslint --fix can change behaviour, not only layout: opt-in.
    let lintable = !matches!(ext, "json" | "css" | "scss" | "html");
    if lintable && fmt.fix {
        let eslint = which("eslint_d", dir).or_else(|| which("eslint", dir));
        if let Some(eslint) = eslint {
            let (ok, out) = run(
                &eslint,
                &["--fix".as_ref(), "--format".as_ref(), "unix".as_ref(), p],
                root,
            );
            tools.push("eslint");
            if !ok {
                // `unix` format: `file:line:col: message [Error/rule]`.
                issues.extend(
                    lines(&out)
                        .into_iter()
                        .filter(|l| l.contains(':') && !l.ends_with("problems")),
                );
            }
        }
    }
}

/// prettierd reads the file on stdin and prints the result.
fn prettierd_stdin(prog: &Path, path: &Path, root: &Path) -> bool {
    use std::io::Write;
    let Ok(content) = std::fs::read(path) else {
        return false;
    };
    let child = Command::new(prog)
        .arg(path)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&content);
    }
    match child.wait_with_output() {
        Ok(o) if o.status.success() && !o.stdout.is_empty() => {
            o.stdout == content || std::fs::write(path, &o.stdout).is_ok()
        }
        _ => false,
    }
}

/// Format through stdin so rustfmt does not follow `mod foo;` into other files.
fn rustfmt_stdin(path: &Path, dir: &Path, before: &[u8]) -> bool {
    use std::io::Write;
    let Some(rustfmt) = which("rustfmt", dir) else {
        return false;
    };
    let edition = cargo_edition(dir).unwrap_or_else(|| "2021".into());
    let child = Command::new(&rustfmt)
        .args(["--edition", &edition, "--emit", "stdout", "--quiet"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(before);
    }
    let Ok(out) = child.wait_with_output() else {
        return false;
    };
    if out.status.success() && !out.stdout.is_empty() && out.stdout != before {
        return std::fs::write(path, &out.stdout).is_ok();
    }
    // Syntax errors are left for the compiler to report.
    out.status.success()
}

fn cargo_edition(dir: &Path) -> Option<String> {
    for d in dir.ancestors() {
        if let Ok(s) = std::fs::read_to_string(d.join("Cargo.toml")) {
            let v: toml::Value = toml::from_str(&s).ok()?;
            let ed = v
                .get("package")
                .and_then(|p| p.get("edition"))
                .and_then(|e| e.as_str())
                .map(String::from);
            if ed.is_some() {
                return ed;
            }
            // `edition.workspace = true`: keep walking up to the workspace root.
        }
    }
    None
}

fn run(prog: &Path, args: &[&std::ffi::OsStr], dir: &Path) -> (bool, String) {
    let mut cmd = if is_cmd_shim(prog) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(prog);
        c
    } else {
        Command::new(prog)
    };
    match cmd
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
    {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.success(), clean(&s))
        }
        Err(_) => (true, String::new()),
    }
}

fn lines(s: &str) -> Vec<String> {
    s.lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

fn is_cmd_shim(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

/// Locate an executable: `node_modules/.bin` in `dir` and its ancestors,
/// then `PATH` (with `PATHEXT` extensions on Windows). No subprocess.
pub fn which(name: &str, dir: &Path) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(|e| e.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    let try_dir = |d: &Path| -> Option<PathBuf> {
        exts.iter()
            .map(|e| d.join(format!("{name}{e}")))
            .find(|p| p.is_file())
    };
    for d in dir.ancestors() {
        if let Some(p) = try_dir(&d.join("node_modules").join(".bin")) {
            return Some(p);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|d| try_dir(&d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_lines_outside_the_edit() {
        let before = b"a = 1\nb  =  2\nc = 3\nd  =  4\n";
        let only_b = b"a = 1\nb = 2\nc = 3\nd  =  4\n";
        let b_and_d = b"a = 1\nb = 2\nc = 3\nd = 4\n";
        let snip = Edited::Snippets(vec!["b  =  2".into()]);
        assert_eq!(lines_outside_edit(before, only_b, &snip), 0);
        assert!(lines_outside_edit(before, b_and_d, &snip) > 0);
        assert_eq!(lines_outside_edit(before, b_and_d, &Edited::Whole), 0);
    }

    #[test]
    fn skips_sensitive_paths() {
        let f = Format::default();
        assert!(skipped(Path::new("/r/db/migrations/001.ts"), &f));
        assert!(skipped(Path::new("/r/package-lock.json"), &f));
        assert!(!skipped(Path::new("/r/src/a.ts"), &f));
    }
}
