# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.0] - 2026-11-25

### Added

- Script wrappers: `npm|pnpm|yarn|bun run <script>`, `npm test`, `pnpm <script>`,
  `make <target>`, `just <recipe>` and `task <name>`. The hook resolves what the
  script runs (`package.json`, one-line Makefile rules and justfile recipes)
  and uses that command's filter. Unresolved wrappers with a terminating name
  (`test`, `build`, `lint`, `check`, `ci`...) get the new `script` filter, which
  picks a filter from the output signature (`Filter::sniff`). Server and watch
  scripts (`dev`, `start`, `serve`, `watch`...) are never wrapped.
- Pipelines: `cmd 2>&1`, `cmd 2>&1 | tail -N`, `cmd 2>&1 | head -N` and
  `cmd 2>&1 | grep ...`. The pipe stays in the shell, so the exit status is
  unchanged; `hk run --piped` prints the log path first and last; for grep the
  grep stage is wrapped so it still sees the raw output. Each stage is checked
  against the user's permission rules.
- `lint` filter: `cargo clippy`, `eslint`, `ruff check`, `biome check|lint|ci`,
  `mypy`, `pyright`, `golangci-lint run`. Machine formats are never wrapped.
- `git` filter: long `git diff`, `git show` and unbounded `git log`. Scripted
  forms (`--porcelain`, `-z`, `--format`, `--stat`, `-n N`...) are never wrapped.
- `docker` filter: `docker build`, `docker buildx build`, `docker compose build`,
  and `docker compose up` when detached or self-terminating.
- `logs` filter: `docker logs`, `docker compose logs`, `kubectl logs`,
  `journalctl`, `curl -v`. Follow modes are never wrapped.
- `listing` filter: `find`, `ls -R`, `tree`, `du`, with a heavy-directory hint.
- `jvm` (Maven, Gradle), `dotnet` (build, test), `rspec`, `phpunit`, `mix_test`
  and `playwright` filters.
- `bundle exec` is unwrapped like `npx` and `uv run`.

### Changed

- `cargo clippy` moved from the `build` filter to the new `lint` filter.
- `js_test` no longer claims `npm test` / `pnpm test` / `yarn test` /
  `bun run test` by name; those go through script resolution instead, so a
  `test` script that runs pytest or cargo gets the right filter.

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

[Unreleased]: https://github.com/Rixmerz/hook-n-hooks/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/Rixmerz/hook-n-hooks/compare/v0.1.0...v1.0.0
[0.1.0]: https://github.com/Rixmerz/hook-n-hooks/releases/tag/v0.1.0
