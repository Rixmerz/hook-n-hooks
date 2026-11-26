//! hk: token-saving hooks for Claude Code.
//!
//! See `docs/ARCHITECTURE.md` for how the pieces fit together.

pub mod config;
pub mod doctor;
pub mod filters;
pub mod format;
pub mod history;
pub mod hook;
pub mod logcmd;
pub mod mcp;
pub mod paths;
pub mod permissions;
pub mod redact;
pub mod repomap;
pub mod run;
pub mod scripts;
pub mod shell;
pub mod stack;
pub mod stats;
pub mod text;
