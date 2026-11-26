//! SessionStart repo map: git state, the detected stack, the resolved
//! build/test/lint commands, what CI really runs and a 2-level tree, so
//! Claude does not spend tool calls (and tokens) rediscovering them. A folder
//! that only groups several repositories gets one line per repository.

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
/// Source and test directories get more room: they are what Claude opens.
const PER_SOURCE_DIR: usize = 40;
const SOURCE_DIRS: &[&str] = &["src", "lib", "app", "tests", "test", "spec", "pkg", "cmd"];

/// Hard cap on the injected text (Claude Code caps hook context at 10,000 chars).
const MAX_CHARS: usize = 4000;

pub fn render(root: &Path) -> String {
    render_with(root, false)
}

/// `short`: after a compaction, the summary already carries the context; the
/// stack and git lines are enough.
pub fn render_with(root: &Path, short: bool) -> String {
    let mut out = String::from("[hk] repo map\n");
    if let Some(g) = git_line(root) {
        out.push_str(&g);
    }
    let stacks = crate::stack::detect(root);
    if stacks.is_empty() {
        let repos = sub_repos(root);
        if repos.len() >= 2 {
            out.push_str(&format!(
                "folder with {} repositories (not a repo itself):\n",
                repos.len()
            ));
            for r in repos.iter().take(25) {
                out.push_str(&repo_line(r));
            }
            if repos.len() > 25 {
                out.push_str(&format!("  +{} more\n", repos.len() - 25));
            }
            return cap(out);
        }
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
    if let Some(ci) = ci_line(root) {
        out.push_str(&ci);
    }
    if short {
        return cap(out);
    }
    out.push_str("tree (2 levels):\n");
    let ignored = gitignored_names(root);
    tree(root, 0, &ignored, &mut out);
    cap(out)
}

fn cap(mut out: String) -> String {
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

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let o = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
}

/// `git: main ↑2 ↓1, 3 uncommitted | last: abc123 msg; def456 msg`.
fn git_line(root: &Path) -> Option<String> {
    let status = git(root, &["status", "--porcelain=v1", "-b"])?;
    let mut lines = status.lines();
    let head = lines.next()?.trim_start_matches("## ").to_string();
    let dirty = lines.count();
    // `main...origin/main [ahead 2, behind 1]` -> `main ↑2 ↓1`.
    let (branch, track) = match head.split_once("...") {
        Some((b, rest)) => (b.to_string(), rest.to_string()),
        None => (head.clone(), String::new()),
    };
    let num = |key: &str| -> Option<String> {
        let i = track.find(key)? + key.len();
        Some(
            track[i..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect(),
        )
    };
    let mut s = format!("git: {branch}");
    if let Some(n) = num("ahead ") {
        s.push_str(&format!(" ↑{n}"));
    }
    if let Some(n) = num("behind ") {
        s.push_str(&format!(" ↓{n}"));
    }
    if dirty > 0 {
        s.push_str(&format!(", {dirty} uncommitted"));
    }
    // A linked worktree: say which checkout it belongs to.
    if root.join(".git").is_file() {
        if let Some(common) = git(
            root,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ) {
            let main = Path::new(&common).parent().map(|p| p.display().to_string());
            if let Some(m) = main {
                s.push_str(&format!(" (worktree of {m})"));
            }
        }
    }
    if let Some(log) = git(root, &["log", "-3", "--format=%h %s"]) {
        let commits: Vec<String> = log.lines().map(|l| l.chars().take(70).collect()).collect();
        if !commits.is_empty() {
            s.push_str(&format!(" | last: {}", commits.join("; ")));
        }
    }
    s.push('\n');
    Some(s)
}

/// Direct children that are git repositories.
fn sub_repos(root: &Path) -> Vec<std::path::PathBuf> {
    if root.join(".git").exists() {
        return Vec::new();
    }
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut v: Vec<_> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join(".git").exists())
        .collect();
    v.sort();
    v
}

fn repo_line(dir: &Path) -> String {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let branch = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|| "?".into());
    let stacks = crate::stack::detect(dir);
    let mut s = format!("  {name}/ [{branch}]");
    if let Some(st) = stacks.first() {
        s.push_str(&format!(" {}", st.name));
        if let Some(t) = &st.test {
            s.push_str(&format!(" test=`{t}`"));
        }
        if let Some(b) = &st.build {
            s.push_str(&format!(" build=`{b}`"));
        }
    }
    s.push('\n');
    s
}

