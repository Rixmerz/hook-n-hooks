//! Mirror the user's own Bash permission rules for the original command.
//!
//! Claude Code evaluates permission rules against the command a PreToolUse
//! hook returns, not the original one. Verified end to end: with
//! `--allowedTools 'Bash(cargo test:*)'`, the rewritten
//! `'/…/hk' run … -- cargo test` is not allowed and asks for approval. A deny
//! rule such as `Bash(npm install:*)` would equally stop matching after the
//! rewrite. So, before rewriting, hk checks the *original* command against
//! the rules in the user's settings files:
//!
//! - matches a `deny` or `ask` rule: do not rewrite (the rule keeps working);
//! - matches an `allow` rule: rewrite and return `permissionDecision: allow`,
//!   which only restates a decision the user already made;
//! - matches nothing: see `Rewrite::unapproved` in the config.
//!
//! Matching is deliberately stricter than Claude Code's: when in doubt a rule
//! does not match "allow", so hk can never allow more than the user did.
//! Rules passed on the command line (`--allowedTools`) and managed settings
//! are not visible to hooks; they are not read.

use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone)]
pub struct Rules {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub ask: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Deny,
    Ask,
    Allow(String),
    Unknown,
}

pub fn settings_files(cwd: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    if let Some(h) = home {
        files.push(PathBuf::from(h).join(".claude").join("settings.json"));
    }
    let project = std::env::var_os("CLAUDE_PROJECT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.to_path_buf());
    files.push(project.join(".claude").join("settings.json"));
    files.push(project.join(".claude").join("settings.local.json"));
    files
}

pub fn load(files: &[PathBuf]) -> Rules {
    let mut r = Rules::default();
    for f in files {
        let Ok(s) = std::fs::read_to_string(f) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&s) else {
            continue;
        };
        let p = &v["permissions"];
        for (key, dst) in [
            ("allow", &mut r.allow),
            ("deny", &mut r.deny),
            ("ask", &mut r.ask),
        ] {
            if let Some(a) = p[key].as_array() {
                dst.extend(a.iter().filter_map(|x| x.as_str().map(String::from)));
            }
        }
    }
    r
}

impl Rules {
    pub fn verdict(&self, command: &str) -> Verdict {
        let command = command.trim();
        // Deny/ask: match loosely (any rule that *might* cover the command).
        if self.deny.iter().any(|r| matches(r, command, true)) {
            return Verdict::Deny;
        }
        if self.ask.iter().any(|r| matches(r, command, true)) {
            return Verdict::Ask;
        }
        // Allow: match strictly.
        match self.allow.iter().find(|r| matches(r, command, false)) {
            Some(r) => Verdict::Allow(r.clone()),
            None => Verdict::Unknown,
        }
    }
}

/// Match a `Bash(...)` rule. `loose` widens matching for deny/ask rules.
fn matches(rule: &str, command: &str, loose: bool) -> bool {
    let rule = rule.trim();
    if rule == "Bash" || rule == "Bash(*)" {
        return true;
    }
    let Some(spec) = rule.strip_prefix("Bash(").and_then(|r| r.strip_suffix(')')) else {
        return false;
    };
    if let Some(prefix) = spec.strip_suffix(":*") {
        return command == prefix || command.starts_with(&format!("{prefix} "));
    }
    if spec.contains('*') {
        return glob(spec, command) || (loose && glob(spec.trim_end_matches(" *"), command));
    }
    if loose {
        command == spec || command.starts_with(&format!("{spec} "))
    } else {
        command == spec
    }
}

/// `*` matches any run of characters (including none).
fn glob(pattern: &str, s: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut rest = s;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            match rest.find(part) {
                Some(idx) => rest = &rest[idx + part.len()..],
                None => return false,
            }
        }
    }
    rest.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(allow: &[&str], deny: &[&str], ask: &[&str]) -> Rules {
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect();
        Rules {
            allow: v(allow),
            deny: v(deny),
            ask: v(ask),
        }
    }

    #[test]
    fn verdicts() {
        let r = rules(
            &["Bash(cargo test:*)", "Bash(npm run *)", "Bash(pytest)"],
            &["Bash(npm install:*)"],
            &["Bash(pnpm add)"],
        );
        assert_eq!(
            r.verdict("cargo test --lib"),
            Verdict::Allow("Bash(cargo test:*)".into())
        );
        assert_eq!(
            r.verdict("cargo test"),
            Verdict::Allow("Bash(cargo test:*)".into())
        );
        assert_eq!(r.verdict("cargo testx"), Verdict::Unknown);
        assert_eq!(
            r.verdict("npm run test"),
            Verdict::Allow("Bash(npm run *)".into())
        );
        assert_eq!(r.verdict("pytest"), Verdict::Allow("Bash(pytest)".into()));
        assert_eq!(r.verdict("pytest -x"), Verdict::Unknown);
        assert_eq!(r.verdict("npm install left-pad"), Verdict::Deny);
        assert_eq!(r.verdict("pnpm add react"), Verdict::Ask);
        assert_eq!(r.verdict("go test ./..."), Verdict::Unknown);
    }

    #[test]
    fn deny_wins_over_allow() {
        let r = rules(&["Bash"], &["Bash(cargo publish:*)"], &[]);
        assert_eq!(r.verdict("cargo publish"), Verdict::Deny);
        assert_eq!(r.verdict("cargo test"), Verdict::Allow("Bash".into()));
    }
}
