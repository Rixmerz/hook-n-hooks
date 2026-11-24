//! PostToolUse format + autofix of exactly one touched file.
//!
//! Each tool runs only if it is installed (project `node_modules/.bin` first
//! for JS tools, then `PATH`). Only the touched file is passed to tools. The
//! report contains a one-line "formatted" note (so Claude knows its view of
//! the file is stale) and the issues the tools could not fix, capped.

use crate::config::Limits;
use crate::text::{clean, push_capped};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn process(path: &Path, limits: &Limits) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if !path.is_file() {
        return None;
    }
    let before = std::fs::read(path).ok()?;
    let dir = path.parent().unwrap_or(Path::new("."));
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
                // Format after fixing, so removed imports leave no blank lines.
                run(
                    &ruff,
                    &["format".as_ref(), "--quiet".as_ref(), path.as_os_str()],
                    dir,
                );
                tools.push("ruff");
                if !ok {
                    issues.extend(lines(&out).into_iter().filter(|l| l.contains(':')));
                }
            }
        }
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "mts" | "cts" | "json" | "css" | "vue"
        | "svelte" => {
            js(path, dir, &ext, &mut tools, &mut issues);
        }
        _ => return None,
    }

    let changed = std::fs::read(path)
        .ok()
        .is_some_and(|after| after != before);
    let mut out = Vec::new();
    let name = path.display();
    if changed {
        out.push(format!(
            "hk: formatted {name} ({}); re-read before editing again",
            tools.join(", ")
        ));
    }
    if !issues.is_empty() {
        out.push(format!("hk: {} unfixed issue(s) in {name}:", issues.len()));
        push_capped(&mut out, issues, limits.max_errors);
    }
    (!out.is_empty()).then(|| out.join("\n"))
}

fn js(path: &Path, dir: &Path, ext: &str, tools: &mut Vec<&str>, issues: &mut Vec<String>) {
    let p = path.as_os_str();
    if let Some(biome) = which("biome", dir) {
        let (ok, out) = run(
            &biome,
            &[
                "check".as_ref(),
                "--write".as_ref(),
                "--reporter=summary".as_ref(),
                p,
            ],
            dir,
        );
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
    if let Some(prettier) = which("prettier", dir) {
        run(
            &prettier,
            &[
                "--write".as_ref(),
                "--log-level".as_ref(),
                "warn".as_ref(),
                p,
            ],
            dir,
        );
        tools.push("prettier");
    }
    let lintable = !matches!(ext, "json" | "css");
    if lintable {
        if let Some(eslint) = which("eslint", dir) {
            let (ok, out) = run(
                &eslint,
                &["--fix".as_ref(), "--format".as_ref(), "unix".as_ref(), p],
                dir,
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
