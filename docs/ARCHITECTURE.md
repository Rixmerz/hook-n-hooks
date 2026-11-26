# Architecture

`hook-n-hooks` is a Claude Code plugin made of two parts. One is a few JSON
manifests. The other is a single Rust binary, `hk`, that every hook calls. The
binary parses JSON in-process and never spawns `jq`, `grep` or a shell to make
its decisions. A hook invocation takes about 1 ms plus process start-up.

```
.claude-plugin/plugin.json       plugin manifest
.claude-plugin/marketplace.json  this repo is also its own marketplace
hooks/hooks.json                 SessionStart, PreToolUse(Bash), PostToolUse(Edit|Write|MultiEdit)
bin/hk                           POSIX sh launcher: find or download the binary, then exec it
crates/hk/                       the binary
  src/filters/                   one module per tool family, plus the registry
  src/run.rs                     `hk run`: execute, tee the log, compact, keep the exit code
  src/hook.rs                    hook handlers (all fail open)
  src/permissions.rs             mirrors the user's Bash rules for the original command
  src/shell.rs                   conservative parsing of simple Bash commands and pipelines
  src/scripts.rs                 resolve npm/pnpm/yarn/bun scripts, Makefile and justfile targets
  src/format.rs                  per-file format and autofix
  src/repomap.rs, src/stack.rs   SessionStart map, stack detection
  src/stats.rs, src/paths.rs     local stats and data directory
  tests/fixtures/<tool>/         captured outputs (the few hand-written ones are listed in CRAFTED)
  tests/snapshots/               insta snapshots of the compacted outputs
```

## What the docs say (verified)

The design rests on the official documentation, read on 2026-11-23 at
<https://code.claude.com/docs/en/hooks> (docs.claude.com redirects there),
<https://code.claude.com/docs/en/plugins-reference> and
<https://code.claude.com/docs/en/plugin-marketplaces>.

Not everything was read. The last ~14k characters of the hooks page were not,
and neither was the separate marketplace-reference page (the full `owner` and
`source` schemas). Anything below that relies on those parts is marked
*unverified*.

### PreToolUse

- Output: `hookSpecificOutput` with `hookEventName: "PreToolUse"`,
  `permissionDecision` (`allow|deny|ask|defer`), `permissionDecisionReason`,
  `updatedInput` and `additionalContext`.
- **`updatedInput` exists, and it replaces the whole input object.** For Bash we
  therefore copy every field (`command`, `description`, `timeout`,
  `run_in_background`) and change only `command`.
