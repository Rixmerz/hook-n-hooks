//! Test runners of other ecosystems: `rspec`, `phpunit`, `mix test` and
//! `playwright test`. One filter each; they share the output shape: a count
//! line, then one block per failing test (name, assertion, location).

use super::{Cmd, Filter};
use crate::config::Limits;
use crate::text::{count_before, fail_block, push_capped};

fn summary_line(output: &str, pred: impl Fn(&str) -> bool) -> Option<String> {
    output
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| pred(l))
        .map(String::from)
}

fn finish(ok: bool, counts: String, fails: Vec<String>, limits: &Limits) -> String {
    if ok {
        return format!("✓ {counts}");
    }
    let mut out = vec![format!("✗ {counts}")];
    push_capped(&mut out, fails, limits.max_errors);
    out.join("\n")
}

/// Numbered failure blocks (`  1) name` ... up to the next number or a
/// terminator), keeping up to `budget` lines selected by `keep`.
fn numbered(
    output: &str,
    end: impl Fn(&str) -> bool,
    keep: impl Fn(&str) -> bool,
    budget: usize,
) -> Vec<String> {
    let mut res = Vec::new();
    let mut cur: Option<(String, Vec<String>)> = None;
    for line in output.lines() {
        let t = line.trim();
        if end(t) {
            if let Some((n, b)) = cur.take() {
                res.push(fail_block(&n, &b));
            }
            continue;
        }
        let num = t
            .split_once(") ")
            .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if let Some((_, name)) = num {
            if let Some((n, b)) = cur.take() {
                res.push(fail_block(&n, &b));
            }
            cur = Some((name.trim_end_matches(['─', ' ']).to_string(), Vec::new()));
            continue;
        }
        if let Some((_, body)) = cur.as_mut() {
            // Progress dots (`....`) and section separators are not part of a failure.
            let noise = t.chars().all(|c| c == '.')
                || t == "--"
                || t.starts_with("There was ")
                || t.starts_with("There were ");
            if !t.is_empty() && !noise && keep(t) && body.len() < budget {
                body.push(t.to_string());
            }
        }
    }
    if let Some((n, b)) = cur.take() {
        res.push(fail_block(&n, &b));
    }
    res
}

pub struct Rspec;

impl Filter for Rspec {
    fn name(&self) -> &'static str {
        "rspec"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        cmd.program == "rspec"
            && !cmd
                .args
                .iter()
                .any(|a| a.starts_with("--format=j") || a == "j")
    }

    fn sniff(&self, output: &str) -> bool {
        summary_line(output, is_rspec_summary).is_some()
    }

    fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String> {
        let summary = summary_line(output, is_rspec_summary)?;
        let failed = count_before(&summary, " failure").unwrap_or(0);
        // `rspec ./spec/calc_spec.rb:6 # Calc adds` lines name every failure.
        let reruns: Vec<&str> = output
            .lines()
            .filter(|l| l.starts_with("rspec ./"))
            .collect();
        // Numbered blocks also appear under `Pending:`; start at `Failures:`.
        let from = output.find("\nFailures:").unwrap_or(0);
        let mut fails = numbered(
            &output[from..],
            |t| t == "Failed examples:" || t.starts_with("Finished in "),
            |t| {
                t.starts_with("Failure/Error:")
                    || t.starts_with("expected")
                    || t.starts_with("got:")
                    || t.starts_with("(compared using")
                    || (t.starts_with("# ./") && !t.contains("/gems/"))
            },
            limits.stack_lines,
        );
        if fails.is_empty() {
            fails = reruns.iter().map(|r| format!("FAIL {r}")).collect();
        }
        Some(finish(failed == 0 && exit == 0, summary, fails, limits))
    }
}

fn is_rspec_summary(l: &str) -> bool {
    l.contains(" example") && (l.contains(" failure") || l.contains(" failures"))
}

pub struct Phpunit;

