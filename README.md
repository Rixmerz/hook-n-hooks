# hook-n-hooks

Token-saving hooks for Claude Code. `hook-n-hooks` is a Claude Code plugin
backed by one small Rust binary, `hk`. It cuts the tokens Claude spends reading
test runs, build errors and package installs. It formats the files Claude edits,
and it gives Claude a compact map of the repository at session start. It does
not block anything: blocking and safety are what Claude Code's own permission
modes are for.

Nothing is hidden without a trace. Whenever output is compacted, the first line
Claude sees is the path to the full log on your disk. There is no telemetry, and
stats stay on your machine.

## What it saves

| Hook | What it does |
| --- | --- |
| **PreToolUse (Bash)** | Rewrites a known command to `hk run -- <cmd>`. `hk run` runs it, saves the full log and prints a compact result. The exit code is kept exactly. |
| **PostToolUse (Edit/Write)** | Formats and autofixes **only the touched file** with rustfmt, gofmt, ruff, biome or prettier/eslint, if installed. It reports only the issues it could not fix. |
| **SessionStart** | Adds a repo map: a 2-level tree, the detected stack, and the resolved build/test/lint commands. |

The Bash filters:

| Filter | Commands | On success | On failure |
| --- | --- | --- | --- |
| `cargo_test` | `cargo test` | `✓ 342 passed` | each failing test, its panic location and assertion |
| `pytest` | `pytest`, `python -m pytest`, `uv run pytest` | `✓ 2 passed` | each failing test, `>`/`E` lines, `file:line` |
| `js_test` | `vitest run`, `jest` | `✓ 1 passed` | test name, assertion, diff, first stack frame |
| `go_test` | `go test` | `✓ 3 package(s) ok` | `--- FAIL` names plus `t.Errorf` lines |
| `deno_bun_test` | `deno test`, `bun test` | `✓ 1 passed` | test name, error, diff, location |
| `rspec`, `phpunit`, `mix_test`, `playwright` | `rspec`, `phpunit`/`pest`, `mix test`, `playwright test --reporter=list` (also `line`, `dot`, `github`) | the count line | each failing test with its assertion and location |
| `jvm` | `mvn`, `mvnw`, `gradle`, `gradlew` | `✓ mvn: BUILD SUCCESS \| Tests run: …` | failing tests, compiler errors as `file:line: msg`, "What went wrong" |
| `dotnet` | `dotnet build/test`, `msbuild` | the count line | `File.cs:10: error CS…` (de-duplicated), failing tests with message and frame |
| `build` | `cargo build/check`, `tsc`, `go build/vet` | `✓ build ok` | one `file:line: msg` per diagnostic, de-duplicated, max 20 + `+N more` |
| `lint` | `cargo clippy`, `eslint`, `ruff check`, `biome check/lint/ci`, `mypy`, `pyright`, `golangci-lint run` | `✓ lint clean` | count line, then one `file:line: msg` per issue, de-duplicated, max 20 + `+N more` |
| `install` | `npm/pnpm/yarn/bun install`, `cargo fetch`, `pip`, `uv`, `poetry` | the result lines | errors kept; progress bars and deprecation noise dropped |
| `git` | `git diff`, `git show`, unbounded `git log` | unchanged if short | long: stat summary + first diff lines, or the first commits |
| `docker` | `docker build`, `docker buildx build`, `docker compose build`, `docker compose up -d` | the result (image, tags, containers) | the failing step, the tail of its output, the error lines |
| `logs` | `docker logs`, `docker compose logs`, `kubectl logs`, `journalctl`, `curl -v` | unchanged if short | repeated lines collapsed (`(x120 similar)`), then head/tail; curl's TLS noise dropped |
| `angular` | `ng build`, `ng test --watch=false` | `✓ ng build ok: N warning(s); Initial total …` or `✓ Executed N of N SUCCESS` | `file:line:col: TS…/NG…` errors, budget warnings first, failing Karma specs |
| `json` | `curl … https://…`, `gh api`, `kubectl get -o json`, `aws`/`gcloud`/`az` with JSON output | unchanged if short | the same JSON shape: every key, 3 items per array with the total, long strings cut |
| `table` | `psql -c`, `mysql -e`, `sqlite3 db query`, `kubectl get`, `docker ps`/`images` | unchanged if short | header, first rows, row count |
| `listing` | `find`, `ls -R`, `tree`, `du` | unchanged if short | counts per directory, first entries, heavy-dir hint; `du`: largest first |
| `script` | `npm/pnpm/yarn/bun run <s>`, `npm test`, `make`, `just`, `task` when the script could not be resolved | picked from the output signature | same |
| `generic` | fallback, or commands listed in `rewrite.generic_commands` or `[limits.commands]` | output unchanged if short | head 30 + tail 30 lines |

