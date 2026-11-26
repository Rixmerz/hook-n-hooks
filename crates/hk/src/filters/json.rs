//! Large JSON documents: `curl` to an API, `gh api`, `kubectl get -o json`,
//! `jq` with no filter. Keeps the shape: every key, the first items of each
//! array with the total, and long strings cut. Output that is not JSON falls
//! back to the generic filter.

use super::{Cmd, Filter};
use crate::config::Limits;
use serde_json::Value;

pub const NAME: &str = "json";

pub struct Json;

/// Items kept per array, and characters kept per string.
const ITEMS: usize = 3;
const STR_CHARS: usize = 200;
const MAX_DEPTH: usize = 8;

impl Filter for Json {
    fn name(&self) -> &'static str {
        NAME
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        let a: Vec<&str> = cmd.args.iter().map(String::as_str).collect();
        let json_out = a
            .windows(2)
            .any(|w| matches!(w, ["-o" | "--output", "json"]))
            || a.iter()
                .any(|x| matches!(*x, "-ojson" | "--output=json" | "-o=json"));
        match cmd.program.as_str() {
            // `curl -v` belongs to the logs filter; `-o FILE` writes nothing here.
            "curl" => {
                !a.iter().any(|x| {
                    matches!(
                        *x,
                        "-v" | "--verbose" | "-o" | "-O" | "--output" | "-I" | "--head"
                    ) || (x.starts_with('-') && !x.starts_with("--") && x.contains('v'))
                }) && a.iter().any(|x| x.starts_with("http"))
            }
            "http" | "https" | "xh" => true,
            "gh" => a.first() == Some(&"api"),
            "kubectl" | "oc" => json_out && matches!(a.first(), Some(&"get")),
            "aws" | "gcloud" | "az" => json_out || a.contains(&"--format=json"),
            _ => false,
        }
    }

    fn sniff(&self, output: &str) -> bool {
        let t = output.trim_start();
        (t.starts_with('{') || t.starts_with('[')) && serde_json::from_str::<Value>(t).is_ok()
    }

    fn filter(&self, output: &str, _exit: i32, limits: &Limits) -> Option<String> {
        let t = output.trim();
        // Headers before the body (`curl -i`): keep the status line only.
        let (status, body) = match t.find(['{', '[']) {
            Some(0) => (None, t),
            Some(i) if t.starts_with("HTTP/") => (t.lines().next(), &t[i..]),
            _ => return None,
        };
        let v: Value = serde_json::from_str(body).ok()?;
        if body.lines().count() <= limits.generic_max_lines && body.len() < 4000 {
            return None; // small enough as it is
        }
        let mut out = String::new();
        if let Some(s) = status {
            out.push_str(s);
            out.push('\n');
        }
        out.push_str(&format!("[json] {}\n", describe(&v)));
        out.push_str(&serde_json::to_string_pretty(&shrink(&v, 0)).ok()?);
        Some(out)
    }
}

fn describe(v: &Value) -> String {
    match v {
        Value::Array(a) => format!("array of {} item(s)", a.len()),
        Value::Object(o) => format!("object with {} key(s)", o.len()),
        _ => "scalar".into(),
    }
}

/// Same shape, fewer bytes: arrays keep their first items plus a marker
/// with the total, strings are cut, depth is capped.
pub fn shrink(v: &Value, depth: usize) -> Value {
    match v {
        Value::Array(a) if depth >= MAX_DEPTH => Value::String(format!("[… {} item(s)]", a.len())),
        Value::Object(o) if depth >= MAX_DEPTH => {
            Value::String(format!("{{… {} key(s)}}", o.len()))
        }
        Value::Array(a) => {
            let mut out: Vec<Value> = a.iter().take(ITEMS).map(|x| shrink(x, depth + 1)).collect();
            if a.len() > ITEMS {
                out.push(Value::String(format!(
                    "… +{} more (total {})",
                    a.len() - ITEMS,
                    a.len()
                )));
            }
            Value::Array(out)
        }
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), shrink(x, depth + 1)))
                .collect(),
        ),
        Value::String(s) if s.chars().count() > STR_CHARS => {
            let cut: String = s.chars().take(STR_CHARS).collect();
            Value::String(format!("{cut}… (+{} chars)", s.chars().count() - STR_CHARS))
        }
        other => other.clone(),
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
    fn detects_json_producers() {
        assert!(Json.detect(&cmd("curl -s https://api.x/items")));
        assert!(!Json.detect(&cmd("curl -sv https://api.x/items")));
        assert!(!Json.detect(&cmd("curl -o out.json https://x")));
        assert!(Json.detect(&cmd("gh api repos/a/b/pulls")));
        assert!(Json.detect(&cmd("kubectl get pods -o json")));
        assert!(!Json.detect(&cmd("kubectl get pods")));
    }

    #[test]
    fn keeps_shape_and_totals() {
        let items: Vec<String> = (0..500)
            .map(|i| format!(r#"{{"id":{i},"name":"item {i}","tags":["a","b"]}}"#))
            .collect();
        let raw = format!(r#"{{"total":500,"items":[{}]}}"#, items.join(","));
        let out = Json.filter(&raw, 0, &Limits::default()).unwrap();
        assert!(out.starts_with("[json] object with 2 key(s)"));
        assert!(out.contains("\"total\": 500"));
        assert!(out.contains("+497 more (total 500)"));
        assert!(out.len() < 600, "{out}");
        assert_eq!(Json.filter("not json", 0, &Limits::default()), None);
    }
}
