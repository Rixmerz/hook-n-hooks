//! Angular CLI: `ng build` and `ng test` (Karma, single run). Keeps TS/NG
//! errors with their location, every warning (budgets first, since they are
//! what CI and Sonar complain about later) and the `Initial total` line.

use super::{Cmd, Filter};
use crate::config::Limits;
use crate::filters::build::dedup_count;
use crate::text::push_capped;

pub struct Angular;

impl Filter for Angular {
    fn name(&self) -> &'static str {
        "angular"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        if cmd.program != "ng" {
            return false;
        }
        match cmd.subcommand() {
            Some("build" | "b") => !cmd.has_arg("--watch") && !cmd.has_arg("--watch=true"),
            // `ng test` watches by default: only a single run is wrapped.
            Some("test" | "t") => cmd
                .args
                .iter()
                .any(|a| matches!(a.as_str(), "--watch=false" | "--no-watch" | "--single-run")),
            _ => false,
        }
    }

    fn sniff(&self, output: &str) -> bool {
        output.lines().any(|l| {
            let t = l.trim();
            t.starts_with("Application bundle generation")
                || t.starts_with("Initial chunk files")
                || t.starts_with("Initial total")
                || (t.contains("Executed ") && t.contains(" of "))
        })
    }

    fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String> {
        if let Some(t) = karma(output, exit, limits) {
            return Some(t);
        }
        let d = scan(output);
        let total = output
            .lines()
            .find(|l| l.contains("Initial total") || l.contains("Initial Total"))
            .map(|l| {
                l.split(['|', ' '])
                    .filter(|w| !w.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            });
        let took = output
            .lines()
            .map(str::trim)
            .find_map(|l| l.strip_prefix("Application bundle generation "))
            .map(String::from);
        let recognised = total.is_some() || took.is_some() || !d.errors.is_empty();
        if !recognised {
            return None;
        }
        let (n_err, n_warn) = (d.errors.len(), d.warnings.len());
        let mut head = if exit == 0 && n_err == 0 {
            format!("✓ ng build ok: {n_warn} warning(s)")
        } else {
            format!("✗ ng build failed: {n_err} error(s), {n_warn} warning(s)")
        };
        if let Some(t) = total {
            head.push_str(&format!("; {t}"));
        }
        let mut out = vec![head];
        let mut items = dedup_count(d.errors);
        // Budgets first among warnings.
        let (mut budget, other): (Vec<String>, Vec<String>) =
            d.warnings.into_iter().partition(|w| w.contains("budget"));
        budget.extend(other);
        items.extend(dedup_count(budget));
        push_capped(&mut out, items, limits.max_errors);
        Some(out.join("\n"))
    }
}

#[derive(Default)]
struct Diags {
    errors: Vec<String>,
    warnings: Vec<String>,
}

/// esbuild-style `✘ [ERROR] TS2322: msg` followed by `file:line:col:`, and
/// webpack-style `Error: file:line:col - error TS2322: msg` / `Warning: msg`.
fn scan(output: &str) -> Diags {
    let mut d = Diags::default();
    let lines: Vec<&str> = output.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        let (is_err, msg) = if let Some(m) = t.strip_prefix("✘ [ERROR] ") {
            (true, m)
        } else if let Some(m) = t.strip_prefix("▲ [WARNING] ") {
            (false, m)
        } else if let Some(m) = t.strip_prefix("Error: ") {
            (true, m)
        } else if let Some(m) = t.strip_prefix("Warning: ") {
            (false, m)
        } else {
            continue;
        };
        let msg = msg.trim_end_matches("[plugin angular-compiler]").trim_end();
        // Location: the first `path:line:col:` within the next lines.
        let loc = lines[i + 1..]
            .iter()
            .take(4)
            .map(|x| x.trim())
            .take_while(|x| {
                !x.starts_with("✘ ")
                    && !x.starts_with("▲ ")
                    && !x.starts_with("Error: ")
                    && !x.starts_with("Warning: ")
            })
            .find(|x| x.ends_with(':') && x.matches(':').count() >= 3 && !x.contains(' '))
            .map(|x| x.trim_end_matches(':'));
        let entry = match loc {
            Some(loc) if !msg.contains(loc) => format!("{loc}: {msg}"),
            _ => msg.to_string(),
        };
        if is_err {
            d.errors.push(entry);
        } else {
            d.warnings.push(entry);
        }
    }
    d
}