Test filters keep the coverage totals (the istanbul `All files` row, Jest's
`text-summary`, coverage.py's `TOTAL`) even when every test passes. Build
filters keep the warning count and the warnings. When the same test command
runs again in the same project, a `[hk] vs last run: 1 new, 2 fixed` line lists
the failures that appeared and the ones that went away.

`hide exec NAME -- 'npm test'` is recognised by its inner command, and the
whole `hide` call is wrapped, so the log only ever holds hide's masked output.

ANSI colours are always stripped. If a filter does not recognise an output, the
generic filter is used instead. A "summary" that would be longer than the
original output is never used.

Scripted forms are never wrapped, because a script or the model may parse
them: `git --porcelain`, `-z`, `--format`, `--stat`, `git log -n N`, linters'
JSON/SARIF formats, `find -exec`/`-print0`, `tree -J`. Commands that never end
are never wrapped either: watch modes, `dev`/`start`/`serve` scripts, foreground
`docker compose up`, and `-f`/`--follow` logs.

### Script wrappers

`npm test`, `npm run lint`, `pnpm build`, `make test` or `just check` say nothing
about their output, so the hook looks at what they run: `scripts.<name>` in
`package.json`, a one-line Makefile rule, or a one-line justfile recipe. If that
is a simple command (`vitest run`, `eslint .`, `cargo test`), its filter is
used. Otherwise, for names that terminate (`test`, `build`, `lint`, `check`,
`ci`, `typecheck`...), the `script` filter recognises the output by its
signature. Anything else, including `npm run dev`, is left alone.

### Lists and pipes

A command line is split on `&&`, `||`, `;` and newlines, and each command in
the list is rewritten on its own; `cd DIR` parts are followed, so
`cd web && npm test && npm run build` wraps both npm commands with the filters
of `web/package.json`. Heredocs, background `&`, comments, subshells and
`$(...)` are never touched.

| Command | Becomes |
| --- | --- |
| `cargo test 2>&1` | `hk run -- cargo test 2>&1` |
| `cargo test 2>&1 \| tail -20` | `hk run --piped -- cargo test 2>&1 \| tail -20` (the log path is printed first and last, so the slice keeps it) |
| `cargo test \| tail -20` | `hk run --piped --stdout-only -- cargo test \| tail -20` (stderr goes straight to the terminal, as before; tail still sees stdout only) |
| `cargo test 2>&1 \| grep FAIL` | `cargo test 2>&1 \| hk run -- grep FAIL` (grep still sees the raw output; hk caps what grep prints) |
| `cargo build && cargo test; ls` | `hk run -- cargo build && hk run -- cargo test; ls` |

The pipe stays in the shell, so the exit status is exactly what Bash would have
reported. Every command and every stage is checked against your permission
rules.

To get the raw output of one command, prefix it with `HK_RAW=1` or end the
line with `# hk:raw`. `hk explain "<command line>"` tells you which filter
would wrap a command, or why it would not.

Every wrapped run ends with one parseable line:

```
[hk] filter=cargo_test exit=101 time=12.4s omitted=340
```

### Before and after

These numbers come from the snapshot tests over real captured outputs
(`crates/hk/tests/snapshots/`). Tokens are estimated as characters / 4.

`cargo test` with two failing tests: 909 chars (~227 tokens) become 250 chars
(~62 tokens), plus the log line.

```
running 4 tests
test tests::fails_one ... FAILED
test tests::adds_neg ... ok
...
---- tests::fails_one stdout ----

thread 'tests::fails_one' (12345) panicked at src/lib.rs:7:30:
assertion `left == right` failed: math is broken
  left: 4
 right: 5
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
...
test result: FAILED. 2 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

becomes

```
[hk] log: ~/Library/Application Support/hk/logs/1795487589516338550-1540218-abc123-cargo_test.log
✗ 2 failed, 2 passed
FAIL tests::fails_one
  panicked at src/lib.rs:7:30:
  assertion `left == right` failed: math is broken
  left: 4
  right: 5
FAIL tests::panics
  panicked at src/lib.rs:8:62:
  index out of bounds: the len is 0 but the index is 3
[hk] filter=cargo_test exit=101 time=3.2s omitted=38
```

(The log path shown is macOS; see [Data and logs](#data-and-logs) for the
other systems. You rarely need it: `hk log last --grep panicked` prints the
matching lines of the newest log.)

More examples from the fixtures:

| Fixture | Raw chars | Compact chars |
| --- | ---: | ---: |
| `cargo test`, passing | 663 | 10 |
| `pytest`, passing | 369 | 10 |
| `pytest`, 1 failure | 810 | 163 |
| `cargo build`, 3 errors | 1249 | 168 |
| `vitest run`, 1 failure | 726 | 183 |
| `pnpm install` with progress bars | 1519 | 136 |

The log line adds about 80 characters to every wrapped command. Real projects
produce far longer outputs than these small fixtures, so the savings grow with
the size of the suite. Run `hk stats` to see your own numbers.

## Install

In Claude Code:

```
/plugin marketplace add Rixmerz/hook-n-hooks
/plugin install hook-n-hooks@hook-n-hooks
```

On Claude Code 2.1.275 or later, this one line does both:

```
/plugin install hook-n-hooks --marketplace Rixmerz/hook-n-hooks
```

On its first run, the launcher (`bin/hk`) downloads the prebuilt `hk` binary for
your platform from GitHub Releases and verifies its SHA256. It caches the binary
under `${CLAUDE_PLUGIN_DATA}`. After that, each hook is a single `exec`.
Prebuilt targets: Linux (musl) and macOS for x86_64 and aarch64, and Windows
(MSVC) for x86_64 and aarch64.

To build from source instead, or if the download is not possible:

```
cargo install --git https://github.com/Rixmerz/hook-n-hooks hk
```

The launcher uses an `hk` on `PATH` when its version matches the plugin's. You
can also point it at a binary with `HK_BIN=/path/to/hk`.

**Windows:** Claude Code runs hook commands through Git Bash, which the launcher
needs. Without Git Bash, Claude Code has no Bash tool, so there are no Bash
commands to compact. Formatting and the repo map then need `HK_BIN` set or an
`hk.exe` on `PATH`. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#windows).

## Permissions

Claude Code checks permission rules against the rewritten command, not the one
Claude wrote. Before it rewrites a command, `hk` checks the original command
against your own rules in `~/.claude/settings.json`, `.claude/settings.json` and
`.claude/settings.local.json`:

- **A `deny` or `ask` rule matches:** the command is not rewritten, so your rule
  keeps working.
- **An `allow` rule matches** (for example `Bash(cargo test:*)`): the command is
  rewritten, and `hk` passes your decision on as `allow`. `hk` never allows a
  command your rules did not allow.
- **No rule matches:** in `auto` and `bypassPermissions` mode the command is
  rewritten and Claude Code's normal checks still apply. In other modes it is
  left alone, so the approval prompt shows the command Claude wrote. To change
  this, set `rewrite.unapproved = "rewrite"`.

Rules you pass on the command line (`--allowedTools`) and managed settings are
not visible to hooks.

## MCP results

Large MCP tool results (over 8,000 characters of JSON by default) are
shortened in a PostToolUse hook with `updatedToolOutput`. The result keeps its
shape, as Claude Code requires: only long strings change. A string that holds
JSON becomes the same JSON with 3 items per array and the totals; other text
keeps its head and tail. The full result is saved to a log whose path is the
first line of the shortened text. Turn it off, or skip some tools, in
`[mcp]`.

## Formatting

After `Edit`, `Write` and `MultiEdit` the touched file is formatted with the
formatter the project already has: `rustfmt`, `gofmt`, `ruff format`, `biome
format`, or `prettierd`/`prettier` (run from the project root, so
`.prettierignore` applies).

- **Only formatters by default.** Fixers (`eslint --fix`, `ruff check --fix`,
  `biome check --write`) can change behaviour; turn them on with
  `format.fix = true`.
- **Only clean files.** If formatting would change lines outside your edit (the
  file was not formatted before), the edit is kept as written and hk says so,
  so the pull request diff stays yours. `format.only_clean = false` formats the
  whole file and reports how many lines outside the edit changed.
- **Never touched:** lockfiles, `migrations/`, `fixtures/`, `__snapshots__/`,
  `vendor/`, generated and minified files, plus `format.exclude`.
- **Once at the end.** `format.when = "stop"` formats every touched file when
  Claude stops, instead of after each edit, so a file never changes between two
  edits.

## Configuration

`hk` works without configuration. Settings come from two optional files, the
project's winning key by key:

- user: `~/.config/hk/hk.toml` (or `$XDG_CONFIG_HOME/hk/hk.toml`, or `$HK_CONFIG`)
- project: `.claude/hk.toml` in the project or its closest ancestor that has one

Every key is optional:

```toml
[filters]          # turn individual filters off
pytest = true
install = false

[limits]
generic_max_lines = 80   # generic filter: truncate above this many lines
head = 30
tail = 30
max_errors = 20          # diagnostics/failures listed before "+N more"
stack_lines = 5          # context lines kept per failing test

[limits.commands]        # per-program cap; these are also wrapped
openspec = 40

[rewrite]
enabled = true
generic_commands = ["bazel"]   # also wrap these with the generic filter
unapproved = "skip"            # or "rewrite"; see Permissions

[format]
enabled = true
fix = false              # also run fixers
only_clean = true        # never reformat lines outside the edit
when = "edit"            # or "stop"
exclude = ["docs/"]

[mcp]
enabled = true
max_chars = 8000
skip = ["mcp__myserver__*"]

[misses]                 # record large outputs hk did not wrap
enabled = true
min_chars = 2000

[session_start]
enabled = true
```

## Repo map

At session start hk injects: the git branch, how far it is ahead of or behind
its upstream, uncommitted files and the last 3 commits (and, in a linked
worktree, which checkout it belongs to); the stack and its build/test/lint
commands; what CI really runs (`bitbucket-pipelines.yml`, `.gitlab-ci.yml`,
GitHub workflows); and a 2-level tree. Dot directories that the root
`.gitignore` lists (tool state) and SQLite or lock side files are left out,
and `src/`, `tests/` and friends list up to 40 entries. A folder that only
groups repositories gets one line per repository instead: branch, stack, test
and build commands. After a compaction only the short version (git, stack, CI)
is injected.

## Stats

```
$ hk stats
Estimated tokens saved: 218 over 2 run(s) (1248 -> 376 chars, tokens = chars/4)

filter             runs tokens saved
cargo_test            1          145
pytest                1           72
```

`hk stats` breaks the savings down per filter, project and session;
`--project NAME`, `--session ID` and `--json` narrow or reshape it. MCP
results count under `mcp`.

`hk misses` shows the other side: large Bash outputs hk did *not* wrap, grouped
by reason (`no-filter`, `shape`, `unparsable`, `permissions`, `watch`) and
program, so the next filter is chosen from data.

The estimate is characters / 4. Raw output is capped at 30,000 characters, the
length at which Claude Code truncates Bash output itself. The printed log and
status lines count as output.

## Data and logs

Logs, stats and the error log live in the data directory: `~/.local/share/hk`
(or `$XDG_DATA_HOME/hk`) on Linux, `~/Library/Application Support/hk` on macOS,
`%LOCALAPPDATA%\hk` on Windows, or `$HK_DATA_DIR`.

- Directories are created `0700` and files `0600`.
- Known secret shapes are masked before a line is written: `Bearer`/`Basic`
  credentials, `user:password@` in URLs, provider tokens (`ghp_`, `glpat-`,
  `sk-`, `xox?-`, `AKIA`, `AIza`, `ovk_`...), `PASSWORD=`/`api_key:`-style
  values and private keys.
- Logs are deleted after 7 days. Above 1,000 logs the oldest go first, but
  never one from the running session or from the last hour.

```
hk log last [--session ID] [--grep TEXT] [--lines A:B]   # part of the newest log
hk log list                                               # the 20 newest logs
hk log path                                               # the newest log's path
hk log <file name or path> [--grep TEXT] [--lines A:B]
```

## Troubleshooting

Every hook fails open: a problem in hk never blocks a tool call. Problems are
recorded instead, and the next session start says so in one line.

```
hk doctor     # binary and version, config files, data directory permissions,
              # formatters found here, and the latest hook errors
```

When the plugin cache holds a newer hook-n-hooks than the binary running in a
session, the session start says to restart Claude Code.

## Other commands

```
hk run [--filter NAME] -- <cmd> [args...]   # use a filter by hand
hk explain "<command line>"                 # which filter, or why not
hk filters                                  # list filters
hk map [DIR]                                # print the SessionStart repo map
hk capture <log> <tool> <case>              # log -> anonymised test fixture
```

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how it works, the verified hook
  schema, and the design decisions
- [CONTRIBUTING.md](CONTRIBUTING.md): how to add a filter and its fixtures
- [ROADMAP.md](ROADMAP.md) and [CHANGELOG.md](CHANGELOG.md)

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option.
