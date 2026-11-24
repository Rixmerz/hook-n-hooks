//! Stack auto-detection from manifest files, and the build/test/lint
//! commands each stack resolves to.

use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stack {
    pub name: &'static str,
    pub manifest: &'static str,
    pub build: Option<String>,
    pub test: Option<String>,
    pub lint: Option<String>,
}

pub fn detect(dir: &Path) -> Vec<Stack> {
    let mut out = Vec::new();
    if dir.join("Cargo.toml").is_file() {
        out.push(Stack {
            name: "rust",
            manifest: "Cargo.toml",
            build: Some("cargo build".into()),
            test: Some("cargo test".into()),
            lint: Some("cargo clippy".into()),
        });
    }
    if let Some(s) = node(dir) {
        out.push(s);
    }
    if let Some(s) = deno(dir) {
        out.push(s);
    }
    if dir.join("pyproject.toml").is_file() {
        let py = std::fs::read_to_string(dir.join("pyproject.toml")).unwrap_or_default();
        let runner = if dir.join("uv.lock").is_file() {
            "uv run "
        } else if dir.join("poetry.lock").is_file() {
            "poetry run "
        } else {
            ""
        };
        out.push(Stack {
            name: "python",
            manifest: "pyproject.toml",
            build: None,
            test: Some(format!("{runner}pytest")),
            lint: py
                .contains("[tool.ruff")
                .then(|| format!("{runner}ruff check")),
        });
    }
    if dir.join("go.mod").is_file() {
        out.push(Stack {
            name: "go",
            manifest: "go.mod",
            build: Some("go build ./...".into()),
            test: Some("go test ./...".into()),
            lint: Some("go vet ./...".into()),
        });
    }
    out
}

fn node(dir: &Path) -> Option<Stack> {
    let s = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let pkg: Value = serde_json::from_str(&s).unwrap_or(Value::Null);
    let pm = if dir.join("pnpm-lock.yaml").is_file() {
        "pnpm"
    } else if dir.join("bun.lockb").is_file() || dir.join("bun.lock").is_file() {
        "bun"
    } else if dir.join("yarn.lock").is_file() {
        "yarn"
    } else {
        "npm"
    };
    let script = |name: &str| -> Option<String> {
        pkg.get("scripts")?.get(name)?.as_str()?;
        Some(if name == "test" && pm != "bun" {
            format!("{pm} test")
        } else {
            format!("{pm} run {name}")
        })
    };
    let ts = dir.join("tsconfig.json").is_file();
    Some(Stack {
        name: if ts { "node+typescript" } else { "node" },
        manifest: "package.json",
        build: script("build").or_else(|| ts.then(|| "npx tsc --noEmit".into())),
        test: script("test"),
        lint: script("lint"),
    })
}

fn deno(dir: &Path) -> Option<Stack> {
    let manifest = ["deno.json", "deno.jsonc"]
        .into_iter()
        .find(|m| dir.join(m).is_file())?;
    let s = std::fs::read_to_string(dir.join(manifest)).unwrap_or_default();
    let cfg: Value = serde_json::from_str(&s).unwrap_or(Value::Null);
    let task = |name: &str| -> Option<String> {
        cfg.get("tasks")?.get(name)?;
        Some(format!("deno task {name}"))
    };
    Some(Stack {
        name: "deno",
        manifest: if manifest == "deno.json" {
            "deno.json"
        } else {
            "deno.jsonc"
        },
        build: task("build").or_else(|| Some("deno check .".into())),
        test: task("test").or_else(|| Some("deno test".into())),
        lint: task("lint").or_else(|| Some("deno lint".into())),
    })
}
