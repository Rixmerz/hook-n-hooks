//! Local data directory for logs and stats.
//!
//! `hk run` is executed by the Bash tool, not by the hook process, so it does
//! not see `CLAUDE_PLUGIN_DATA`; `hk stats` runs from the user's terminal.
//! Every subcommand therefore computes the same fixed per-OS directory:
//!
//! - `$HK_DATA_DIR` if set (tests, custom setups)
//! - Windows: `%LOCALAPPDATA%\hk`
//! - macOS: `~/Library/Application Support/hk`
//! - other: `$XDG_DATA_HOME/hk` or `~/.local/share/hk`

use std::path::PathBuf;

pub fn data_dir() -> PathBuf {
    if let Some(d) = env_path("HK_DATA_DIR") {
        return d;
    }
    if cfg!(windows) {
        if let Some(d) = env_path("LOCALAPPDATA") {
            return d.join("hk");
        }
    }
    let home = env_path("HOME")
        .or_else(|| env_path("USERPROFILE"))
        .unwrap_or_else(std::env::temp_dir);
    if cfg!(target_os = "macos") {
        return home.join("Library").join("Application Support").join("hk");
    }
    env_path("XDG_DATA_HOME")
        .unwrap_or_else(|| home.join(".local").join("share"))
        .join("hk")
}

pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

pub fn stats_file() -> PathBuf {
    data_dir().join("stats.jsonl")
}

fn env_path(k: &str) -> Option<PathBuf> {
    std::env::var_os(k)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Create `dir` (and parents) readable only by the current user on Unix.
pub fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The closest ancestor of `dir` that holds a `.git` entry, else `dir`.
pub fn project_root(dir: &std::path::Path) -> PathBuf {
    dir.ancestors()
        .find(|d| d.join(".git").exists())
        .unwrap_or(dir)
        .to_path_buf()
}

/// Global user config: `$HK_CONFIG`, else `$XDG_CONFIG_HOME/hk/hk.toml`,
/// else `~/.config/hk/hk.toml` (every OS, so one path is documented).
pub fn user_config() -> Option<PathBuf> {
    if let Some(p) = env_path("HK_CONFIG") {
        return Some(p);
    }
    let base = env_path("XDG_CONFIG_HOME").or_else(|| {
        env_path("HOME")
            .or_else(|| env_path("USERPROFILE"))
            .map(|h| h.join(".config"))
    })?;
    Some(base.join("hk").join("hk.toml"))
}

/// Errors hk swallowed while failing open (see `hk doctor`).
pub fn errors_file() -> PathBuf {
    data_dir().join("errors.jsonl")
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
