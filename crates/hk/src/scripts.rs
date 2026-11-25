//! Script wrappers: `npm|pnpm|yarn|bun run <script>`, `npm test`,
//! `make <target>`, `just <recipe>` and `task <name>`.
//!
//! The wrapper itself says nothing about the output, so the hook tries to
//! find out what the script really runs, when that is cheap:
//!
//! - `package.json` `scripts.<name>`;
//! - a `Makefile` rule whose recipe is a single line;
//! - a `justfile` recipe whose body is a single line.
//!
//! The resolved body must itself be a simple command (no `&&`, `$`, pipes);
//! anything else stays unresolved, and we never guess. An unresolved wrapper
//! is only wrapped when its name is in a small allowlist of targets that
//! terminate (`test`, `build`, `lint`...); the `script` filter then picks the
//! right filter from the output signature (see [`crate::filters::script`]).

use crate::filters::{is_watch_mode, Cmd};
use crate::shell;
use std::path::{Path, PathBuf};

/// Script or target names that run a server or a watcher: never wrapped,
/// whatever they resolve to, because `hk run` buffers output until exit.
const LONG_RUNNING: &[&str] = &[
    "dev",
    "serve",
    "server",
    "start",
    "watch",
    "preview",
    "run",
    "up",
    "shell",
    "repl",
    "storybook",
];

/// Names that are safe to wrap even when we cannot see what they run.
const SAFE_UNRESOLVED: &[&str] = &[
    "test",
    "tests",
    "check",
    "lint",
    "build",
    "ci",
    "typecheck",
    "type-check",
    "verify",
    "compile",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Package,
    Make,
    Just,
    Task,
}

/// A recognised wrapper invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrapper {
    pub kind: Kind,
    /// Script/target name. Empty for a bare `make`/`just`/`task`.
    pub name: String,
    /// Arguments forwarded to the script (`npm test -- -x` gives `["-x"]`).
    pub extra: Vec<String>,
    /// True when the wrapper was given options we do not model (`make -C dir`).
    pub opaque: bool,
}

/// Recognise a wrapper. `pnpm <name>` and `yarn <name>` shorthands are only
/// recognised as scripts when `package.json` (read lazily through
/// `has_script`) defines that name.
pub fn wrapper(cmd: &Cmd, has_script: &dyn Fn(&str) -> bool) -> Option<Wrapper> {
    let a: Vec<&str> = cmd.args.iter().map(String::as_str).collect();
    let pkg = |name: &str, rest: &[&str]| {
        let extra = match rest.first() {
            Some(&"--") => &rest[1..],
            _ => rest,
        };
        Some(Wrapper {
            kind: Kind::Package,
            name: name.to_string(),
            extra: extra.iter().map(|s| s.to_string()).collect(),
            opaque: false,
        })
    };
    match (cmd.program.as_str(), a.as_slice()) {
        ("npm", ["test" | "t" | "tst", rest @ ..]) => pkg("test", rest),
        ("npm", ["run" | "run-script" | "rum" | "urn", name, rest @ ..]) => pkg(name, rest),
        ("pnpm" | "yarn", ["test" | "t", rest @ ..]) => pkg("test", rest),
        ("pnpm" | "yarn" | "bun", ["run", name, rest @ ..]) if !name.starts_with('-') => {
            pkg(name, rest)
        }
        ("pnpm" | "yarn", [name, rest @ ..]) if !name.starts_with('-') && has_script(name) => {
            pkg(name, rest)
        }
        ("make" | "gmake" | "just" | "task", _) => {
            let kind = match cmd.program.as_str() {
                "just" => Kind::Just,
                "task" => Kind::Task,
                _ => Kind::Make,
            };
            let mut names = Vec::new();
            let mut opaque = false;
            for x in &a {
                if x.starts_with('-') {
                    // `-j8`, `-k`, `-s` are harmless; anything else may change
                    // the file or directory we would read.
                    let harmless = matches!(kind, Kind::Make)
                        && (x.starts_with("-j") || matches!(*x, "-k" | "-s" | "--silent"));
                    opaque |= !harmless;
                } else if x.contains('=') {
                    // `make test V=1`, `just test arg=x`: variables.
                    opaque |= kind != Kind::Make;
                } else {
                    names.push(*x);
                }
            }
            // `make a b` runs two targets; `just test arg` passes arguments.
            opaque |= names.len() > 1;
            Some(Wrapper {
                kind,
                name: names.first().map(|s| s.to_string()).unwrap_or_default(),
                extra: Vec::new(),
                opaque,
            })
        }
        _ => None,
    }
}