- `permissionDecision` is not required alongside `updatedInput`.
- **Permission rules are evaluated against the input the hook returns.** We
  checked this end to end with `claude -p --plugin-dir`. With
  `--allowedTools 'Bash(cargo test:*)'`, the rewritten command was refused with
  "This command requires approval". The same run with a project
  `allow: ["Bash(cargo test:*)"]` succeeded, because `hk` restated the allow.
  Auto mode accepted the rewritten command. See [Permissions](#permissions).
- Exit code 2 blocks the tool call. `hk` never exits 2.

### PostToolUse

- Top level: `decision: "block"` and `reason`.
  `hookSpecificOutput.additionalContext` is also available.
- `updatedToolOutput` exists (and is preferred over `updatedMCPToolOutput`). It
  can replace what Claude sees from a built-in tool, after the tool has run.
- **A failed tool call fires `PostToolUseFailure`, which cannot rewrite
  results.** A Bash command that exits non-zero is the case where compaction
  saves the most, so rewriting output in PostToolUse would miss exactly that
  case. That is why Bash compaction rewrites the command in PreToolUse
  (`hk run -- <cmd>`) instead.
- For Edit/Write we only add `additionalContext`.

### SessionStart

- Claude Code adds a SessionStart hook's plain-text stdout to Claude's context.
  `hookSpecificOutput.additionalContext` also works. `hk` prints plain text.
- Each field is capped at 10,000 characters. The map is capped at 4,000.

### Input

- Common fields: `session_id`, `transcript_path`, `cwd`, `permission_mode`,
  `hook_event_name`. Tool events add `tool_name`, `tool_input` and
  `tool_use_id`. PostToolUse adds `tool_response`.
- Command hooks must print only the JSON object on stdout.

### Plugins

- `.claude-plugin/plugin.json` requires only `name`. `hooks/hooks.json` needs
  the top-level `"hooks"` wrapper.
- `${CLAUDE_PLUGIN_ROOT}` is the installed plugin directory, which changes on
  update. `${CLAUDE_PLUGIN_DATA}` is `~/.claude/plugins/data/<id>/`, which
  survives updates and is removed on uninstall. Both are also set as environment
  variables for hook processes.
- On Windows, shell-form hook commands run through Git Bash, or through
  PowerShell when Git Bash is missing. Without Git Bash the Bash tool is not
  registered.
- Marketplace: `.claude-plugin/marketplace.json` with `name`, `owner` and
  `plugins[]` (`name`, `source`). A relative `source` is resolved from the
  marketplace root. `claude plugin validate .` passes on this repository.

## Bash compaction

1. **PreToolUse** gets `{"tool_name":"Bash","tool_input":{"command":"cargo test"}}`.
2. `shell::parse_pipeline` accepts only *simple* commands: an optional
   `cd DIR &&` prefix, optional `NAME=value` assignments, then plain words,
   optionally followed by `2>&1`, optionally piped (with stderr merged) into
   one `tail -N`, `head -N` or `grep` stage. Other pipes, redirections, `;`,
   `&&` chains, `$(...)`, backticks, subshells, background `&` and newlines are
   all left alone. So are `run_in_background` and watch modes (`--watch`, bare
   `vitest`, `dev`, `serve`).
3. `hook::choose_filter` picks the filter. Script wrappers (`npm run`, `npm
   test`, `make`, `just`, `task`) go through `scripts.rs` first: the script
   body is read from `package.json`, a one-line Makefile rule or a one-line
   justfile recipe, in the directory the command runs in, and the filter of
   the command it runs is used. An unresolved wrapper with a terminating name
   (`test`, `build`, `lint`...) gets the `script` filter, which asks every
   filter's `sniff` whether it recognises the output. Server and watch scripts
   are skipped. Otherwise the registry picks the filter that owns the command.
4. `permissions.rs` checks the original command against the user's rules (see
   below).
5. The command becomes
   `cd DIR && NAME=v '<abs path to hk>' run --filter F --session S -- <original words>`.
   The `cd` and the assignments stay in the parent shell, so the Bash tool's
   working directory still carries over to later commands. The original words
   are passed verbatim, so Bash still does the real quoting and expansion. `hk`
   gets an argv and runs it directly, with no shell, so the same code runs on
   every OS. On Windows, `.cmd` shims (npm, pnpm, npx) found on `PATH` run
   through `cmd /C`.
6. **`hk run`** prints `[hk] log: <path>` first and flushes it. Then it merges
   the child's stdout and stderr into one pipe (`std::io::pipe`), which keeps
   their interleaving. Every chunk is teed to the log as it arrives, so a
   timeout kill still leaves the path in Claude's output and the log on disk.
   Stdin is inherited.
7. When the child exits, the cleaned output goes to the filter.
   `text::clean` strips ANSI, normalises CRLF, and collapses `\r` progress
   redraws. If the filter returns `None`, or a result that is not shorter than
   the input, the generic filter is used. `hk` then prints the result, appends
   a stats record, and exits with **the child's exact exit code**: on Unix,
   `128 + signal` for a signal; on Windows, the full 32-bit code, through
   `process::exit`.

### Pipelines

