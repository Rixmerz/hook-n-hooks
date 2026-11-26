# Roadmap

## Out of scope for v1.0

- **MCP output compaction.** Use PostToolUse `updatedToolOutput` (verified to
  exist) to shrink large MCP tool results, keeping the full result in a local
  log.
- **Repeated-read detection.** Notice when Claude reads a file it has already
  read and that has not changed since, and say so instead of sending the whole
  file again.
- **Large-file outline.** When a Read targets a very large file, offer an outline
  (symbols plus line ranges) first.
- **PreCompact summaries.** Keep a compact, structured summary of the session
  (decisions, files touched, failing tests) so less is lost at compaction.

## Done in v1.0

- Script wrappers (`npm run`, `npm test`, `make`, `just`, `task`), resolved
  through `package.json`, Makefiles and justfiles, or recognised by output.
- Pipes: `2>&1`, `| tail -N`, `| head -N`, `| grep`.
- Filters for linters, git, docker, logs and network, large listings, Maven,
  Gradle, .NET, RSpec, PHPUnit, `mix test` and Playwright.

## Next

- A PowerShell launcher (`bin/hk.ps1`) for Windows machines without Git Bash.
- More filters: `cargo nextest`, `swift test`, `bazel`, `terraform plan`,
  `helm`, `kubectl describe`.
- Taskfile resolution (`task <name>` is only recognised by its output today).
- Compound commands: compact the last stage of `a && b` chains.
- A stream mode for very long runs: print the summary as test results arrive.
- `hk stats --json` and a per-project breakdown.
- Mirror permission rules more completely, once Claude Code documents its
  matching semantics.
