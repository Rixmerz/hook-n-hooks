//! `docker build` (legacy builder and BuildKit), `docker compose build`, and
//! `docker compose up` when it detaches or exits on its own.
//!
//! Layer and progress lines are dropped. On success the final result is kept
//! (image id, tags, container states); on failure the failing step, the tail
//! of its output and the error lines. Foreground `docker compose up` streams
//! logs forever and is never wrapped. `docker logs` is handled by `logs`.

use super::{Cmd, Filter};
use crate::config::Limits;
use crate::text::dedup;

pub struct Docker;

impl Filter for Docker {
    fn name(&self) -> &'static str {
        "docker"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        let a: Vec<&str> = cmd.args.iter().map(String::as_str).collect();
        if a.iter().any(|x| matches!(*x, "-q" | "--quiet")) {
            return false;
        }
        let compose: &[&str] = match (cmd.program.as_str(), a.as_slice()) {
            ("docker", ["build" | "image", ..]) => {
                return a[0] == "build" || a.get(1) == Some(&"build");
            }
            ("docker", ["buildx", "build", ..]) => return true,
            ("docker", ["compose", rest @ ..]) => rest,
            ("docker-compose", rest) => rest,
            _ => return false,
        };
        // Skip compose's own options (`-f file`, `-p name`).
        let mut i = 0;
        while i < compose.len() && compose[i].starts_with('-') {
            i += if matches!(
                compose[i],
                "-f" | "--file" | "-p" | "--project-name" | "--env-file"
            ) {
                2
            } else {
                1
            };
        }
        let rest = &compose[i.min(compose.len())..];
        match rest.first() {
            Some(&"build") => true,
            Some(&"up") => rest.iter().any(|x| {
                matches!(
                    *x,
                    "-d" | "--detach" | "--abort-on-container-exit" | "--wait"
                ) || x.starts_with("--exit-code-from")
            }),
            _ => false,
        }
    }

    fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String> {
        let lines: Vec<&str> = output.lines().collect();
        let steps = count_steps(&lines);
        let mut out = Vec::new();
        if exit == 0 {
            out.push(if steps > 0 {
                format!("✓ docker: {steps} build step(s) done")
            } else {
                "✓ docker: done".to_string()
            });
            out.extend(results(&lines));
        } else {
            out.push("✗ docker failed".to_string());
            let fail = failure(&lines, limits.stack_lines * 2);
            if fail.is_empty() {
                return None; // unrecognised: keep it all via generic
            }
            out.extend(fail);
        }
        Some(dedup(out).join("\n"))
    }
}

/// `Step 3/5 : RUN ...` (legacy) or distinct `#N [stage x/y] ...` (BuildKit).
fn count_steps(lines: &[&str]) -> usize {
    let legacy = lines.iter().filter(|l| l.starts_with("Step ")).count();
    if legacy > 0 {
        return legacy;
    }
    let mut ids: Vec<&str> = lines
        .iter()
        .filter_map(|l| {
            let (id, rest) = l.split_once(' ')?;
            (id.starts_with('#') && rest.starts_with('[')).then_some(id)
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids.len()
}

/// BuildKit `#12 0.345 text` -> `text`; `#12 DONE 0.1s` -> `DONE 0.1s`.
fn strip_buildkit(l: &str) -> &str {
    let Some((id, rest)) = l.split_once(' ') else {
        return l;
    };
    if !(id.starts_with('#') && id[1..].chars().all(|c| c.is_ascii_digit())) {
        return l;
    }
    match rest.split_once(' ') {
        Some((t, r)) if t.parse::<f64>().is_ok() => r,
        _ => rest,
    }
}

fn is_result(t: &str) -> bool {
    if t.starts_with("--->") {
        return false; // legacy builder layer lines
    }
    t.starts_with("Successfully built")
        || t.starts_with("Successfully tagged")
        || t.starts_with("naming to ")
        || t.starts_with("writing image ")
        || t.starts_with("WARNING")
        || t.contains(" Started")
        || t.contains(" Healthy")
        || t.contains(" Created")
        || t.contains(" Recreated")
        || t.contains(" Running ")
        || t.contains(" Built")
        || t.contains(" Exited")
}

fn results(lines: &[&str]) -> Vec<String> {
    lines
        .iter()
        .map(|l| strip_buildkit(l).trim())
        .filter(|t| is_result(t) && !t.starts_with("[+]"))
        .map(|t| t.trim_start_matches(['✔', ' ']).to_string())
        .collect()
}

fn is_error(t: &str) -> bool {
    let lower = t.to_ascii_lowercase();
    t.starts_with("ERROR")
        || t.starts_with("Error")
        || t.starts_with("The command '")
        || lower.contains("failed to solve")
        || lower.contains("did not complete successfully")
        || t.contains("✘")
        || lower.starts_with("error:")
        || t.starts_with(">>>")
        || (t.starts_with("Dockerfile:") && t[11..].chars().all(|c| c.is_ascii_digit()))
}

fn failure(lines: &[&str], tail: usize) -> Vec<String> {
    let mut out = Vec::new();
    // The failing step and the tail of its output.
    let legacy_step = lines.iter().rposition(|l| l.starts_with("Step "));
    let kit_step = lines.iter().position(|l| l.trim_start().starts_with("> ["));
    if let Some(i) = legacy_step {
        out.push(lines[i].to_string());
        let body: Vec<&str> = lines[i + 1..]
            .iter()
            .copied()
            .filter(|l| !l.starts_with(" ---> ") && !l.starts_with("The command '"))
            .collect();
        let skip = body.len().saturating_sub(tail);
        if skip > 0 {
            out.push(format!("[hk] ... {skip} earlier output lines of this step"));
        }
        out.extend(body[skip..].iter().map(|s| s.to_string()));
    } else if let Some(i) = kit_step {
        // `------` / ` > [2/3] RUN ...:` / output / `------`
        out.push(lines[i].trim().to_string());
        let body: Vec<&str> = lines[i + 1..]
            .iter()
            .copied()
            .take_while(|l| !l.starts_with("------"))
            .collect();
        let skip = body.len().saturating_sub(tail);
        if skip > 0 {
            out.push(format!("[hk] ... {skip} earlier output lines of this step"));
        }
        out.extend(body[skip..].iter().map(|s| s.trim().to_string()));
    }
    for l in lines {
        let t = strip_buildkit(l).trim();
        if is_error(t) {
            out.push(t.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(s: &str) -> bool {
        let v: Vec<String> = s.split_whitespace().map(String::from).collect();
        Docker.detect(&Cmd::from_argv(&v).unwrap())
    }

    #[test]
    fn never_wraps_foreground_compose_up() {
        assert!(detect("docker build -t x ."));
        assert!(detect("docker buildx build ."));
        assert!(detect("docker compose build"));
        assert!(detect("docker compose -f dev.yml up -d"));
        assert!(detect("docker-compose up --abort-on-container-exit"));
        assert!(!detect("docker compose up"));
        assert!(!detect("docker build -q ."));
        assert!(!detect("docker run ubuntu"));
        assert!(!detect("docker compose logs"));
    }
}
