//! Maven (`mvn`, `mvnw`) and Gradle (`gradle`, `gradlew`).
//!
//! Keeps the build result, the test counts, failing tests with their
//! assertion and first project frame, compiler errors as `file:line: msg`,
//! and the "What went wrong" block. Scripted Maven calls
//! (`help:evaluate -DforceStdout`) are never wrapped.

use super::build::dedup_count;
use super::{Cmd, Filter};
use crate::config::Limits;
use crate::text::{fail_block, push_capped};

pub struct Jvm;

impl Filter for Jvm {
    fn name(&self) -> &'static str {
        "jvm"
    }

    fn detect(&self, cmd: &Cmd) -> bool {
        match cmd.program.as_str() {
            "mvn" | "mvnw" => !cmd.args.iter().any(|a| {
                a.starts_with("help:") || a == "-DforceStdout" || a == "-q" || maven_server(a)
            }),
            "gradle" | "gradlew" => !cmd.args.iter().any(|a| {
                a == "-t" || a == "--continuous" || a == "-q" || a == "--quiet" || gradle_server(a)
            }),
            _ => false,
        }
    }

    fn sniff(&self, output: &str) -> bool {
        output.lines().any(|l| {
            let t = l
                .trim_start_matches("[INFO] ")
                .trim_start_matches("[ERROR] ");
            t.starts_with("BUILD SUCCESS") || t.starts_with("BUILD FAIL")
        })
    }

    fn filter(&self, output: &str, exit: i32, limits: &Limits) -> Option<String> {
        if output.contains("[INFO]") || output.contains("[ERROR]") {
            maven(output, exit, limits)
        } else {
            gradle(output, exit, limits)
        }
    }
}

/// Goals that start a server or a program and never exit on their own:
/// `spring-boot:run`, `quarkus:dev`, `jetty:run`, `exec:java`.
fn maven_server(goal: &str) -> bool {
    match goal.split_once(':') {
        Some((plugin, g)) => {
            plugin == "exec" || g == "run" || g.ends_with("-run") || g == "dev" || g == "start"
        }
        None => false,
    }
}

/// `run`, `bootRun`, `quarkusDev`, `appRun`, `:app:bootRun`.
fn gradle_server(task: &str) -> bool {
    if task.starts_with('-') {
        return false;
    }
    let t = task.rsplit(':').next().unwrap_or(task).to_ascii_lowercase();
    t.ends_with("run") || t.ends_with("dev") || t == "start"
}

fn strip_level(l: &str) -> (&str, &str) {
    for lvl in ["[INFO]", "[ERROR]", "[WARNING]", "[WARN]", "[DEBUG]"] {
        if let Some(r) = l.strip_prefix(lvl) {
            return (lvl, r.strip_prefix(' ').unwrap_or(r));
        }
    }
    ("", l)
}

/// `/a/B.java:[5,21] msg` -> `/a/B.java:5: msg`.
fn javac_error(t: &str) -> Option<String> {
    let idx = t.find(":[")?;
    let close = t[idx..].find(']')? + idx;
    let line = t[idx + 2..close].split(',').next()?;
    if !line.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("{}:{line}: {}", &t[..idx], t[close + 1..].trim()))
}

fn maven(output: &str, exit: i32, limits: &Limits) -> Option<String> {
    let mut result = None;
    let mut tests = None;
    let mut compile = Vec::new();
    let mut fails = Vec::new();
    let mut what = None;
    let mut warnings = 0;
    let mut in_results = false;
    for l in output.lines() {
        let (lvl, t) = strip_level(l);
        if lvl == "[WARNING]" {
            warnings += 1;
        }
        if t.starts_with("BUILD SUCCESS") || t.starts_with("BUILD FAILURE") {
            result = Some(t.trim().to_string());
        }
        if t.starts_with("Tests run:") && !t.contains("Time elapsed") {
            tests = Some(t.trim().to_string());
        }
        if t.trim() == "Results:" {
            in_results = true;
            continue;
        }
        if in_results {
            if t.starts_with("Tests run:") {
                in_results = false;
            } else if t.starts_with("  ") && !t.trim().is_empty() {
                fails.push(t.trim().to_string());
            }
        }
        if lvl == "[ERROR]" {
            if let Some(e) = javac_error(t) {
                compile.push(e);
            }
            if t.starts_with("Failed to execute goal") && what.is_none() {
                what = Some(t.trim().to_string());
            }
        }
    }
    let result = result?;
    let ok = exit == 0 && result.starts_with("BUILD SUCCESS");
    let mut head = format!("{} mvn: {result}", if ok { "✓" } else { "✗" });
    if let Some(t) = &tests {
        head.push_str(&format!(" | {t}"));
    }
    if warnings > 0 {
        head.push_str(&format!(" | {warnings} warning line(s)"));
    }
    let mut out = vec![head];
    // Maven repeats compiler errors in its final "Failed to execute goal" block.
    let mut items = crate::text::dedup(compile);
    items.extend(fails.into_iter().map(|f| format!("FAIL {f}")));
    push_capped(&mut out, items, limits.max_errors);
    if !ok {
        out.extend(what);
    }
    Some(out.join("\n"))
}

