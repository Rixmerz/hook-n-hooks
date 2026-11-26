//! Local, append-only savings stats. Nothing ever leaves the machine.
//!
//! Each filtered run appends one JSON line to `stats.jsonl`:
//! `{"ts":1700000000,"session":"abc","filter":"cargo_test","raw":12345,"out":40}`
//! where `raw` and `out` are character counts. Tokens are estimated as
//! `chars / 4`. `raw` is capped at [`BASH_OUTPUT_CAP`] because Claude Code
//! itself truncates longer Bash output, so counting beyond that would
//! overstate the savings.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// Claude Code's default `BASH_MAX_OUTPUT_LENGTH`.
pub const BASH_OUTPUT_CAP: usize = 30_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub ts: u64,
    pub session: String,
    pub filter: String,
    pub raw: usize,
    pub out: usize,
    /// Project root (closest `.git` ancestor) the command ran in.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    /// Wall time in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
    /// Empty for an `hk run`; `miss:<reason>` for a Bash output hk did not
    /// wrap (see `hk misses`), `mcp` for a compacted MCP result.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
}

impl Record {
    pub fn saved_chars(&self) -> usize {
        self.raw.min(BASH_OUTPUT_CAP).saturating_sub(self.out)
    }
}

pub fn tokens(chars: usize) -> usize {
    chars / 4
}

pub fn append(path: &Path, rec: &Record) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        crate::paths::create_private_dir(dir)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    let mut line = serde_json::to_string(rec).map_err(std::io::Error::other)?;
    line.push('\n');
    f.write_all(line.as_bytes())
}

pub fn load(path: &Path) -> Vec<Record> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Agg {
    pub runs: usize,
    pub raw: usize,
    pub out: usize,
    pub saved: usize,
}

impl Agg {
    fn add(&mut self, r: &Record) {
        self.runs += 1;
        self.raw += r.raw.min(BASH_OUTPUT_CAP);
        self.out += r.out;
        self.saved += r.saved_chars();
    }
}

/// Which records a report covers.
#[derive(Debug, Default)]
pub struct Scope<'a> {
    pub session: Option<&'a str>,
    /// Matches when the record's project ends with this text.
    pub project: Option<&'a str>,
}

impl Scope<'_> {
    fn keep(&self, r: &Record) -> bool {
        self.session.is_none_or(|s| r.session == s)
            && self.project.is_none_or(|p| r.project.ends_with(p))
    }
}

fn is_miss(r: &Record) -> bool {
    r.kind.starts_with("miss:")
}

/// Render the `hk stats` report.
pub fn report(records: &[Record], session: Option<&str>) -> String {
    report_scoped(
        records,
        &Scope {
            session,
            project: None,
        },
    )
}

pub fn report_scoped(records: &[Record], scope: &Scope) -> String {
    let recs: Vec<&Record> = records
        .iter()
        .filter(|r| scope.keep(r) && !is_miss(r))
        .collect();
    if recs.is_empty() {
        return "hk: no stats recorded yet".to_string();
    }
    let mut total = Agg::default();
    let mut by_filter: BTreeMap<String, Agg> = BTreeMap::new();
    let mut by_session: BTreeMap<&str, (u64, Agg)> = BTreeMap::new();
    let mut by_project: BTreeMap<&str, Agg> = BTreeMap::new();
    for r in &recs {
        total.add(r);
        let fname = if r.kind == "mcp" {
            "mcp".to_string()
        } else {
            r.filter.clone()
        };
        by_filter.entry(fname).or_default().add(r);
        let e = by_session
            .entry(&r.session)
            .or_insert((r.ts, Agg::default()));
        e.0 = e.0.max(r.ts);
        e.1.add(r);
        let proj = if r.project.is_empty() {
            "(unknown)"
        } else {
            &r.project
        };
        by_project.entry(proj).or_default().add(r);
    }
    let mut out = Vec::new();
    out.push(format!(
        "Estimated tokens saved: {} over {} run(s) ({} -> {} chars, tokens = chars/4)",
        tokens(total.saved),
        total.runs,
        total.raw,
        total.out
    ));
    out.push(String::new());
    out.push(format!(
        "{:<16} {:>6} {:>12}",
        "filter", "runs", "tokens saved"
    ));
    let mut filters: Vec<_> = by_filter.into_iter().collect();
    filters.sort_by_key(|f| std::cmp::Reverse(f.1.saved));
    for (name, a) in filters {
        out.push(format!("{name:<16} {:>6} {:>12}", a.runs, tokens(a.saved)));
    }
    out.push(String::new());
    out.push(format!(
        "{:<38} {:>6} {:>12}",
        "project (top 10)", "runs", "tokens saved"
    ));
    let mut projects: Vec<_> = by_project.into_iter().collect();
    projects.sort_by_key(|p| std::cmp::Reverse(p.1.saved));
    for (name, a) in projects.into_iter().take(10) {
        let short = shorten(name, 38);
        out.push(format!("{short:<38} {:>6} {:>12}", a.runs, tokens(a.saved)));
    }
    out.push(String::new());
    out.push(format!(
        "{:<38} {:>6} {:>12}",
        "session (latest 10)", "runs", "tokens saved"
    ));
    let mut sessions: Vec<_> = by_session.into_iter().collect();
    sessions.sort_by_key(|s| std::cmp::Reverse(s.1 .0));
    for (name, (_, a)) in sessions.into_iter().take(10) {
        out.push(format!("{name:<38} {:>6} {:>12}", a.runs, tokens(a.saved)));
    }
    out.join("\n")
}

