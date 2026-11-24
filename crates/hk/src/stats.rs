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
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
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

/// Render the `hk stats` report.
pub fn report(records: &[Record], session: Option<&str>) -> String {
    let recs: Vec<&Record> = records
        .iter()
        .filter(|r| session.is_none_or(|s| r.session == s))
        .collect();
    if recs.is_empty() {
        return "hk: no stats recorded yet".to_string();
    }
    let mut total = Agg::default();
    let mut by_filter: BTreeMap<&str, Agg> = BTreeMap::new();
    let mut by_session: BTreeMap<&str, (u64, Agg)> = BTreeMap::new();
    for r in &recs {
        total.add(r);
        by_filter.entry(&r.filter).or_default().add(r);
        let e = by_session
            .entry(&r.session)
            .or_insert((r.ts, Agg::default()));
        e.0 = e.0.max(r.ts);
        e.1.add(r);
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
        "session (latest 10)", "runs", "tokens saved"
    ));
    let mut sessions: Vec<_> = by_session.into_iter().collect();
    sessions.sort_by_key(|s| std::cmp::Reverse(s.1 .0));
    for (name, (_, a)) in sessions.into_iter().take(10) {
        out.push(format!("{name:<38} {:>6} {:>12}", a.runs, tokens(a.saved)));
    }
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
}
