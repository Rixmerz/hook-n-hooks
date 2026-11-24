# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-11-23

### Added

- `hk` binary with `run`, `hook`, `stats`, `filters`, `map` and `version`
  subcommands.
- PreToolUse(Bash) rewrite of simple commands to `hk run -- <cmd>`. The full log
  is saved locally, its path is printed first, and the exit code is kept exactly.
- Filters: `cargo_test`, `pytest`, `js_test` (vitest, jest), `go_test`,
  `deno_bun_test`, `build` (cargo build/check/clippy, tsc, go build/vet),
  `install` (npm, pnpm, yarn, bun, cargo fetch, pip, uv, poetry) and a
  `generic` head/tail fallback.
- The user's own Bash `allow`/`deny`/`ask` rules are checked against the original
  command before rewriting.
- PostToolUse(Edit|Write|MultiEdit): format and autofix only the touched file
  with rustfmt, gofmt, ruff, biome, prettier or eslint, when installed.
- SessionStart repo map: 2-level tree, detected stack, and resolved
  build/test/lint commands.
- Local-only `hk stats`: estimated tokens saved per filter and per session.
- Optional `.claude/hk.toml` configuration.
- Plugin manifest, marketplace, and a launcher that downloads the release binary
  and verifies its SHA256.
- CI on Linux, macOS and Windows, and a release workflow for six targets.

[Unreleased]: https://github.com/Rixmerz/hook-n-hooks/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Rixmerz/hook-n-hooks/releases/tag/v0.1.0
