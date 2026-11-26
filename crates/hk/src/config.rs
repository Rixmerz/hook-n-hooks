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
    pub format: Format,
    pub session_start: Toggle,
    pub rewrite: Rewrite,
    pub mcp: Mcp,
    pub misses: Misses,
}

/// PostToolUse formatting of touched files.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Format {
    pub enabled: bool,
    /// Also run fixers (`eslint --fix`, `ruff check --fix`, `biome check
    /// --write`). Off by default: a fixer can change behaviour, not only layout.
    pub fix: bool,
    /// Only keep the formatting when it changes no line outside the edit, so
    /// an unformatted file does not get reformatted wholesale.
    pub only_clean: bool,
    /// `"edit"`: after each Edit/Write. `"stop"`: once, when Claude stops,
    /// so a file does not change between two edits.
    pub when: String,
    /// Path fragments never formatted (in addition to lockfiles, migrations,
    /// fixtures, snapshots and vendored code).
    pub exclude: Vec<String>,
}

impl Default for Format {
    fn default() -> Self {
        Format {
            enabled: true,
            fix: false,
            only_clean: true,
            when: "edit".into(),
            exclude: Vec::new(),
        }
    }
}

/// PostToolUse compaction of MCP tool results.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Mcp {
    pub enabled: bool,
    /// Results whose JSON is at most this many characters are left alone.
    pub max_chars: usize,
    /// Tool names never shortened (`mcp__server__*` matches a whole server).
    pub skip: Vec<String>,
}

impl Default for Mcp {
    fn default() -> Self {
        Mcp {
            enabled: true,
            max_chars: 8000,
            skip: Vec::new(),
        }
    }
}

/// Recording of Bash outputs hk did not wrap (`hk misses`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Misses {
    pub enabled: bool,
    /// Outputs shorter than this many characters are not recorded.
    pub min_chars: usize,
}

impl Default for Misses {
    fn default() -> Self {
        Misses {
            enabled: true,
            min_chars: 2000,
        }
    }
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
    /// Per-program cap for the generic filter, e.g. `{ openspec = 40 }`.
    /// A program listed here is also wrapped with the generic filter.
    pub commands: BTreeMap<String, usize>,
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
            format: Format::default(),
            session_start: Toggle { enabled: true },
            rewrite: Rewrite::default(),
            mcp: Mcp::default(),
            misses: Misses::default(),
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
            commands: BTreeMap::new(),
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
    /// Load the user config (`~/.config/hk/hk.toml`, see
    /// [`crate::paths::user_config`]) and then `.claude/hk.toml` from `dir` or
    /// its closest ancestor that has one; project keys win over user keys.
    pub fn load(dir: &Path) -> Config {
        let mut merged = toml::Table::new();
        let mut read = |p: &Path| {
            let Ok(s) = std::fs::read_to_string(p) else {
                return;
            };
            match s.parse::<toml::Table>() {
                Ok(t) => merge(&mut merged, t),
                Err(e) => {
                    crate::doctor::record_error("config", &format!("{}: {e}", p.display()));
                    eprintln!("hk: ignoring invalid {}: {e}", p.display());
                }
            }
        };
        if let Some(user) = crate::paths::user_config() {
            read(&user);
        }
        if let Some(p) = Self::project_file(dir) {
            read(&p);
        }
        match toml::Value::Table(merged).try_into() {
            Ok(c) => c,
            Err(e) => {
                crate::doctor::record_error("config", &e.to_string());
                eprintln!("hk: ignoring invalid config: {e}");
                Config::default()
            }
        }
    }

    /// The project's `.claude/hk.toml`, if any.
    pub fn project_file(dir: &Path) -> Option<std::path::PathBuf> {
        dir.ancestors()
            .map(|d| d.join(".claude").join("hk.toml"))
            .find(|p| p.is_file())
    }

    pub fn parse(s: &str) -> Result<Config, toml::de::Error> {
        toml::from_str(s)
    }

    pub fn filter_enabled(&self, name: &str) -> bool {
        self.filters.get(name).copied().unwrap_or(true)
    }
}

/// Deep-merge `over` into `base`: tables merge key by key, anything else
/// (including arrays) is replaced.
fn merge(base: &mut toml::Table, over: toml::Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
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

    #[test]
    fn project_overrides_user() {
        let mut base: toml::Table = "[limits]\nhead = 5\ntail = 7\n[filters]\npytest = false\n"
            .parse()
            .unwrap();
        merge(&mut base, "[limits]\nhead = 9\n".parse().unwrap());
        let c: Config = toml::Value::Table(base).try_into().unwrap();
        assert_eq!((c.limits.head, c.limits.tail), (9, 7));
        assert!(!c.filter_enabled("pytest"));
    }
}