/// What the hook should do with a wrapper.
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// The script runs this command: pick its filter.
    Resolved(Cmd),
    /// Wrap with the output-sniffing `script` filter.
    Sniff,
    /// Leave the command alone.
    Skip,
}

pub fn decide(w: &Wrapper, dir: &Path) -> Decision {
    if is_long_running(&w.name) {
        return Decision::Skip;
    }
    if !w.opaque {
        if let Some(cmd) = resolve(w, dir, 0) {
            if is_watch_mode(&cmd) {
                return Decision::Skip;
            }
            return Decision::Resolved(cmd);
        }
    }
    decide_unresolved(w)
}

/// The decision from the script name alone.
pub fn decide_unresolved(w: &Wrapper) -> Decision {
    let name = w.name.as_str();
    if !is_long_running(name) && SAFE_UNRESOLVED.contains(&name) {
        Decision::Sniff
    } else {
        Decision::Skip
    }
}

fn is_long_running(name: &str) -> bool {
    LONG_RUNNING.contains(&name) || name.contains("watch") || name.starts_with("dev:")
}

const MAX_DEPTH: usize = 3;

/// Resolve a wrapper to the command its script runs, following nested
/// wrappers (`make test` -> `npm test` -> `vitest run`).
pub fn resolve(w: &Wrapper, dir: &Path, depth: usize) -> Option<Cmd> {
    if depth >= MAX_DEPTH || w.name.is_empty() {
        return None;
    }
    let body = match w.kind {
        Kind::Package => package_script(dir, &w.name)?,
        Kind::Make => make_recipe(&std::fs::read_to_string(makefile(dir)?).ok()?, &w.name)?,
        Kind::Just => just_recipe(&std::fs::read_to_string(justfile(dir)?).ok()?, &w.name)?,
        Kind::Task => return None,
    };
    let simple = shell::parse(&body)?;
    if !simple.prefix.is_empty() || simple.rest.contains('$') {
        return None;
    }
    let mut words = simple.words;
    words.extend(w.extra.iter().cloned());
    let cmd = Cmd::from_argv(&words)?;
    let has = |n: &str| package_script(dir, n).is_some();
    match wrapper(&cmd, &has) {
        Some(inner) if !inner.opaque => resolve(&inner, dir, depth + 1),
        Some(_) => None,
        None => Some(cmd),
    }
}

/// `scripts.<name>` from `dir/package.json`.
pub fn package_script(dir: &Path, name: &str) -> Option<String> {
    let s = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v.get("scripts")?.get(name)?.as_str().map(String::from)
}

fn makefile(dir: &Path) -> Option<PathBuf> {
    ["GNUmakefile", "makefile", "Makefile"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
}

fn justfile(dir: &Path) -> Option<PathBuf> {
    ["justfile", "Justfile", ".justfile"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
}

/// The single recipe line of `target:` in a Makefile, without its `@`/`-`
/// prefix. `None` when the target is missing, has several recipe lines,
/// uses variables, or is defined more than once.
pub fn make_recipe(src: &str, target: &str) -> Option<String> {
    let lines: Vec<&str> = src.lines().collect();
    let mut found: Option<String> = None;
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with('\t') || line.starts_with('#') {
            continue;
        }
        let Some((head, after)) = line.split_once(':') else {
            continue;
        };
        if after.starts_with('=') || after.starts_with(':') {
            continue; // `X := v`, `X ::= v`
        }
        if !head.split_whitespace().any(|t| t == target) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        let recipe: Vec<&str> = lines[i + 1..]
            .iter()
            .take_while(|l| l.starts_with('\t'))
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        let inline = after.split_once(';').map(|(_, r)| r.trim());
        let body = match (inline, recipe.as_slice()) {
            (Some(r), []) if !r.is_empty() => r,
            (None, [one]) => one,
            _ => return None,
        };
        let body = body.trim_start_matches(['@', '-', '+']).trim();
        if body.contains('$') {
            return None;
        }
        found = Some(body.to_string());
    }
    found
}

/// The single body line of `recipe:` (or `recipe arg:`) in a justfile.
pub fn just_recipe(src: &str, recipe: &str) -> Option<String> {
    let lines: Vec<&str> = src.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with(char::is_whitespace) || line.starts_with('#') {
            continue;
        }
        let Some((head, after)) = line.split_once(':') else {
            continue;
        };
        if after.starts_with('=') {
            continue;
        }
        let head = head.trim_start_matches('@');
        if head.split_whitespace().next() != Some(recipe) {
            continue;
        }
        if head.split_whitespace().count() > 1 || !after.trim().is_empty() {
            return None; // parameters or dependencies: not worth modelling
        }
        let body: Vec<&str> = lines[i + 1..]
            .iter()
            .take_while(|l| l.starts_with(char::is_whitespace) || l.is_empty())
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        let [one] = body.as_slice() else {
            return None;
        };
        let one = one.trim_start_matches(['@', '-']).trim();
        if one.contains('{') || one.contains('$') {
            return None;
        }
        return Some(one.to_string());
    }
    None
}