impl Filter for Phpunit {
    fn name(&self) -> &'static str {
        "phpunit"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        (cmd.program == "phpunit" || cmd.program == "pest")
            && !cmd
                .args
                .iter()
                .any(|a| a.starts_with("--log-") || a == "--list-tests")
    }

    fn sniff(&self, output: &str) -> bool {
        summary_line(output, is_phpunit_summary).is_some()
    }

    fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String> {
        let summary = summary_line(output, is_phpunit_summary)?;
        let ok = summary.starts_with("OK") && exit == 0;
        let fails = numbered(
            output,
            |t| t == "FAILURES!" || t == "ERRORS!" || t.starts_with("Tests: "),
            |t| !t.starts_with("--- ") && !t.starts_with("+++ "),
            limits.stack_lines,
        );
        Some(finish(ok, summary, fails, limits))
    }
}

fn is_phpunit_summary(l: &str) -> bool {
    (l.starts_with("OK (") && l.contains(" test"))
        || (l.starts_with("Tests: ") && l.contains("Assertions: "))
}

pub struct MixTest;

impl Filter for MixTest {
    fn name(&self) -> &'static str {
        "mix_test"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        cmd.program == "mix"
            && cmd.subcommand() == Some("test")
            && !cmd.has_arg("--listen-on-stdin")
    }

    fn sniff(&self, output: &str) -> bool {
        summary_line(output, is_mix_summary).is_some()
    }

    fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String> {
        let summary = summary_line(output, is_mix_summary)?;
        let failed = count_before(&summary, " failure").unwrap_or(0);
        let fails = numbered(
            output,
            |t| t.starts_with("Finished in "),
            |t| t != "stacktrace:",
            limits.stack_lines + 2,
        );
        Some(finish(failed == 0 && exit == 0, summary, fails, limits))
    }
}

fn is_mix_summary(l: &str) -> bool {
    (l.contains(" test, ") || l.contains(" tests, ")) && l.contains(" failure")
}

pub struct Playwright;

impl Filter for Playwright {
    fn name(&self) -> &'static str {
        "playwright"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        cmd.program == "playwright"
            && cmd.subcommand() == Some("test")
            && !cmd.args.iter().any(|a| {
                a == "--ui" || a == "--debug" || a.starts_with("--reporter=json") || a == "--list"
            })
    }

    fn sniff(&self, output: &str) -> bool {
        output.contains(" › ") && summary_line(output, is_pw_count).is_some()
    }

    fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String> {
        // Footer: `  1 failed` / list of names / `  1 flaky` / `  5 passed (12.3s)`.
        let mut counts = Vec::new();
        for l in output.lines().map(str::trim) {
            if is_pw_count(l) {
                counts.push(l.to_string());
            }
        }
        if counts.is_empty() {
            return None;
        }
        let failed: u64 = counts
            .iter()
            .filter_map(|c| count_before(c, " failed"))
            .sum();
        let fails = numbered(
            output,
            |t| is_pw_count(t) || t.starts_with("Error Context:") || t.starts_with("attachment #"),
            |t| {
                t.starts_with("Error:")
                    || t.starts_with("Expected")
                    || t.starts_with("Received")
                    || t.starts_with("Locator:")
                    || t.starts_with("Timeout")
                    || (t.starts_with("at ") && !t.contains("node_modules"))
            },
            limits.stack_lines,
        );
        Some(finish(
            failed == 0 && exit == 0,
            counts.join(", "),
            fails,
            limits,
        ))
    }
}

fn is_pw_count(l: &str) -> bool {
    let mut it = l.splitn(2, ' ');
    let n = it.next().unwrap_or("");
    let rest = it.next().unwrap_or("");
    !n.is_empty()
        && n.chars().all(|c| c.is_ascii_digit())
        && [
            "passed",
            "failed",
            "flaky",
            "skipped",
            "did not run",
            "interrupted",
        ]
        .iter()
        .any(|w| rest == *w || rest.starts_with(&format!("{w} (")))
}
