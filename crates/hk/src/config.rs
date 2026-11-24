//! Optional per-project configuration in `.claude/hk.toml`.
//!
//! Zero-config is the default: every field has a default, and a missing or
//! unreadable file means "use defaults". A malformed file is reported on
//! stderr (never on stdout, which belongs to the hook protocol) and ignored.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Per-filter toggles, keyed by filter name (`cargo_test`, `pytest`, ...).
    pub filters: BTreeMap<String, bool>,
    pub limits: Limits,
    pub format: Toggle,
    pub session_start: Toggle,
    pub rewrite: Rewrite,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// The generic filter truncates outputs longer than this many lines.
    pub generic_max_lines: usize,
    pub head: usize,
    pub tail: usize,
    /// Maximum diagnostics/failures listed before `+N more`.
    pub max_errors: usize,
    /// Lines of context kept per failing test.
    pub stack_lines: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Toggle {
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Rewrite {
    /// Master switch for the PreToolUse Bash rewrite.
    pub enabled: bool,
    /// Extra program names that should go through the generic filter
    /// (head/tail truncation), e.g. `["make", "just"]`.
    pub generic_commands: Vec<String>,
    /// What to do with a filterable command that none of the user's allow
    /// rules covers: `"skip"` (default: leave it alone, so the normal
    /// approval prompt shows the original command) or `"rewrite"`.
    /// In `auto` and `bypassPermissions` modes commands are always rewritten.
    pub unapproved: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            filters: BTreeMap::new(),
            limits: Limits::default(),
            format: Toggle { enabled: true },
            session_start: Toggle { enabled: true },
            rewrite: Rewrite::default(),
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            generic_max_lines: 80,
            head: 30,
            tail: 30,
            max_errors: 20,
            stack_lines: 5,
        }
    }
}

impl Default for Toggle {
    fn default() -> Self {
        Toggle { enabled: true }
    }
}

impl Default for Rewrite {
    fn default() -> Self {
        Rewrite {
            enabled: true,
            generic_commands: Vec::new(),
            unapproved: "skip".into(),
        }
    }
}

impl Config {
    /// Load `.claude/hk.toml` from `dir` or its closest ancestor that has one.
    pub fn load(dir: &Path) -> Config {
        for d in dir.ancestors() {
            let p = d.join(".claude").join("hk.toml");
            if let Ok(s) = std::fs::read_to_string(&p) {
                return match Config::parse(&s) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("hk: ignoring invalid {}: {e}", p.display());
                        Config::default()
                    }
                };
            }
        }
        Config::default()
    }

    pub fn parse(s: &str) -> Result<Config, toml::de::Error> {
        toml::from_str(s)
    }

    pub fn filter_enabled(&self, name: &str) -> bool {
        self.filters.get(name).copied().unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_partial_config() {
        let c = Config::parse("[filters]\npytest = false\n[limits]\nmax_errors = 5\n").unwrap();
        assert!(!c.filter_enabled("pytest"));
        assert!(c.filter_enabled("cargo_test"));
        assert_eq!(c.limits.max_errors, 5);
        assert_eq!(c.limits.head, 30);
        assert!(c.format.enabled);
    }
}
