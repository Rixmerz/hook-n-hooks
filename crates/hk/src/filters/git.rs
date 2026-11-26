//! `git diff`, `git show` and unbounded `git log`.
//!
//! Short output passes through untouched. Long output becomes a stat summary
//! (computed from the diff text itself, git is not run twice) plus the head
//! of the diff, or the first commits of the log. Everything else is in the
//! full log, whose path `hk run` prints first.
//!
//! Output that a script may parse is never wrapped: `--porcelain`, `-z`,
//! `--format`, `--pretty=format:`, `--stat`/`--numstat`/`--name-only` and
//! friends, `--quiet`/`--exit-code`, and `git log -n N`.

use super::{Cmd, Filter};
use crate::config::Limits;
use crate::text::push_capped;

pub struct Git;

impl Filter for Git {
    fn name(&self) -> &'static str {
        "git"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        if cmd.program != "git" {
            return false;
        }
        let Some((sub, rest)) = subcommand(&cmd.args) else {
            return false;
        };
        if !matches!(sub, "diff" | "log" | "show") {
            return false;
        }
        let scripted = rest.iter().any(|a| {
            let a = a.as_str();
            matches!(
                a,
                "-z" | "--porcelain"
                    | "--stat"
                    | "--numstat"
                    | "--shortstat"
                    | "--dirstat"
                    | "--name-only"
                    | "--name-status"
                    | "--raw"
                    | "--quiet"
                    | "--exit-code"
                    | "--check"
                    | "--summary"
                    | "--no-patch"
                    | "-s"
                    | "--graph"
            ) || a.starts_with("--format")
                || a.starts_with("--pretty=format:")
                || a.starts_with("--pretty=tformat:")
                || a.starts_with("--stat=")
                || a.starts_with("--output")
                || a.starts_with("--ext-diff")
        });
        if scripted {
            return false;
        }
        match sub {
            "log" => !rest.iter().any(|a| {
                a == "-n"
                    || a.starts_with("--max-count")
                    || (a.starts_with("-n") && a[2..].chars().all(|c| c.is_ascii_digit()))
                    || (a.len() > 1
                        && a.starts_with('-')
                        && a[1..].chars().all(|c| c.is_ascii_digit()))
            }),
            // `git show rev:path` prints a file, not a commit.
            "show" => !rest.iter().any(|a| !a.starts_with('-') && a.contains(':')),
            _ => true,
        }
    }

    fn filter(&self, output: &str, _exit: i32, limits: &Limits) -> Option<String> {
        let lines: Vec<&str> = output.lines().collect();
        if lines.len() <= limits.generic_max_lines {
            return None; // short: the generic filter passes it through as is
        }
        if lines.iter().any(|l| l.starts_with("diff --git ")) {
            Some(diff_summary(&lines, limits))
        } else if lines.first().is_some_and(|l| l.starts_with("commit ")) {
            Some(log_summary(&lines, limits))
        } else {
            None
        }
    }
}

/// Skip git's global options (`-C dir`, `-c k=v`, `--no-pager`...) and return
/// the subcommand and its arguments.
fn subcommand(args: &[String]) -> Option<(&str, &[String])> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" => i += 2,
            _ if a.starts_with('-') => i += 1,
            _ => return Some((a, &args[i + 1..])),
        }
    }
    None
}

struct FileStat {
    path: String,
    added: usize,
    removed: usize,
}

fn diff_summary(lines: &[&str], limits: &Limits) -> String {
    let first_diff = lines
        .iter()
        .position(|l| l.starts_with("diff --git "))
        .unwrap_or(0);
    let mut files: Vec<FileStat> = Vec::new();
    for l in &lines[first_diff..] {
        if let Some(rest) = l.strip_prefix("diff --git ") {
            // `a/x b/x`: keep the `b/` side.
            let path = rest
                .rsplit_once(" b/")
                .map(|(_, p)| p.to_string())
                .unwrap_or_else(|| rest.to_string());
            files.push(FileStat {
                path,
                added: 0,
                removed: 0,
            });
        } else if let Some(f) = files.last_mut() {
            if l.starts_with('+') && !l.starts_with("+++ ") {
                f.added += 1;
            } else if l.starts_with('-') && !l.starts_with("--- ") {
                f.removed += 1;
            }
        }
    }
    let (add, del) = files
        .iter()
        .fold((0, 0), |(a, d), f| (a + f.added, d + f.removed));
    let mut out: Vec<String> = Vec::new();
    // `git show`: keep the commit header (hash, author, date, subject).
    let header: Vec<&str> = lines[..first_diff]
        .iter()
        .copied()
        .filter(|l| !l.trim().is_empty())
        .collect();
    let keep = header.len().min(limits.head / 3).max(1);
    if !header.is_empty() {
        out.extend(
            header[..keep.min(header.len())]
                .iter()
                .map(|s| s.to_string()),
        );
        if header.len() > keep {
            out.push(format!(
                "[hk] ... {} more header lines",
                header.len() - keep
            ));
        }
    }
    out.push(format!("{} file(s) changed, +{add} -{del}", files.len()));
    let stats = files
        .iter()
        .map(|f| format!(" {} | +{} -{}", f.path, f.added, f.removed))
        .collect();
    push_capped(&mut out, stats, limits.max_errors);
    let diff = &lines[first_diff..];
    let head = limits.head.min(diff.len());
    out.push(format!("--- first {head} of {} diff lines", diff.len()));
    out.extend(diff[..head].iter().map(|s| s.to_string()));
    if diff.len() > head {
        out.push(format!(
            "[hk] ... {} more diff lines in the full log",
            diff.len() - head
        ));
    }
    out.join("\n")
}

fn log_summary(lines: &[&str], limits: &Limits) -> String {
    // Split into commits at `commit <hash>` lines.
    let mut commits: Vec<&[&str]> = Vec::new();
    let mut start = 0;
    for (i, l) in lines.iter().enumerate() {
        if i > 0 && l.starts_with("commit ") {
            commits.push(&lines[start..i]);
            start = i;
        }
    }
    commits.push(&lines[start..]);
    let budget = limits.generic_max_lines / 2;
    let mut out: Vec<String> = Vec::new();
    let mut shown = 0;
    for c in &commits {
        if shown > 0 && out.len() + c.len() > budget {
            break;
        }
        let take = c.len().min(budget);
        out.extend(c[..take].iter().map(|s| s.to_string()));
        if take < c.len() {
            out.push(format!(
                "[hk] ... {} more lines in this commit",
                c.len() - take
            ));
        }
        shown += 1;
    }
    if shown < commits.len() {
        out.push(format!(
            "[hk] ... {} more commit(s) not shown ({} total); the full log has them all",
            commits.len() - shown,
            commits.len()
        ));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(s: &str) -> bool {
        let v: Vec<String> = s.split_whitespace().map(String::from).collect();
        Git.detect(&Cmd::from_argv(&v).unwrap())
    }

    #[test]
    fn leaves_scripted_git_alone() {
        assert!(detect("git diff"));
        assert!(detect("git -C repo --no-pager diff --cached"));
        assert!(detect("git log"));
        assert!(detect("git log -p src/"));
        assert!(detect("git show HEAD"));
        for c in [
            "git diff --stat",
            "git diff --name-only",
            "git diff --quiet",
            "git diff -z",
            "git log -n 5",
            "git log -5",
            "git log -n5",
            "git log --max-count=3",
            "git log --format=%H",
            "git log --pretty=format:%s",
            "git show HEAD:src/main.rs",
            "git status --porcelain",
            "git status",
            "git -C diff status",
        ] {
            assert!(!detect(c), "{c}");
        }
    }
}
