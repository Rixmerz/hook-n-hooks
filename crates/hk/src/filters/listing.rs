//! Large listings: `find`, `ls -R`, `tree`, `du`.
//!
//! Short listings pass through. Long ones become counts per top-level
//! directory, the first entries, and a hint about heavy directories
//! (`node_modules`, `target`, `.git`...) that are usually worth excluding.
//! `du` keeps the largest entries instead. Scripted forms (`find -exec`,
//! `-print0`, `-printf`, `tree -J`, `du -0`) are never wrapped.

use super::{Cmd, Filter};
use crate::config::Limits;
use std::collections::BTreeMap;

pub struct Listing;

const HEAVY: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".cache",
    "vendor",
    ".gradle",
    ".tox",
    "coverage",
];

impl Filter for Listing {
    fn name(&self) -> &'static str {
        "listing"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        let has = |f: &[&str]| cmd.args.iter().any(|a| f.contains(&a.as_str()));
        match cmd.program.as_str() {
            "find" => !has(&[
                "-exec", "-execdir", "-ok", "-okdir", "-delete", "-print0", "-printf", "-fprint",
                "-fprint0", "-fprintf", "-fls", "-ls",
            ]),
            "ls" => cmd.args.iter().any(|a| {
                a == "--recursive"
                    || (a.starts_with('-') && !a.starts_with("--") && a.contains('R'))
            }),
            "tree" => !has(&["-J", "-X", "--json", "--xml"]),
            "du" => !has(&["-0", "--null"]),
            _ => false,
        }
    }

    fn filter(&self, output: &str, _exit: i32, limits: &Limits) -> Option<String> {
        let lines: Vec<&str> = output.lines().collect();
        if lines.len() <= limits.generic_max_lines {
            return None;
        }
        if is_du(&lines) {
            return Some(du(&lines, limits));
        }
        if lines
            .iter()
            .any(|l| l.contains("├── ") || l.contains("└── "))
        {
            return Some(tree(&lines, limits));
        }
        if lines.iter().any(|l| l.ends_with(':') && !l.contains(' ')) && output.contains("\n\n") {
            return Some(ls_r(&lines, limits));
        }
        Some(paths(&lines, limits))
    }
}

fn is_du(lines: &[&str]) -> bool {
    lines.iter().take(20).all(|l| {
        l.split_once('\t')
            .is_some_and(|(size, _)| parse_size(size).is_some())
    })
}

/// `4.0K`, `1,5M`, `12G`, `512` (blocks) -> bytes, roughly.
fn parse_size(s: &str) -> Option<f64> {
    let s = s.trim().replace(',', ".");
    let (num, mult) = match s.chars().last()? {
        'K' | 'k' => (&s[..s.len() - 1], 1024.0),
        'M' => (&s[..s.len() - 1], 1024.0 * 1024.0),
        'G' => (&s[..s.len() - 1], 1024.0 * 1024.0 * 1024.0),
        'T' => (&s[..s.len() - 1], 1024.0f64.powi(4)),
        'B' => (&s[..s.len() - 1], 1.0),
        _ => (&s[..], 1024.0),
    };
    num.parse::<f64>().ok().map(|n| n * mult)
}

fn heavy_hint(counts: &BTreeMap<String, usize>) -> Option<String> {
    let heavy: Vec<String> = counts
        .iter()
        .filter(|(k, _)| k.split('/').any(|c| HEAVY.contains(&c)))
        .map(|(k, n)| format!("{k} ({n})"))
        .collect();
    if heavy.is_empty() {
        return None;
    }
    Some(format!(
        "[hk] heavy dirs: {}. Exclude them, e.g. `find . -name node_modules -prune -o -print`, `tree -I node_modules`, `du --exclude=node_modules`",
        heavy.join(", ")
    ))
}

/// Top-level bucket of a path: `./src/a/b.rs` -> `src`; `./README.md` -> `.`.
fn bucket(p: &str) -> String {
    let p = p.trim_start_matches("./");
    match p.split_once(['/', '\\']) {
        Some((first, _)) => first.to_string(),
        None => ".".to_string(),
    }
}

fn by_bucket(entries: &[&str]) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for e in entries {
        *m.entry(bucket(e)).or_insert(0) += 1;
    }
    m
}

fn render_counts(out: &mut Vec<String>, counts: &BTreeMap<String, usize>, cap: usize) {
    let mut v: Vec<(&String, &usize)> = counts.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    out.push("entries per top-level directory:".to_string());
    for (k, n) in v.iter().take(cap) {
        out.push(format!("  {k}: {n}"));
    }
    if v.len() > cap {
        out.push(format!("  +{} more", v.len() - cap));
    }
}

fn paths(lines: &[&str], limits: &Limits) -> String {
    let counts = by_bucket(lines);
    let mut out = vec![format!("[hk] {} entries", lines.len())];
    out.extend(heavy_hint(&counts));
    render_counts(&mut out, &counts, limits.max_errors);
    let light: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| !l.split(['/', '\\']).any(|c| HEAVY.contains(&c)))
        .collect();
    let head = limits.head.min(light.len());
    out.push(format!("first {head} entries outside heavy dirs:"));
    out.extend(light[..head].iter().map(|s| s.to_string()));
    out.join("\n")
}

