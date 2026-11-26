//! `hk log` (read part of a saved log without opening the whole file or
//! quoting `~/Library/Application Support`) and `hk capture` (turn a log
//! into an anonymised fixture).

use std::path::{Path, PathBuf};

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

/// Logs, newest first, optionally of one session.
pub fn list(session: Option<&str>) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(crate::paths::logs_dir()) else {
        return Vec::new();
    };
    let tag = session.map(|s| format!("-{s}-"));
    let mut v: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "log"))
        .filter(|p| {
            tag.as_ref().is_none_or(|t| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.contains(t.as_str()))
            })
        })
        .collect();
    v.sort();
    v.reverse();
    v
}

/// Parse `A:B`, `A:` or `:B` (1-based, inclusive).
fn range(s: &str, n: usize) -> Option<(usize, usize)> {
    let (a, b) = s.split_once(':')?;
    let a = if a.is_empty() { 1 } else { a.parse().ok()? };
    let b = if b.is_empty() { n } else { b.parse().ok()? };
    Some((a.max(1), b.min(n)))
}

pub fn run(args: &[String]) -> i32 {
    let session = flag(args, "--session");
    let sub = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .map(String::as_str)
        .unwrap_or("last");
    let logs = list(session.as_deref());
    match sub {
        "list" => {
            for p in logs.iter().take(20) {
                let when = crate::run::log_time(p).unwrap_or(0);
                let cmd = std::fs::read_to_string(p)
                    .ok()
                    .and_then(|s| {
                        s.lines()
                            .find_map(|l| l.strip_prefix("[hk] command: ").map(String::from))
                    })
                    .unwrap_or_default();
                println!("{when}  {}  {cmd}", p.display());
            }
            0
        }
        "path" => match logs.first() {
            Some(p) => {
                println!("{}", p.display());
                0
            }
            None => {
                eprintln!("hk: no logs yet");
                1
            }
        },
        "last" => {
            let Some(p) = logs.first() else {
                eprintln!("hk: no logs yet");
                return 1;
            };
            print!(
                "{}",
                show(
                    p,
                    flag(args, "--grep").as_deref(),
                    flag(args, "--lines").as_deref()
                )
            );
            0
        }
        other => {
            // A path or a file name from `[hk] log:`.
            let p = Path::new(other);
            let p = if p.is_file() {
                p.to_path_buf()
            } else {
                crate::paths::logs_dir().join(other)
            };
            if !p.is_file() {
                eprintln!("hk: no such log: {other}");
                return 1;
            }
            print!(
                "{}",
                show(
                    &p,
                    flag(args, "--grep").as_deref(),
                    flag(args, "--lines").as_deref()
                )
            );
            0
        }
    }
}

/// The log's header, then the selected lines with their numbers.
pub fn show(path: &Path, grep: Option<&str>, lines: Option<&str>) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    let mut out = format!("[hk] {}\n", path.display());
    let (a, b) = lines
        .and_then(|r| range(r, all.len()))
        .unwrap_or((1, all.len()));
    let mut shown = 0;
    for (i, l) in all.iter().enumerate() {
        let n = i + 1;
        if n < a || n > b {
            continue;
        }
        if grep.is_some_and(|g| !l.contains(g)) {
            continue;
        }
        out.push_str(&format!("{n:>6}  {l}\n"));
        shown += 1;
        if shown >= 400 && lines.is_none() {
            out.push_str("[hk] stopped at 400 lines; narrow with --lines A:B or --grep\n");
            break;
        }
    }
    out
}

/// `hk capture <log> <tool> <case>`: strip the hk header, mask secrets,
/// replace the home directory and user name, and write
/// `crates/hk/tests/fixtures/<tool>/<case>.txt` under the current directory.
pub fn capture(args: &[String]) -> i32 {
    let (Some(log), Some(tool), Some(case)) = (args.first(), args.get(1), args.get(2)) else {
        eprintln!("usage: hk capture <log> <tool> <case>");
        return 2;
    };
    let Ok(text) = std::fs::read_to_string(log) else {
        eprintln!("hk: cannot read {log}");
        return 1;
    };
    let mut command = String::new();
    let mut exit = String::from("0");
    let home = std::env::var("HOME").unwrap_or_default();
    let user = std::env::var("USER").unwrap_or_default();
    let mut body = Vec::new();
    for l in text.lines() {
        if let Some(c) = l.strip_prefix("[hk] command: ") {
            command = c.to_string();
            continue;
        }
        if let Some(e) = l.strip_prefix("[hk] exit code: ") {
            exit = e.trim().to_string();
            continue;
        }
        if l.starts_with("[hk] cwd: ") {
            continue;
        }
        body.push(anonymise(&crate::redact::line(l), &home, &user));
    }
    while body.first().is_some_and(|l| l.is_empty()) {
        body.remove(0);
    }
    while body.last().is_some_and(|l| l.is_empty()) {
        body.pop();
    }
    let dir = crate::paths::project_root(&std::env::current_dir().unwrap_or_default())
        .join("crates/hk/tests/fixtures")
        .join(tool);
    if std::fs::create_dir_all(&dir).is_err() {
        eprintln!("hk: cannot create {}", dir.display());
        return 1;
    }
    let dest = dir.join(format!("{case}.txt"));
    if std::fs::write(&dest, body.join("\n") + "\n").is_err() {
        eprintln!("hk: cannot write {}", dest.display());
        return 1;
    }
    let command = anonymise(&command, &home, &user);
    println!("wrote {}", dest.display());
    println!("add to CASES in tests/fixtures.rs:");
    println!("    ({tool:?}, {case:?}, {command:?}, {exit}),");
    println!("then review the anonymised text before committing it.");
    0
}

fn anonymise(s: &str, home: &str, user: &str) -> String {
    let mut s = s.to_string();
    if home.len() > 1 {
        s = s.replace(home, "/home/user");
    }
    if user.len() > 2 {
        s = s.replace(user, "user");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ranges() {
        assert_eq!(range("3:5", 10), Some((3, 5)));
        assert_eq!(range(":4", 10), Some((1, 4)));
        assert_eq!(range("8:", 10), Some((8, 10)));
        assert_eq!(range("x", 10), None);
    }

    #[test]
    fn shows_slices() {
        let p = std::env::temp_dir().join(format!("hk-logcmd-{}.log", std::process::id()));
        std::fs::write(&p, "a\nerror one\nb\nerror two\n").unwrap();
        let g = show(&p, Some("error"), None);
        assert!(g.contains("     2  error one") && g.contains("     4  error two"));
        assert!(!g.contains("  a\n"));
        let r = show(&p, None, Some("3:3"));
        assert!(r.contains("     3  b") && !r.contains("error"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn anonymises_paths() {
        assert_eq!(
            anonymise("/Users/jdoe/x by jdoe", "/Users/jdoe", "jdoe"),
            "/home/user/x by user"
        );
    }
}
