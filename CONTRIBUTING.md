# Contributing

Thanks for helping save tokens. The most useful contribution is a new filter, or
a better one, together with real fixtures.

## Ground rules

- Everything in the repository is in English: code, comments, docs, commit
  messages, CLI strings and test names.
- Never hide information without a trace. A filter may drop lines only because
  the runner always prints the path to the full log. A filter must keep
  everything needed to act on a failure: which test, where it failed, and why.
- When in doubt, return `None`. The runner then falls back to the generic
  filter.
- Keep it fast and dependency-light. Do not spawn processes from filters, and do
  not add async runtimes.
- No telemetry, ever.

## Adding a filter

1. **Capture real output.** Run the tool once passing and once failing, then
   save both outputs with stderr merged and colours off:

   ```sh
   NO_COLOR=1 mytool test > crates/hk/tests/fixtures/mytool/pass.txt 2>&1
   NO_COLOR=1 mytool test > crates/hk/tests/fixtures/mytool/fail.txt 2>&1
   ```

   Replace personal paths (`/home/you/...`) with `/work`, and remove tokens and
   hostnames. Do not hand-edit anything else. Fixtures are stored byte-exact
   (see `.gitattributes`).

2. **Write the module** `crates/hk/src/filters/mytool.rs`:

   ```rust
   use super::{Cmd, Filter};
   use crate::config::Limits;

   pub struct MyTool;

   impl Filter for MyTool {
       fn name(&self) -> &'static str { "mytool" }

       fn detect(&self, cmd: &Cmd) -> bool {
           cmd.program == "mytool" && cmd.subcommand() == Some("test")
       }

       fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String> {
           // `output` is already ANSI-stripped with CRLF normalised.
           // Return None if you do not recognise it.
           todo!()
       }
   }
   ```

   Conventions: on success, print one line (`✓ N passed`). On failure, print a
   `✗` summary line, then one `FAIL <name>` block per failure, with at most
   `limits.stack_lines` indented lines. Cap lists at `limits.max_errors` with
   `text::push_capped` (`+N more`).

3. **Register it.** Add `pub mod mytool;` and `&mytool::MyTool` to `registry()`
   in `crates/hk/src/filters/mod.rs`. Add a line to the
   `registry_routes_commands` test.

   If the tool's output has a recognisable signature (a summary line, a
   diagnostic format), implement `sniff` too. The `script` filter uses it to
   handle `npm run x`, `make x` and friends when the script cannot be resolved.
   If the tool has machine-readable or never-ending modes (`--format json`,
   `--watch`, `-f`), make `detect` return `false` for them.

4. **Add the fixture cases** to `CASES` in `crates/hk/tests/fixtures.rs`. Each
   case is the fixture directory, the case name, the command line that
   produced it, and its exit code. Real captures are strongly preferred;
   remove absolute paths, user names and e-mail addresses from them. If the
   tool is not available and you have to write a fixture by hand in its exact
   format, list it in `CRAFTED` in the same file.

5. **Review the snapshots:**

   ```sh
   cargo test               # new snapshots are written as .snap.new
   cargo insta review       # or: INSTA_UPDATE=always cargo test
   ```

   Read each snapshot. Would you be able to fix the failure from the compact
   output alone? If not, keep more lines.

6. Update the filter table in `README.md` and add a line to `CHANGELOG.md`
   under `Unreleased`.

## Before opening a PR

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs these on Linux, macOS and Windows.

## Releasing (maintainers)

1. Bump the version in `Cargo.toml`, `.claude-plugin/plugin.json` and `bin/hk`
   (`VERSION=`), then move `Unreleased` in `CHANGELOG.md` under the new version.
2. Merge, then tag `vX.Y.Z` and push the tag. The release workflow checks that
   the versions match, builds all six targets, and publishes the assets with
   their `.sha256` files and `SHA256SUMS`.