The pipe is never moved into `hk`; it stays in the shell, so the exit status
the Bash tool reports is the one the original pipeline would have had (the last
stage's, or the first failing one under `pipefail`).

- `cmd 2>&1` becomes `hk run -- cmd 2>&1`. `hk` merges stderr itself; the
  `2>&1` now applies to `hk`, which is harmless.
- `cmd 2>&1 | tail -N` (or `head -N`) becomes
  `hk run --piped -- cmd 2>&1 | tail -N`. `tail` slices the compact output,
  which is usually already shorter than N lines. `--piped` prints the log path
  last as well as first, so either end of the slice keeps it. One difference:
  with `head`, the original command could be killed early by `SIGPIPE`; under
  `hk` it runs to completion. Only commands that some filter owns (tests,
  builds, linters, listings) are rewritten, and they terminate anyway.
- `cmd 2>&1 | grep ...` becomes `cmd 2>&1 | hk run --filter generic -- grep ...`.
  Wrapping the left side would make grep search the compact text and change
  its result, so the grep stage is wrapped instead: grep reads the raw output on
  its inherited stdin, and `hk` caps what grep prints.
- Without `2>&1`, a pipe is left alone: `hk run` always merges stderr, so the
  next stage would start seeing stderr.

For permissions, every stage is checked on its own (see below).

### Golden rule

Any output Claude sees from a wrapped command starts with the path to the full
raw log. Logs live in the data directory (`logs/`), and the latest 200 are kept.

## Filter registry

```rust
pub trait Filter: Sync {
    fn name(&self) -> &'static str;
    fn detect(&self, cmd: &Cmd) -> bool;
    fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String>;
    /// Output signature, for the `script` filter. Defaults to `false`.
    fn sniff(&self, output: &str) -> bool;
}
```

`Cmd` is the effective program and its arguments. Wrappers such as `npx`,
`bunx`, `pnpm exec`, `uv run`, `poetry run` and `python -m` are already removed,
and paths and `.exe` are stripped from the program name. Filters are listed in
`filters::registry()`, in priority order. To add a tool, see
[CONTRIBUTING.md](../CONTRIBUTING.md).

## Permissions

The rewrite changes the command string that permission rules are matched
against. Left as it is, that would cause two problems. It would cause extra
approval prompts for commands the user already allowed. Worse, it would bypass
`deny` and `ask` rules, such as `Bash(npm install:*)`. So before rewriting,
`permissions.rs` reads the `permissions.{allow,deny,ask}` arrays from
`~/.claude/settings.json`, `$CLAUDE_PROJECT_DIR/.claude/settings.json` and
`.claude/settings.local.json`, and checks the **original** command:

| Original matches | Action |
| --- | --- |
| `deny` or `ask` (matched loosely) | no rewrite |
| `allow` (matched strictly) | rewrite, and return `permissionDecision: "allow"` with the rule in the reason |
| nothing, in `auto` or `bypassPermissions` mode | rewrite with no decision; Claude Code's checks still apply |
| nothing, in another mode | no rewrite (`rewrite.unapproved = "rewrite"` changes this) |

With a `cd`/env prefix, only a catch-all `Bash` rule is restated as allow.
For a pipeline, each stage (`cargo test`, `tail -20`) is checked on its own: a
`deny` or `ask` rule on any stage, or on the whole command line, means no
rewrite, and allow is restated only when every stage matches an allow rule (or
a catch-all does). Script wrappers are checked as written (`npm run lint`), never
as the command the script resolves to.
Rules from `--allowedTools` and managed settings are not visible to hooks, so
they are not mirrored. A command allowed only through `--allowedTools` is
therefore not rewritten in default mode.

## PostToolUse formatting

Only the touched file is passed to any tool, and a tool runs only if it is
found: first in `node_modules/.bin` (walking up from the file), then on `PATH`.
`which` is implemented in-process.

| Extension | Tools |
| --- | --- |
| `.rs` | `rustfmt --emit stdout` fed through **stdin**, so it cannot follow `mod foo;` into other files. The edition comes from the nearest `Cargo.toml`; the file is written only if it changed |
| `.go` | `gofmt -w` |
| `.py`, `.pyi` | `ruff check --fix`, then `ruff format` |
| js/ts/json/css/vue/svelte | `biome check --write` if present; otherwise `prettier --write` and `eslint --fix --format unix` |

If the file changed, the context says so ("re-read before editing again"),
because Claude's copy is now stale. Unfixed issues are listed, capped at
`max_errors`. If there is nothing to report, the hook prints nothing.

## SessionStart map

`stack.rs` detects `Cargo.toml`, `package.json` (package manager from the
lockfile; scripts `build`/`test`/`lint`; `tsconfig.json`), `deno.json[c]`
(tasks), `pyproject.toml` (uv/poetry runner; ruff) and `go.mod`. `repomap.rs`
prints the stacks, the resolved commands, and a 2-level tree, with directories
first, at most 15 entries per directory, and vendored or build directories
skipped.

## Data, stats, privacy

Logs and `stats.jsonl` live in a fixed per-OS directory (`paths.rs`), because
`hk run` runs inside the Bash tool, which does not see `CLAUDE_PLUGIN_DATA`, and
`hk stats` runs from your terminal. `HK_DATA_DIR` overrides it. Tokens are
estimated as `chars / 4`. Raw output is capped at 30,000 characters (Claude
Code's default Bash output limit) before computing savings, so the stats do not
overclaim. Nothing is sent anywhere.

## Distribution

`bin/hk` is a POSIX `sh` launcher:

1. `$HK_BIN` if it is set;
2. the cached `${CLAUDE_PLUGIN_DATA}/bin/<version>/hk[.exe]`;
3. an `hk` on `PATH` whose `hk version` matches;
4. a download of `hk-<target>[.exe]` and `.sha256` from the `v<version>` GitHub
   Release (curl, wget or PowerShell), verified with sha256sum, shasum or
   certutil, then an atomic move into the cache.

If every step fails, `hk hook …` exits 0 with no output (fail open). The reason
goes to stderr, along with the `cargo install` fallback.

### Windows

Claude Code runs shell-form hook commands through Git Bash on Windows, so the
`sh` launcher works there. Without Git Bash, Claude Code has no Bash tool, so
Bash compaction does not apply anyway. The PowerShell fallback cannot run an
extension-less `sh` script. Users without Git Bash need `hk.exe` on `PATH`, from
`cargo install`, and then the formatting and SessionStart hooks have to point to
it. A PowerShell launcher is on the roadmap. Release assets use the MSVC targets
for x86_64 and aarch64.

## Known limits

- Only simple commands are rewritten. `cargo test 2>&1 | tail` is passed
  through untouched.
- `hk run` buffers the output for the filter; the log is written as the output
  arrives. Watch modes are never wrapped.
- Filters are heuristic. Anything a filter does not recognise falls back to
  generic head/tail, which is never worse than the raw output plus one log line.

## v1.1 additions

- **Lists** (`shell::split_chain`): the command line is split on top-level
  `&&`, `||`, `;` and newlines; each part goes through `parse_pipeline` and is
  wrapped on its own (`hook::plan_rewrite`). Separators and untouched parts are
  kept byte for byte. Every part and pipe stage is checked against the
  permission rules; a narrow allow rule is only restated for a single part
  with no `cd` or env prefix.
- **`--stdout-only`**: for `cmd | tail` without `2>&1`, the child's stderr is
  inherited instead of merged, so the pipe stage sees what it saw before.
- **PostToolUse dispatch** (`hook::post_tool_use`): Edit/Write/MultiEdit go to
  `format`, `mcp__*` to `mcp` (`updatedToolOutput`, shape-preserving: only
  long strings change), Bash to the miss recorder.
- **Failing open, visibly** (`doctor`): hook errors and panics are appended to
  `errors.jsonl` (capped at 200); `hk doctor` and the next SessionStart report
  them.
- **Logs** (`run`, `redact`): written `0600` in a `0700` directory, line by
  line through `redact::line`. Names are `<nanos>-<pid>-<session>-<filter>.log`
  so pruning can spare the running session.
- **Formatting** (`format`): the change a formatter made is compared with the
  edited lines (common prefix/suffix hull against the lines the new strings
  occupy); with `only_clean`, a change outside them is reverted.
