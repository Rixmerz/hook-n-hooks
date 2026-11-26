//! Make failing open visible. Every hook swallows its own errors so it can
//! never block a tool call; this module records them, and `hk doctor` and the
//! SessionStart note show them.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Keep at most this many error lines; older ones are dropped on write.
const MAX_ERRORS: usize = 200;

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorRec {
    pub ts: u64,
    pub kind: String,
    pub msg: String,
    #[serde(default)]
    pub version: String,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Append an error. Never fails, never prints on stdout.
pub fn record_error(kind: &str, msg: &str) {
    let path = crate::paths::errors_file();
    let _ = (|| -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            crate::paths::create_private_dir(d)?;
        }
        let rec = ErrorRec {
            ts: now(),
            kind: kind.into(),
            msg: crate::redact::line(msg).chars().take(500).collect(),
            version: env!("CARGO_PKG_VERSION").into(),
        };
        let mut lines: Vec<String> = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect();
        lines.push(serde_json::to_string(&rec).map_err(std::io::Error::other)?);
        let start = lines.len().saturating_sub(MAX_ERRORS);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&path)?;
        f.write_all((lines[start..].join("\n") + "\n").as_bytes())
    })();
}

pub fn load_errors() -> Vec<ErrorRec> {
    std::fs::read_to_string(crate::paths::errors_file())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Install a panic hook that records the panic before the process aborts.
pub fn install_panic_hook(kind: &'static str) {
    std::panic::set_hook(Box::new(move |info| {
        record_error(kind, &format!("panic: {info}"));
    }));
}

fn seen_file() -> PathBuf {
    crate::paths::data_dir().join("errors.seen")
}

/// For SessionStart: one line when errors were recorded since the previous
/// session start, then mark them as seen.
pub fn unseen_note() -> Option<String> {
    let seen: u64 = std::fs::read_to_string(seen_file())
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let errs: Vec<ErrorRec> = load_errors().into_iter().filter(|e| e.ts > seen).collect();
    let _ = std::fs::write(seen_file(), now().to_string());
    let last = errs.last()?;
    Some(format!(
        "[hk] {} hook error(s) since the last session (latest: {}: {}); run `hk doctor`",
        errs.len(),
        last.kind,
        last.msg.lines().next().unwrap_or("")
    ))
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.trim().split('.').map(|p| p.parse::<u64>().ok());
    Some((it.next()??, it.next()??, it.next()??))
}

/// When the plugin cache holds a newer hook-n-hooks than the binary that is
/// running (an old session after an update), say so.
pub fn version_note(plugin_root: Option<&Path>) -> Option<String> {
    let root = plugin_root?;
    let parent = root.parent()?;
    let mine = parse_version(env!("CARGO_PKG_VERSION"))?;
    let newest = std::fs::read_dir(parent)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            parse_version(&n).map(|v| (v, n))
        })
        .max()?;
    (newest.0 > mine).then(|| {
        format!(
            "[hk] running hk {} but {} is installed; restart Claude Code to use it",
            env!("CARGO_PKG_VERSION"),
            newest.1
        )
    })
}

/// `hk doctor`: binary, config, data directory, formatters and last errors.
pub fn report(cwd: &Path) -> String {
    let mut out = Vec::new();
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());
    out.push(format!("hk {} ({exe})", env!("CARGO_PKG_VERSION")));
    let root = std::env::var_os("CLAUDE_PLUGIN_ROOT").map(PathBuf::from);
    if let Some(n) = version_note(root.as_deref()) {
        out.push(format!("✗ {n}"));
    }
    // Config.
    let user = crate::paths::user_config();
    let project = crate::config::Config::project_file(cwd);
    for (label, p) in [("user config", user), ("project config", project)] {
        match p {
            Some(p) if p.is_file() => {
                let ok = std::fs::read_to_string(&p)
                    .ok()
                    .and_then(|s| s.parse::<toml::Table>().ok())
                    .is_some();
                let mark = if ok { "✓" } else { "✗ invalid:" };
                out.push(format!("{mark} {label}: {}", p.display()));
            }
            Some(p) => out.push(format!("· {label}: none ({})", p.display())),
            None => out.push(format!("· {label}: none")),
        }
    }
    // Data directory.
    let data = crate::paths::data_dir();
    out.push(format!("· data: {}", data.display()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let logs = crate::paths::logs_dir();
        if let Ok(m) = std::fs::metadata(&logs) {
            let mode = m.permissions().mode() & 0o777;
            let mark = if mode & 0o077 == 0 { "✓" } else { "✗" };
            out.push(format!(
                "{mark} logs dir mode {mode:o} ({})",
                logs.display()
            ));
        }
    }
    // Formatters the PostToolUse hook would use here.
    let found: Vec<String> = [
        "rustfmt",
        "gofmt",
        "ruff",
        "biome",
        "prettier",
        "prettierd",
        "eslint",
        "eslint_d",
    ]
    .iter()
    .filter_map(|t| crate::format::which(t, cwd).map(|_| t.to_string()))
    .collect();
    out.push(format!(
        "· formatters found: {}",
        if found.is_empty() {
            "none".into()
        } else {
            found.join(", ")
        }
    ));
    // Errors.
    let errs = load_errors();
    if errs.is_empty() {
        out.push("✓ no hook errors recorded".into());
    } else {
        out.push(format!("✗ {} hook error(s) recorded; latest:", errs.len()));
        for e in errs.iter().rev().take(5) {
            out.push(format!(
                "  {} [{}] {}: {}",
                e.ts,
                e.version,
                e.kind,
                e.msg.lines().next().unwrap_or("")
            ));
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_newer_installed_version() {
        let base = std::env::temp_dir().join(format!("hk-doctor-{}", std::process::id()));
        let mine = base.join(env!("CARGO_PKG_VERSION"));
        std::fs::create_dir_all(&mine).unwrap();
        assert_eq!(version_note(Some(&mine)), None);
        std::fs::create_dir_all(base.join("99.0.0")).unwrap();
        assert!(version_note(Some(&mine)).unwrap().contains("99.0.0"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