/// Karma: `Executed 42 of 42 (2 FAILED) ...` plus `... FAILED` blocks.
fn karma(output: &str, exit: i32, limits: &Limits) -> Option<String> {
    let summary = output
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.contains("Executed ") && l.contains(" of "))?;
    let s = &summary[summary.find("Executed ")?..];
    let s = s.split(" (").next().unwrap_or(s).to_string();
    let failed_marker = summary.contains("FAILED") || exit != 0;
    if !failed_marker {
        return Some(format!("✓ {s}"));
    }
    let mut out = vec![format!(
        "✗ {}",
        summary[summary.find("Executed ")?..].trim()
    )];
    let lines: Vec<&str> = output.lines().collect();
    let mut fails = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if t.ends_with(" FAILED") && !t.contains("Executed ") {
            let name = t.trim_end_matches(" FAILED");
            let name = name.split(") ").last().unwrap_or(name);
            let mut block = format!("FAIL {name}");
            for b in lines[i + 1..]
                .iter()
                .map(|x| x.trim())
                .take_while(|x| {
                    !x.is_empty() && !x.ends_with(" FAILED") && !x.contains("Executed ")
                })
                .take(limits.stack_lines + 1)
            {
                block.push_str("\n  ");
                block.push_str(b);
            }
            fails.push(block);
        }
    }
    push_capped(&mut out, fails, limits.max_errors);
    Some(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(s: &str) -> Cmd {
        let v: Vec<String> = s.split_whitespace().map(String::from).collect();
        Cmd::from_argv(&v).unwrap()
    }

    #[test]
    fn detects_single_runs_only() {
        assert!(Angular.detect(&cmd("ng build --configuration production")));
        assert!(Angular.detect(&cmd("npx ng test --watch=false")));
        assert!(!Angular.detect(&cmd("ng test")));
        assert!(!Angular.detect(&cmd("ng serve")));
    }

    #[test]
    fn summarises_esbuild_output() {
        let out = "\
Initial chunk files   | Names         |  Raw size
main-ABC.js           | main          | 500.00 kB |
polyfills-XYZ.js      | polyfills     |  34.00 kB |

                      | Initial total | 620.00 kB

Application bundle generation failed. [8.123 seconds]

▲ [WARNING] bundle initial exceeded maximum budget. Budget 500.00 kB was not met by 120.00 kB with a total of 620.00 kB.

✘ [ERROR] TS2322: Type 'string' is not assignable to type 'number'. [plugin angular-compiler]

    src/app/app.component.ts:10:2:
      10 │   x: number = 'a';
";
        let r = Angular.filter(out, 1, &Limits::default()).unwrap();
        assert!(
            r.starts_with("✗ ng build failed: 1 error(s), 1 warning(s); Initial total 620.00 kB"),
            "{r}"
        );
        assert!(r.contains("src/app/app.component.ts:10:2: TS2322"), "{r}");
        assert!(r.contains("exceeded maximum budget"));
    }

    #[test]
    fn summarises_karma() {
        let ok = "Chrome Headless 120: Executed 42 of 42 SUCCESS (1.2 secs / 1.1 secs)\nTOTAL: 42 SUCCESS";
        assert_eq!(
            Angular.filter(ok, 0, &Limits::default()).unwrap(),
            "✓ Executed 42 of 42 SUCCESS"
        );
        let bad = "Chrome Headless 120 (Mac) AppComponent should render title FAILED\n\tExpected 'a' to be 'b'.\n\tat <Jasmine>\nChrome Headless 120: Executed 2 of 2 (1 FAILED) (0.1 secs)";
        let r = Angular.filter(bad, 1, &Limits::default()).unwrap();
        assert!(r.contains("FAIL AppComponent should render title"), "{r}");
        assert!(r.contains("Expected 'a' to be 'b'."));
    }
}
