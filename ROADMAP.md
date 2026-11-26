# Roadmap

## Out of scope for v1.0

- **Repeated-read detection.** Notice when Claude reads a file it has already
  read and that has not changed since, and say so instead of sending the whole
  file again.
- **Large-file outline.** When a Read targets a very large file, offer an outline
  (symbols plus line ranges) first.
- **PreCompact summaries.** Keep a compact, structured summary of the session
  (decisions, files touched, failing tests) so less is lost at compaction.

## Done in v1.1

- MCP output compaction, command lists, pipes without `2>&1`, `hide exec`.
- `hk misses`, `hk explain`, `hk log`, `hk doctor`, `hk capture`.
- Safe formatting (formatters only, only clean files, exclusions, at Stop).
- Private, masked logs; pruning by age and session.
- Repo map with git state, CI commands and folders of repositories.
- `angular`, `json` and `table` filters; coverage kept; re-run deltas.

## Done in v1.0

- Script wrappers (`npm run`, `npm test`, `make`, `just`, `task`), resolved
  through `package.json`, Makefiles and justfiles, or recognised by output.
- Pipes: `2>&1`, `| tail -N`, `| head -N`, `| grep`.
- Filters for linters, git, docker, logs and network, large listings, Maven,
  Gradle, .NET, RSpec, PHPUnit, `mix test` and Playwright.

## Next

- Heredocs (`cmd <<'EOF'`): rewrite the command before the heredoc.
- Formatting only the edited line ranges (prettier `--range-start/--range-end`)
  instead of all-or-nothing.

- A PowerShell launcher (`bin/hk.ps1`) for Windows machines without Git Bash.
- More filters: `cargo nextest`, `swift test`, `bazel`, `terraform plan`,
  `helm`, `kubectl describe`.
- Taskfile resolution (`task <name>` is only recognised by its output today).
- A stream mode for very long runs: print the summary as test results arrive.
- Mirror permission rules more completely, once Claude Code documents its
  matching semantics.