/// Directory named by a `cd DIR && ` prefix, relative to `cwd`.
pub fn prefix_dir(cwd: &Path, prefix: &str) -> Option<PathBuf> {
    if prefix.is_empty() {
        return Some(cwd.to_path_buf());
    }
    let d = prefix
        .trim()
        .strip_prefix("cd ")?
        .strip_suffix("&&")?
        .trim()
        .trim_matches(['\'', '"']);
    if d.starts_with('~') || d.contains('$') {
        return None;
    }
    Some(cwd.join(d))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(s: &str) -> Cmd {
        let v: Vec<String> = s.split_whitespace().map(String::from).collect();
        Cmd::from_argv(&v).unwrap()
    }

    #[test]
    fn recognises_wrappers() {
        let none = |_: &str| false;
        let w = wrapper(&cmd("npm test -- -x"), &none).unwrap();
        assert_eq!(
            (w.name.as_str(), w.extra.clone()),
            ("test", vec!["-x".into()])
        );
        assert_eq!(wrapper(&cmd("npm run lint"), &none).unwrap().name, "lint");
        assert_eq!(wrapper(&cmd("bun run build"), &none).unwrap().name, "build");
        assert!(wrapper(&cmd("pnpm lint"), &none).is_none());
        assert_eq!(wrapper(&cmd("pnpm lint"), &|_| true).unwrap().name, "lint");
        assert!(wrapper(&cmd("bun test"), &none).is_none()); // bun's own runner
        let m = wrapper(&cmd("make -j8 test V=1"), &none).unwrap();
        assert_eq!((m.name.as_str(), m.opaque), ("test", false));
        assert!(wrapper(&cmd("make -C sub test"), &none).unwrap().opaque);
        assert!(wrapper(&cmd("cargo test"), &none).is_none());
    }

    #[test]
    fn reads_make_and_just_recipes() {
        let mk = "X := 1\n.PHONY: test lint\ntest: build\n\t@cargo test --all\n\nlint:\n\tcargo fmt\n\tcargo clippy\nvars:\n\t$(CARGO) test\none: ; pytest -q\n";
        assert_eq!(make_recipe(mk, "test").as_deref(), Some("cargo test --all"));
        assert_eq!(make_recipe(mk, "lint"), None);
        assert_eq!(make_recipe(mk, "vars"), None);
        assert_eq!(make_recipe(mk, "one").as_deref(), Some("pytest -q"));
        assert_eq!(make_recipe(mk, "missing"), None);
        let jf = "set shell := [\"bash\"]\n\n# run tests\ntest:\n    go test ./...\n\nbuild arg:\n    go build {{arg}}\n";
        assert_eq!(just_recipe(jf, "test").as_deref(), Some("go test ./..."));
        assert_eq!(just_recipe(jf, "build"), None);
    }

    #[test]
    fn decides_from_package_json() {
        let dir = std::env::temp_dir().join(format!("hk-scripts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"scripts":{"test":"vitest run","lint":"eslint .","dev":"vite","x":"a && b","cov":"npm test","w":"jest --watch"}}"#,
        )
        .unwrap();
        let none = |_: &str| false;
        let d = |s: &str| decide(&wrapper(&cmd(s), &none).unwrap(), &dir);
        assert_eq!(d("npm test"), Decision::Resolved(cmd("vitest run")));
        assert_eq!(d("npm run lint"), Decision::Resolved(cmd("eslint .")));
        assert_eq!(d("npm run cov"), Decision::Resolved(cmd("vitest run")));
        assert_eq!(d("npm run dev"), Decision::Skip);
        assert_eq!(d("npm run w"), Decision::Skip);
        assert_eq!(d("npm run x"), Decision::Skip);
        assert_eq!(d("npm run build"), Decision::Sniff);
        assert_eq!(d("make test"), Decision::Sniff);
        assert_eq!(d("make"), Decision::Skip);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_prefix_dir() {
        let cwd = Path::new("/r");
        assert_eq!(prefix_dir(cwd, ""), Some(PathBuf::from("/r")));
        assert_eq!(prefix_dir(cwd, "cd web && "), Some(cwd.join("web")));
        assert_eq!(prefix_dir(cwd, "cd ~/x && "), None);
    }
}