/// Keep the end of a long path, which is the part that tells projects apart.
fn shorten(s: &str, n: usize) -> String {
    let len = s.chars().count();
    if len <= n {
        return s.to_string();
    }
    let tail: String = s.chars().skip(len - (n - 1)).collect();
    format!("…{tail}")
}

/// `hk stats --json`: totals per filter, project and session.
pub fn report_json(records: &[Record], scope: &Scope) -> serde_json::Value {
    let mut by: BTreeMap<&str, BTreeMap<String, Agg>> = BTreeMap::new();
    let mut total = Agg::default();
    for r in records.iter().filter(|r| scope.keep(r) && !is_miss(r)) {
        total.add(r);
        by.entry("filters")
            .or_default()
            .entry(r.filter.clone())
            .or_default()
            .add(r);
        by.entry("projects")
            .or_default()
            .entry(r.project.clone())
            .or_default()
            .add(r);
        by.entry("sessions")
            .or_default()
            .entry(r.session.clone())
            .or_default()
            .add(r);
    }
    let agg = |a: &Agg| serde_json::json!({"runs": a.runs, "raw_chars": a.raw, "out_chars": a.out, "tokens_saved": tokens(a.saved)});
    let mut out = serde_json::json!({"total": agg(&total)});
    for (k, m) in by {
        out[k] = m
            .iter()
            .map(|(n, a)| (n.clone(), agg(a)))
            .collect::<serde_json::Map<_, _>>()
            .into();
    }
    out
}

/// `hk misses`: large Bash outputs hk did not wrap, grouped by reason, so
/// the next filter is picked from data.
pub fn misses(records: &[Record], scope: &Scope) -> String {
    let mut by: BTreeMap<(&str, &str), (usize, usize)> = BTreeMap::new();
    for r in records.iter().filter(|r| scope.keep(r) && is_miss(r)) {
        let reason = r.kind.trim_start_matches("miss:");
        let reason = reason.split(':').next().unwrap_or(reason);
        let e = by.entry((reason, &r.filter)).or_default();
        e.0 += 1;
        e.1 += r.raw.min(BASH_OUTPUT_CAP);
    }
    if by.is_empty() {
        return "hk: no unwrapped large outputs recorded".into();
    }
    let mut rows: Vec<_> = by.into_iter().collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1 .1));
    let mut out = vec![format!(
        "{:<14} {:<20} {:>6} {:>12}",
        "reason", "program", "runs", "~tokens"
    )];
    for ((reason, prog), (n, chars)) in rows.into_iter().take(30) {
        out.push(format!(
            "{reason:<14} {prog:<20} {n:>6} {:>12}",
            tokens(chars)
        ));
    }
    out.push(String::new());
    out.push("reasons: no-filter = no filter knows the program; shape = redirection, subshell or other pipe; unparsable = heredoc, background or comment; permissions = a filter exists but your rules did not allow the rewrite; watch = long-running mode".into());
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(session: &str, filter: &str, raw: usize, out: usize) -> Record {
        Record {
            ts: 1,
            session: session.into(),
            filter: filter.into(),
            raw,
            out,
            project: String::new(),
            exit: None,
            ms: None,
            kind: String::new(),
        }
    }

    #[test]
    fn caps_raw_at_bash_limit() {
        assert_eq!(rec("s", "f", 100_000, 0).saved_chars(), BASH_OUTPUT_CAP);
        assert_eq!(rec("s", "f", 10, 40).saved_chars(), 0);
    }

    #[test]
    fn report_aggregates() {
        let rs = vec![rec("a", "pytest", 4000, 0), rec("b", "cargo_test", 800, 0)];
        let r = report(&rs, None);
        assert!(r.starts_with("Estimated tokens saved: 1200 over 2 run(s)"));
        assert!(report(&rs, Some("b")).contains("200 over 1 run(s)"));
    }

    #[test]
    fn misses_do_not_count_as_savings() {
        let mut m = rec("a", "openspec", 9000, 9000);
        m.kind = "miss:no-filter:openspec".into();
        let rs = vec![rec("a", "pytest", 4000, 0), m];
        assert!(report(&rs, None).contains("over 1 run(s)"));
        let r = misses(&rs, &Scope::default());
        assert!(r.contains("no-filter") && r.contains("openspec"), "{r}");
        let j = report_json(&rs, &Scope::default());
        assert_eq!(j["total"]["runs"], 1);
    }
}
