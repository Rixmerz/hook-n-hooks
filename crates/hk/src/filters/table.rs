//! Tabular output: `psql`, `mysql`, `sqlite3`, `kubectl get`, `docker ps`,
//! `docker images`. Keeps the header, the first rows and the row count.

use super::{Cmd, Filter};
use crate::config::Limits;

pub struct Table;

impl Filter for Table {
    fn name(&self) -> &'static str {
        "table"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        let a: Vec<&str> = cmd.args.iter().map(String::as_str).collect();
        let watch = a
            .iter()
            .any(|x| matches!(*x, "-w" | "--watch" | "-f" | "--follow"));
        if watch {
            return false;
        }
        match cmd.program.as_str() {
            // Only with a query on the command line: an interactive shell never ends.
            "psql" => a.iter().any(|x| *x == "-c" || x.starts_with("--command")),
            "mysql" => a.iter().any(|x| *x == "-e" || x.starts_with("--execute")),
            "sqlite3" => a.len() >= 2,
            "kubectl" | "oc" => {
                a.first() == Some(&"get")
                    && !a
                        .iter()
                        .any(|x| x.starts_with("-o") || x.starts_with("--output"))
            }
            "docker" | "podman" => matches!(a.first(), Some(&("ps" | "images"))),
            _ => false,
        }
    }

    fn filter(&self, output: &str, _exit: i32, limits: &Limits) -> Option<String> {
        let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
        let keep = limits.head.max(5);
        if lines.len() <= keep + 2 {
            return None;
        }
        // psql: header, `----+----` rule, rows, `(N rows)` footer.
        let rule = lines
            .iter()
            .take(4)
            .position(|l| l.trim_start().starts_with("---") || l.starts_with("+-"));
        let header_end = rule.map_or(1, |i| i + 1);
        let footer = lines
            .last()
            .filter(|l| l.trim_start().starts_with('(') && l.contains("row"))
            .copied();
        let body_end = lines.len() - usize::from(footer.is_some());
        let rows = &lines[header_end..body_end];
        if rows.len() <= keep {
            return None;
        }
        let mut out: Vec<String> = lines[..header_end].iter().map(|s| s.to_string()).collect();
        out.extend(rows.iter().take(keep).map(|s| s.to_string()));
        out.push(format!(
            "… +{} more row(s) ({} total)",
            rows.len() - keep,
            rows.len()
        ));
        if let Some(f) = footer {
            out.push(f.to_string());
        }
        Some(out.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(s: &str) -> Cmd {
        let v: Vec<String> = s.split_whitespace().map(String::from).collect();
        Cmd::from_argv(&v).unwrap()
    }

    #[test]
    fn detects_tables() {
        assert!(Table.detect(&cmd("psql -d x -c select")));
        assert!(!Table.detect(&cmd("psql -d x")));
        assert!(Table.detect(&cmd("kubectl get pods -A")));
        assert!(!Table.detect(&cmd("kubectl get pods -w")));
        assert!(!Table.detect(&cmd("kubectl get pods -o yaml")));
        assert!(Table.detect(&cmd("docker ps -a")));
    }

    #[test]
    fn keeps_header_and_count() {
        let mut s = String::from(" id | name\n----+------\n");
        for i in 0..100 {
            s.push_str(&format!(" {i} | n{i}\n"));
        }
        s.push_str("(100 rows)\n");
        let out = Table.filter(&s, 0, &Limits::default()).unwrap();
        assert!(out.starts_with(" id | name\n----+------\n 0 | n0"));
        assert!(out.contains("+70 more row(s) (100 total)"));
        assert!(out.ends_with("(100 rows)"));
    }
}