fn ls_r(lines: &[&str], limits: &Limits) -> String {
    // Sections: `./dir:` followed by entries, separated by blank lines.
    let mut sections: Vec<(String, usize)> = Vec::new();
    let mut total = 0;
    for l in lines {
        if let Some(dir) = l.strip_suffix(':').filter(|d| !d.contains(' ')) {
            sections.push((dir.to_string(), 0));
        } else if !l.trim().is_empty() {
            total += 1;
            if let Some(s) = sections.last_mut() {
                s.1 += 1;
            } else {
                sections.push((".".to_string(), 1));
            }
        }
    }
    let mut counts = BTreeMap::new();
    for (dir, n) in &sections {
        *counts.entry(bucket(&format!("{dir}/x"))).or_insert(0) += n;
    }
    let mut out = vec![format!(
        "[hk] {total} entries in {} directories",
        sections.len()
    )];
    out.extend(heavy_hint(&counts));
    render_counts(&mut out, &counts, limits.max_errors);
    // Skip the sections of heavy directories.
    let mut light = Vec::new();
    let mut skipping = false;
    for l in lines {
        if let Some(dir) = l.strip_suffix(':').filter(|d| !d.contains(' ')) {
            skipping = dir.split(['/', '\\']).any(|c| HEAVY.contains(&c));
        }
        if !skipping {
            light.push(*l);
        }
    }
    push_head(&mut out, &light, limits.head);
    out.join("\n")
}

fn push_head(out: &mut Vec<String>, lines: &[&str], head: usize) {
    let head = head.min(lines.len());
    out.push(format!("first {head} lines outside heavy dirs:"));
    out.extend(lines[..head].iter().map(|s| s.to_string()));
}

fn tree(lines: &[&str], limits: &Limits) -> String {
    // Top-level entries are `├── x` / `└── x` with no indentation before them.
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut current = ".".to_string();
    let summary = lines
        .iter()
        .rev()
        .find(|l| l.contains(" director") && l.contains(" file"));
    for l in &lines[1..] {
        if let Some(name) = l.strip_prefix("├── ").or_else(|| l.strip_prefix("└── ")) {
            current = name.trim().to_string();
            counts.entry(current.clone()).or_insert(0);
        } else if l.contains("── ") {
            *counts.entry(current.clone()).or_insert(0) += 1;
        }
    }
    counts.retain(|_, n| *n > 0);
    let mut out = vec![format!(
        "[hk] {}",
        summary.map_or_else(|| format!("{} lines", lines.len()), |s| s.to_string())
    )];
    out.extend(heavy_hint(&counts));
    render_counts(&mut out, &counts, limits.max_errors);
    // Keep a heavy top-level entry's own line, not its subtree.
    let mut light = Vec::new();
    let mut skipping = false;
    for l in lines {
        if let Some(name) = l.strip_prefix("├── ").or_else(|| l.strip_prefix("└── ")) {
            skipping = HEAVY.contains(&name.trim());
            light.push(*l);
        } else if !skipping || !l.contains("── ") {
            light.push(*l);
        }
    }
    push_head(&mut out, &light, limits.head);
    out.join("\n")
}

fn du(lines: &[&str], limits: &Limits) -> String {
    let mut rows: Vec<(f64, &str)> = lines
        .iter()
        .filter_map(|l| {
            let (s, _) = l.split_once('\t')?;
            Some((parse_size(s)?, *l))
        })
        .collect();
    let total_line = lines.last().copied();
    rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let paths: Vec<&str> = lines
        .iter()
        .filter_map(|l| l.split_once('\t').map(|(_, p)| p))
        .collect();
    let counts = by_bucket(&paths);
    let mut out = vec![format!("[hk] {} entries, largest first", lines.len())];
    out.extend(heavy_hint(&counts));
    for (_, l) in rows.iter().take(limits.max_errors) {
        out.push(l.to_string());
    }
    if rows.len() > limits.max_errors {
        out.push(format!("+{} more", rows.len() - limits.max_errors));
    }
    if let Some(t) = total_line {
        out.push(format!("last line: {t}"));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(s: &str) -> bool {
        let v: Vec<String> = s.split_whitespace().map(String::from).collect();
        Listing.detect(&Cmd::from_argv(&v).unwrap())
    }

    #[test]
    fn detects_listings_only() {
        assert!(detect("find . -name *.rs"));
        assert!(detect("ls -laR src"));
        assert!(detect("tree -L 3"));
        assert!(detect("du -h ."));
        assert!(!detect("ls -la"));
        assert!(!detect("find . -name x -delete"));
        assert!(!detect("find . -print0"));
        assert!(!detect("tree -J"));
    }

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("4.0K"), Some(4096.0));
        assert_eq!(parse_size("2"), Some(2048.0));
        assert_eq!(parse_size("x"), None);
    }
}