/// Stack frames of the JDK, JUnit, Gradle and Kotlin runtime: noise next to
/// the project's own frames.
fn is_framework_frame(t: &str) -> bool {
    let Some(f) = t.strip_prefix("at ") else {
        return false;
    };
    let f = f.trim_start_matches("app//");
    [
        "org.junit.",
        "org.opentest4j.",
        "java.",
        "jdk.internal.",
        "kotlin.",
        "org.gradle.",
        "sun.",
    ]
    .iter()
    .any(|p| f.starts_with(p))
}

fn gradle(output: &str, exit: i32, limits: &Limits) -> Option<String> {
    let lines: Vec<&str> = output.lines().collect();
    let result = lines
        .iter()
        .rev()
        .map(|l| l.trim())
        .find(|l| l.starts_with("BUILD SUCCESSFUL") || l.starts_with("BUILD FAILED"))?;
    let tests = lines
        .iter()
        .map(|l| l.trim())
        .find(|l| l.contains(" tests completed"));
    let mut items: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        // Kotlin `e: file:///a/B.kt:10:5 msg`, javac `/a/B.java:10: error: msg`.
        if let Some(r) = t.strip_prefix("e: ") {
            items.push(r.trim_start_matches("file://").to_string());
        } else if (t.contains(".java:") && t.contains(": error: "))
            || (t.starts_with("> Task ") && t.ends_with(" FAILED"))
        {
            items.push(t.to_string());
        } else if t.ends_with(" FAILED") && t.contains(" > ") {
            // `CalcTest > addsWrong() FAILED` then indented cause lines.
            let body: Vec<String> = lines[i + 1..]
                .iter()
                .take_while(|l| l.starts_with(' ') && !l.trim().is_empty())
                .map(|l| l.trim())
                .filter(|l| !is_framework_frame(l))
                .map(String::from)
                .take(limits.stack_lines)
                .collect();
            items.push(fail_block(t.trim_end_matches(" FAILED"), &body));
        }
        i += 1;
    }
    // `* What went wrong:` up to `* Try:`.
    let what: Vec<String> = lines
        .iter()
        .skip_while(|l| !l.starts_with("* What went wrong:"))
        .skip(1)
        .take_while(|l| !l.starts_with("* Try:"))
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .take(limits.stack_lines * 2)
        .map(String::from)
        .collect();
    let ok = exit == 0 && result.starts_with("BUILD SUCCESSFUL");
    let mut head = format!("{} gradle: {result}", if ok { "✓" } else { "✗" });
    if let Some(t) = tests {
        head.push_str(&format!(" | {t}"));
    }
    let mut out = vec![head];
    push_capped(&mut out, dedup_count(items), limits.max_errors);
    if !ok && !what.is_empty() {
        out.push("What went wrong:".to_string());
        out.extend(what.into_iter().map(|w| format!("  {w}")));
    }
    Some(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(s: &str) -> bool {
        let v: Vec<String> = s.split_whitespace().map(String::from).collect();
        Jvm.detect(&Cmd::from_argv(&v).unwrap())
    }

    #[test]
    fn never_wraps_servers() {
        assert!(detect("mvn -B test"));
        assert!(detect("mvn clean install"));
        assert!(detect("./gradlew test"));
        assert!(detect("gradle :app:build"));
        for c in [
            "mvn spring-boot:run",
            "mvn quarkus:dev",
            "mvn jetty:run",
            "mvn exec:java",
            "./mvnw help:evaluate -Dexpression=project.version -q -DforceStdout",
            "./gradlew bootRun",
            "./gradlew :app:bootRun",
            "gradle quarkusDev",
            "gradle appRun",
            "gradle run",
            "gradle test --continuous",
        ] {
            assert!(!detect(c), "{c}");
        }
    }
}
