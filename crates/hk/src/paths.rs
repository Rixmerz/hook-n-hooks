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
