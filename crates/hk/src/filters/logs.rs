//! Logs and network: `docker logs`, `docker compose logs`, `kubectl logs`,
//! `journalctl`, and `curl -v`. Following modes (`-f`, `--follow`) never end
//! and are never wrapped.
//!
//! Repeated lines are collapsed: lines that differ only in their digits
//! (timestamps, counters, ids) are shown once, at their first occurrence,
//! with `(xN)`. What is left is cut to head/tail. For `curl -v`, the TLS
//! handshake and `{ [5 bytes data]` noise go; request and response headers
//! stay.

use super::{Cmd, Filter};
use crate::config::Limits;
use crate::text::head_tail;
use std::collections::HashMap;

pub struct Logs;

impl Filter for Logs {
    fn name(&self) -> &'static str {
        "logs"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        let a: Vec<&str> = cmd.args.iter().map(String::as_str).collect();
        let follows = a.iter().any(|x| {
            matches!(*x, "-f" | "--follow" | "--follow=true")
                || (x.starts_with('-') && !x.starts_with("--") && x.len() > 2 && x.contains('f'))
        });
        let owned = match (cmd.program.as_str(), a.as_slice()) {
            ("docker", ["logs", ..]) | ("docker", ["compose", .., "logs"]) => true,
            ("docker", ["compose", rest @ ..]) => rest.contains(&"logs"),
            ("docker-compose", rest) => rest.contains(&"logs"),
            ("kubectl", rest) => rest.first() == Some(&"logs"),
            ("journalctl", _) => true,
            ("curl", _) => {
                return a.iter().any(|x| {
                    *x == "-v"
                        || *x == "--verbose"
                        || (x.starts_with('-') && !x.starts_with("--") && x.contains('v'))
                }) && !a.iter().any(|x| x.starts_with("--trace"));
            }
            _ => false,
        };
        owned && !follows
    }

    fn filter(&self, output: &str, _exit: i32, limits: &Limits) -> Option<String> {
        let lines: Vec<&str> = output.lines().collect();
        let curl = lines
            .iter()
            .any(|l| l.starts_with("* ") || l.starts_with("> ") || l.starts_with("< "));
        let kept: Vec<&str> = if curl {
            lines.iter().copied().filter(|l| !curl_noise(l)).collect()
        } else {
            lines.clone()
        };
        if kept.len() <= limits.generic_max_lines && kept.len() == lines.len() {
            return None; // short and nothing to drop: pass through
        }
        let collapsed = collapse(&kept);
        let mut text = collapsed.join("\n");
        if let Some(cut) = head_tail(&text, limits.generic_max_lines, limits.head, limits.tail) {
            text = cut;
        }
        let dropped = lines.len() - kept.len();
        let mut head = format!(
            "[hk] {} lines, {} after collapsing repeats",
            lines.len(),
            collapsed.len()
        );
        if dropped > 0 {
            head.push_str(&format!(", {dropped} TLS/transfer noise lines dropped"));
        }
        Some(format!("{head}\n{text}"))
    }
}

fn curl_noise(l: &str) -> bool {
    if l.starts_with("{ [") || l.starts_with("} [") {
        return true;
    }
    let Some(info) = l.strip_prefix("* ") else {
        return false;
    };
    let keep = [
        "Connected",
        "Established",
        "rror",
        "ail",
        "Could not",
        "timed out",
        "refused",
        "verified",
        "expire date",
        "subject:",
        "Closing",
        "Connection #",
        "using HTTP",
        "Issue another request",
        "Redirect",
    ];
    !keep.iter().any(|k| info.contains(k))
}

/// Order-preserving collapse of lines equal up to their digits. A collapsed
/// line shows `(xN)` when every copy was identical, `(xN similar)` when
/// only numbers differed (the first one is shown).
fn collapse(lines: &[&str]) -> Vec<String> {
    let key = |l: &str| -> String {
        let mut k = String::with_capacity(l.len());
        for c in l.chars() {
            if c.is_ascii_digit() {
                if !k.ends_with('#') {
                    k.push('#');
                }
            } else {
                k.push(c);
            }
        }
        k
    };
    // key -> (count, first line, all identical)
    let mut counts: HashMap<String, (usize, &str, bool)> = HashMap::new();
    for l in lines {
        let e = counts.entry(key(l)).or_insert((0, l, true));
        e.0 += 1;
        e.2 &= e.1 == *l;
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for l in lines {
        let k = key(l);
        if l.trim().is_empty() || !seen.insert(k.clone()) {
            if l.trim().is_empty() && out.last().is_some_and(|p: &String| !p.is_empty()) {
                out.push(String::new());
            }
            continue;
        }
        match counts[&k] {
            (1, _, _) => out.push(l.to_string()),
            (n, _, true) => out.push(format!("{l} (x{n})")),
            (n, _, false) => out.push(format!("{l} (x{n} similar)")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(s: &str) -> bool {
        let v: Vec<String> = s.split_whitespace().map(String::from).collect();
        Logs.detect(&Cmd::from_argv(&v).unwrap())
    }

    #[test]
    fn skips_follow_modes() {
        assert!(detect("docker logs web"));
        assert!(detect("docker compose logs api"));
        assert!(detect("kubectl logs pod/x --tail=500"));
        assert!(detect("journalctl -u nginx --since today"));
        assert!(detect("curl -v https://x"));
        assert!(detect("curl -sv https://x"));
        assert!(!detect("curl https://x"));
        assert!(!detect("docker logs -f web"));
        assert!(!detect("kubectl logs --follow x"));
        assert!(!detect("journalctl -fu nginx"));
        assert!(!detect("kubectl get pods"));
    }

    #[test]
    fn collapses_repeats_in_order() {
        let c = collapse(&["start", "tick 1", "tick 2", "boom", "tick 3"]);
        assert_eq!(c, vec!["start", "tick 1 (x3 similar)", "boom"]);
        let c = collapse(&["ok", "ok", "id 9", "id 10"]);
        assert_eq!(c, vec!["ok (x2)", "id 9 (x2 similar)"]);
    }
}
