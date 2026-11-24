//! SessionStart repo map: a 2-level tree, the detected stack and the
//! resolved build/test/lint commands, so Claude does not spend tool calls
//! (and tokens) rediscovering them.

use std::path::Path;

/// Directories never worth listing.
const SKIP: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "dist",
    "build",
    "out",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".next",
    ".nuxt",
    ".turbo",
    ".cache",
    "coverage",
    "vendor",
    ".idea",
    ".vscode",
    ".DS_Store",
];

/// Entries listed per directory before `+N more`.
const PER_DIR: usize = 15;

/// Hard cap on the injected text (Claude Code caps hook context at 10,000 chars).
const MAX_CHARS: usize = 4000;

pub fn render(root: &Path) -> String {
    let mut out = String::from("[hk] repo map\n");
    let stacks = crate::stack::detect(root);
    if stacks.is_empty() {
        out.push_str("stack: unknown\n");
    } else {
        let names: Vec<&str> = stacks.iter().map(|s| s.name).collect();
        out.push_str(&format!("stack: {}\n", names.join(", ")));
        for s in &stacks {
            let mut cmds = Vec::new();
            for (k, v) in [("build", &s.build), ("test", &s.test), ("lint", &s.lint)] {
                if let Some(v) = v {
                    cmds.push(format!("{k}=`{v}`"));
                }
            }
            if !cmds.is_empty() {
                out.push_str(&format!(
                    "{} ({}): {}\n",
                    s.name,
                    s.manifest,
                    cmds.join(" ")
                ));
            }
        }
    }
    out.push_str("tree (2 levels):\n");
    tree(root, 0, &mut out);
    if out.len() > MAX_CHARS {
        let mut cut = MAX_CHARS;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("\n[hk] repo map truncated\n");
    }
    out
}

fn tree(dir: &Path, depth: usize, out: &mut String) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<(String, bool)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if SKIP.contains(&name.as_str()) {
                return None;
            }
            let is_dir = e.file_type().ok()?.is_dir();
            Some((name, is_dir))
        })
        .collect();
    // Directories first, then files, each alphabetically.
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let total = entries.len();
    let indent = "  ".repeat(depth + 1);
    for (name, is_dir) in entries.into_iter().take(PER_DIR) {
        if is_dir {
            out.push_str(&format!("{indent}{name}/\n"));
            if depth == 0 {
                tree(&dir.join(&name), depth + 1, out);
            }
        } else {
            out.push_str(&format!("{indent}{name}\n"));
        }
    }
    if total > PER_DIR {
        out.push_str(&format!("{indent}+{} more\n", total - PER_DIR));
    }
}
