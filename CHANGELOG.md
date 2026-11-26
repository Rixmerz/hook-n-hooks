# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.1.1] - 2026-11-26

### Fixed

- Launcher: before the binary was cached, `bin/hk` probed the `hk` on PATH,
  which inside Claude Code is the launcher itself (the plugin's `bin/` is on
  PATH), so it recursed forever and left `hk version` processes hanging. A
  probe now runs with `HK_LAUNCHER_PROBE=1` and a launcher in that mode exits
  at once instead of probing or downloading.

## [1.1.0] - 2026-11-26

### Added

- Command lists: `a && b`, `a; b`, `a || b` and newlines are rewritten command
  by command; `cd DIR` parts are followed for script resolution. Heredocs,
  background `&`, comments and subshells are still left alone.
- Pipes without `2>&1`: `cmd | tail -N`, `cmd | head -N` and `cmd | grep`.
  `hk run --stdout-only` captures stdout only and lets stderr through, so the
  next stage sees exactly what it saw before.
- `hide exec NAME -- 'cmd'` is recognised by its inner command.
- Raw escape: `HK_RAW=1 cmd` or `# hk:raw` skips the rewrite.
- MCP compaction: a PostToolUse hook shortens large MCP results with
  `updatedToolOutput`, keeping their shape; the full result goes to a log.
  Configured in `[mcp]`.
- `hk misses`: a PostToolUse(Bash) hook records large outputs hk did not wrap,
  with the reason, so filters can be chosen from data. Configured in `[misses]`.
- `hk explain "<cmd>"`: which filter would wrap a command, or why not.
- `hk log last|list|path [--session] [--grep] [--lines A:B]`: read part of a
  log without opening the file or quoting the macOS path.
- `hk doctor` and an error log: hooks still fail open, but record what went
  wrong (including panics); the next session start mentions new errors.
- Version check: the session start says when the plugin cache holds a newer
  version than the running binary.
- `hk capture <log> <tool> <case>`: turn a log into an anonymised fixture.
- Every wrapped run ends with `[hk] filter=… exit=… time=…s omitted=…`.
- Test re-runs show `[hk] vs last run: N new, M fixed` with the failure names.
- Coverage totals (istanbul `All files`, Jest `text-summary`, coverage.py
  `TOTAL`) are kept in passing and failing test summaries.
- Filters: `angular` (`ng build`, `ng test --watch=false`), `json` (API
  responses, `gh api`, `kubectl get -o json`) and `table` (`psql -c`,
  `mysql -e`, `sqlite3`, `kubectl get`, `docker ps`/`images`).
- `[limits.commands]`: a generic-filter cap per program.
- User config `~/.config/hk/hk.toml`, merged under the project's.
- `hk stats --project`, `--json`, and a per-project breakdown; MCP savings
  count under `mcp`.
- Repo map: git branch, ahead/behind, uncommitted count, last 3 commits and
  worktree origin; the commands CI runs (Bitbucket, GitLab, GitHub); one line
  per repository in a folder of repositories; a short map after compaction.
- Formatting: `format.when = "stop"` formats touched files once when Claude
  stops; `prettierd` and `eslint_d` are used when present.

### Changed

- Formatting runs formatters only by default; fixers (`eslint --fix`,
  `ruff check --fix`, `biome check --write`) need `format.fix = true`.
- `format.only_clean` (default on): when formatting would change lines outside
  the edit, the edit is kept as written and hk says so.
- Lockfiles, migrations, fixtures, snapshots, vendored, generated and minified
  files are never formatted; `format.exclude` adds more. JS tools run from the
  project root so `.prettierignore` applies.
- Logs and stats are written `0600` in `0700` directories, with known secret
  shapes masked. Log names carry the session.
- Log pruning: by age (7 days); above 1,000 logs the oldest go first, never
  the running session's or the last hour's.
- The repo map hides git-ignored dot directories and SQLite/lock side files,
  and lists up to 40 entries in `src/`, `tests/` and similar directories.
- README: the data directory is documented per OS.

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
  `test` script that runs pytest or cargo gets the right filter. A script that
  runs bare `vitest` is wrapped (without a TTY it runs once); `vitest --watch`
  is not.
- `make`, `just` and `task` listed in `rewrite.generic_commands` are no longer
  wrapped for server-like targets (`dev`, `run`, `serve`, `start`, `watch`...).
- (new in this release) `mvn` and `gradle` goals that start a server (`spring-boot:run`,
  `quarkus:dev`, `exec:java`, `bootRun`, `run`) are never wrapped.
- `playwright test` is only wrapped with an explicit terminal reporter
  (`--reporter=list|line|dot|github`): the default `html` reporter serves the
  report after a failure and never exits.

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

[Unreleased]: https://github.com/Rixmerz/hook-n-hooks/compare/v1.1.1...HEAD
[1.1.1]: https://github.com/Rixmerz/hook-n-hooks/compare/v1.1.0...v1.1.1
[1.1.0]: https://github.com/Rixmerz/hook-n-hooks/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/Rixmerz/hook-n-hooks/compare/v0.1.0...v1.0.0
[0.1.0]: https://github.com/Rixmerz/hook-n-hooks/releases/tag/v0.1.0