/// Commands the CI runs: `script:` items of `bitbucket-pipelines.yml` and
/// `.gitlab-ci.yml`, `run:` steps of GitHub workflows. A line scan, not a
/// YAML parser: good enough to show what to run before pushing.
fn ci_line(root: &Path) -> Option<String> {
    let mut files: Vec<(String, std::path::PathBuf)> = Vec::new();
    for f in ["bitbucket-pipelines.yml", ".gitlab-ci.yml"] {
        let p = root.join(f);
        if p.is_file() {
            files.push((f.to_string(), p));
        }
    }
    if let Ok(rd) = std::fs::read_dir(root.join(".github/workflows")) {
        let mut w: Vec<_> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        w.sort();
        for p in w.into_iter().take(3) {
            let n = format!(".github/workflows/{}", p.file_name()?.to_string_lossy());
            files.push((n, p));
        }
    }
    let mut out = String::new();
    for (name, p) in files {
        let cmds = ci_commands(&std::fs::read_to_string(&p).unwrap_or_default());
        if cmds.is_empty() {
            // Shared pipelines (`- pipe: ...`, `import:`): say where they come from.
            continue;
        }
        let shown: Vec<String> = cmds.iter().take(8).map(|c| format!("`{c}`")).collect();
        let more = if cmds.len() > 8 {
            format!(" +{} more", cmds.len() - 8)
        } else {
            String::new()
        };
        out.push_str(&format!("ci ({name}): {}{more}\n", shown.join(", ")));
    }
    (!out.is_empty()).then_some(out)
}

pub fn ci_commands(yaml: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_script: Option<usize> = None;
    for line in yaml.lines() {
        let indent = line.len() - line.trim_start().len();
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some(i) = in_script {
            if indent <= i && !t.starts_with("- ") || indent < i {
                in_script = None;
            }
        }
        let mut push = |c: &str| {
            let c = c.trim().trim_matches(['"', '\'']).trim();
            if !c.is_empty() && c != "|" && c != ">" && !out.iter().any(|x| x == c) {
                out.push(c.chars().take(80).collect());
            }
        };
        if let Some(rest) = t.strip_prefix("- run:").or_else(|| t.strip_prefix("run:")) {
            push(rest);
            continue;
        }
        if t == "script:" || t.ends_with(" script:") || t == "- script:" {
            in_script = Some(indent);
            continue;
        }
        if in_script.is_some() {
            if let Some(item) = t.strip_prefix("- ") {
                if !item.starts_with("pipe:") && !item.ends_with(':') {
                    push(item);
                }
            }
        }
    }
    out
}

/// Top-level names the root `.gitignore` excludes (`/.locks/`, `.mcp-docs`),
/// used to hide tool state from the tree. Only plain names, no globs.
fn gitignored_names(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join(".gitignore"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().trim_start_matches('/').trim_end_matches('/'))
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('!'))
        .filter(|l| !l.contains(['*', '?', '[', '/']))
        .map(String::from)
        .collect()
}

/// Files that are tool state, never worth a line: SQLite side files, locks.
fn is_state_file(name: &str) -> bool {
    [".db-shm", ".db-wal", ".lock", ".pid", ".sock"]
        .iter()
        .any(|s| name.ends_with(s))
        && !matches!(
            name,
            "Cargo.lock"
                | "flake.lock"
                | "uv.lock"
                | "poetry.lock"
                | "Gemfile.lock"
                | "composer.lock"
                | "Package.resolved"
        )
}

fn tree(dir: &Path, depth: usize, ignored: &[String], out: &mut String) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<(String, bool)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if SKIP.contains(&name.as_str()) || is_state_file(&name) {
                return None;
            }
            // Dot directories that git ignores are tool state (`.locks/`).
            if depth == 0 && name.starts_with('.') && ignored.contains(&name) {
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
    let is_source = dir
        .file_name()
        .is_some_and(|n| SOURCE_DIRS.contains(&n.to_string_lossy().as_ref()));
    let per_dir = if depth > 0 && is_source {
        PER_SOURCE_DIR
    } else {
        PER_DIR
    };
    for (name, is_dir) in entries.into_iter().take(per_dir) {
        if is_dir {
            out.push_str(&format!("{indent}{name}/\n"));
            if depth == 0 {
                tree(&dir.join(&name), depth + 1, ignored, out);
            }
        } else {
            out.push_str(&format!("{indent}{name}\n"));
        }
    }
    if total > per_dir {
        out.push_str(&format!("{indent}+{} more\n", total - per_dir));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ci_commands() {
        let bb = "pipelines:\n  default:\n    - step:\n        name: Test\n        script:\n          - npm ci\n          - npm run test -- --coverage\n          - pipe: atlassian/x\n    - step:\n        script:\n          - npm run build\n";
        assert_eq!(
            ci_commands(bb),
            vec!["npm ci", "npm run test -- --coverage", "npm run build"]
        );
        let gh = "jobs:\n  t:\n    steps:\n      - uses: actions/checkout@v4\n      - run: cargo test --locked\n      - name: lint\n        run: cargo clippy -- -D warnings\n";
        assert_eq!(
            ci_commands(gh),
            vec!["cargo test --locked", "cargo clippy -- -D warnings"]
        );
    }

    #[test]
    fn hides_tool_state_and_lists_sub_repos() {
        let d = std::env::temp_dir().join(format!("hk-map-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".locks")).unwrap();
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join(".gitignore"), "/.locks/\n").unwrap();
        std::fs::write(d.join("docs.db-shm"), "").unwrap();
        let m = render(&d);
        assert!(
            m.contains("src/") && !m.contains(".locks") && !m.contains("db-shm"),
            "{m}"
        );
        // A folder of repositories.
        let f = d.join("group");
        for r in ["a", "b"] {
            std::fs::create_dir_all(f.join(r).join(".git")).unwrap();
        }
        let m = render(&f);
        assert!(m.contains("folder with 2 repositories"), "{m}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
