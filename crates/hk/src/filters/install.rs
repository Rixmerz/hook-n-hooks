//! Package installers: npm/pnpm/yarn/bun install, cargo fetch, pip, uv, poetry.
//!
//! Drops progress lines and deprecation noise; keeps the result summary,
//! errors and non-deprecation warnings. Long package lists are capped.

use super::{Cmd, Filter};
use crate::config::Limits;
use crate::text::{dedup, head_tail};

pub struct Install;

impl Filter for Install {
    fn name(&self) -> &'static str {
        "install"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        let sub = cmd.subcommand();
        match cmd.program.as_str() {
            "npm" => matches!(sub, Some("install" | "i" | "ci" | "add" | "update")),
            "pnpm" | "yarn" | "bun" => {
                // Bare `yarn` installs.
                (cmd.program == "yarn" && cmd.args.is_empty())
                    || matches!(sub, Some("install" | "i" | "add" | "update" | "up"))
            }
            "cargo" => matches!(sub, Some("fetch")),
            "pip" | "pip3" => matches!(sub, Some("install")),
            "uv" => {
                matches!(sub, Some("sync" | "add"))
                    || (sub == Some("pip") && cmd.arg(1) == Some("install"))
            }
            "poetry" => matches!(sub, Some("install" | "add" | "update")),
            _ => false,
        }
    }

    fn filter(&self, output: &str, _exit: i32, limits: &Limits) -> Option<String> {
        let mut kept = Vec::new();
        let mut dropped_deprecations = 0usize;
        let mut pkg_lines = 0usize;
        for line in output.lines() {
            let t = line.trim();
            if t.is_empty() || is_progress(t) {
                continue;
            }
            if is_deprecation(t) {
                dropped_deprecations += 1;
                continue;
            }
            // `+ requests==2.34.2`, `+ left-pad 1.3.0`: keep the first few.
            if t.starts_with("+ ") || t.starts_with("- ") {
                pkg_lines += 1;
                if pkg_lines > 10 {
                    continue;
                }
            }
            kept.push(t.to_string());
        }
        let mut kept = dedup(kept);
        if pkg_lines > 10 {
            kept.push(format!("+{} more package lines", pkg_lines - 10));
        }
        if dropped_deprecations > 0 {
            kept.push(format!(
                "({dropped_deprecations} deprecation warning(s) hidden)"
            ));
        }
        let text = kept.join("\n");
        Some(head_tail(&text, limits.generic_max_lines, limits.head, limits.tail).unwrap_or(text))
    }
}

fn is_progress(t: &str) -> bool {
    let lower = t.to_ascii_lowercase();
    lower.starts_with("progress:")
        || lower.starts_with("downloading ")
        || lower.starts_with("downloaded ")
        || lower.starts_with("fetching ")
        || lower.starts_with("resolving ")
        || lower.starts_with("collecting ")
        || lower.starts_with("using cached ")
        || lower.starts_with("prepared ")
        || lower.starts_with("updating crates.io index")
        || lower.starts_with("packages are hard linked")
        || lower.starts_with("content-addressable store")
        || lower.starts_with("virtual store is at")
        || lower.starts_with("npm notice")
        || (lower.starts_with("packages: ") && lower.len() < 40)
        || t.chars().all(|c| matches!(c, '+' | '-' | '#' | '=' | '.' | ' ' | '|'))
        || t.contains('━')
        || t.contains('█')
        || t.starts_with(['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'])
        // Fragments left by cursor-movement redraws (`5, reused 0, downloaded 2`).
        || (lower.contains("reused") && lower.contains("downloaded") && !lower.contains("done"))
        || lower.starts_with("warning: failed to hardlink")
        || lower.starts_with("if the cache and target directories")
        || lower.starts_with("if this is intentional, set `export uv_link_mode")
}

fn is_deprecation(t: &str) -> bool {
    let lower = t.to_ascii_lowercase();
    lower.contains("deprecated") && (lower.contains("warn") || lower.starts_with("npm warn"))
}
