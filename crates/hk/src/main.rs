use hk::{hook, run, stats};
use std::io::Write;

const USAGE: &str = "\
hk - token-saving hooks for Claude Code

Usage:
  hk run [--filter NAME] [--session ID] [--piped] -- <command> [args...]
      Run a command, save the full log locally, print a compact summary,
      and exit with the command's exit code. --piped also prints the log
      path last, for output that goes through `| tail` or `| head`.
  hk hook <pre-tool-use|post-tool-use|session-start|stop>
      Hook entry points (read the hook JSON on stdin).
  hk stats [--session ID] [--project NAME] [--json]
      Estimated tokens saved, per filter, project and session (local only).
  hk misses [--session ID] [--project NAME]
      Large Bash outputs hk did not wrap, grouped by reason.
  hk explain \"<command line>\"
      Which filter would wrap the command, or why it would not.
  hk log [last|list|path] [--session ID] [--grep TEXT] [--lines A:B]
      Show part of the newest full log (or list recent logs).
  hk doctor
      Check the binary, config, data directory, formatters and hook errors.
  hk capture <log> <tool> <case>
      Turn a log into an anonymised fixture under tests/fixtures/.
  hk filters
      List the available filters.
  hk map [DIR]
      Print the SessionStart repo map for DIR (default: current directory).
  hk version
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("run") => run_cmd(&args[1..]),
        Some("hook") => hook_cmd(args.get(1).map(String::as_str)),
        Some("stats") => {
            let session = flag(&args[1..], "--session");
            let project = flag(&args[1..], "--project");
            let scope = stats::Scope {
                session: session.as_deref(),
                project: project.as_deref(),
            };
            let recs = stats::load(&hk::paths::stats_file());
            if args.iter().any(|a| a == "--json") {
                println!("{}", stats::report_json(&recs, &scope));
            } else {
                println!("{}", stats::report_scoped(&recs, &scope));
            }
            0
        }
        Some("misses") => {
            let session = flag(&args[1..], "--session");
            let project = flag(&args[1..], "--project");
            let scope = stats::Scope {
                session: session.as_deref(),
                project: project.as_deref(),
            };
            println!(
                "{}",
                stats::misses(&stats::load(&hk::paths::stats_file()), &scope)
            );
            0
        }
        Some("explain") => {
            let Some(cmd) = args.get(1) else {
                eprintln!("usage: hk explain \"<command line>\"");
                return_code(2)
            };
            let cwd = std::env::current_dir().unwrap_or_default();
            let cfg = hk::config::Config::load(&cwd);
            println!("{}", hook::explain(cmd, &cwd, &cfg));
            0
        }
        Some("log") => hk::logcmd::run(&args[1..]),
        Some("doctor") => {
            let cwd = std::env::current_dir().unwrap_or_default();
            println!("{}", hk::doctor::report(&cwd));
            0
        }
        Some("capture") => hk::logcmd::capture(&args[1..]),
        Some("filters") => {
            for f in hk::filters::registry() {
                println!("{}", f.name());
            }
            println!("{}", hk::filters::generic::NAME);
            0
        }
        Some("map") => {
            let dir = args
                .get(1)
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default();
            print!("{}", hk::repomap::render(&dir));
            0
        }
        Some("version" | "--version" | "-V") => {
            println!("hk {}", env!("CARGO_PKG_VERSION"));
            0
        }
        // Test helper: print text on stdout and stderr, then exit with a code.
        Some("__emit") => {
            let code: i32 = args.get(1).and_then(|c| c.parse().ok()).unwrap_or(0);
            let text = args.get(2).map(String::as_str).unwrap_or("");
            println!("{text}");
            eprintln!("{text} (stderr)");
            code
        }
        Some("help" | "--help" | "-h") | None => {
            print!("{USAGE}");
            0
        }
        Some(other) => {
            eprintln!("hk: unknown command `{other}`\n\n{USAGE}");
            2
        }
    };
    // `process::exit` keeps the full code (Windows codes can exceed 255).
    let _ = std::io::stdout().flush();
    std::process::exit(code)
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn run_cmd(args: &[String]) -> i32 {
    let mut filter = None;
    let mut session = String::from("manual");
    let mut piped = false;
    let mut stdout_only = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--" => {
                i += 1;
                break;
            }
            "--filter" => {
                filter = args.get(i + 1).cloned();
                i += 2;
            }
            "--session" => {
                session = args.get(i + 1).cloned().unwrap_or(session);
                i += 2;
            }
            "--piped" => {
                piped = true;
                i += 1;
            }
            "--stdout-only" => {
                stdout_only = true;
                i += 1;
            }
            _ => break,
        }
    }
    run::run(run::RunArgs {
        filter,
        session,
        piped,
        stdout_only,
        argv: args[i.min(args.len())..].to_vec(),
    })
}

/// Exit right away (used where `return` cannot reach `main`'s value).
fn return_code(code: i32) -> ! {
    let _ = std::io::stdout().flush();
    std::process::exit(code)
}

fn hook_cmd(event: Option<&str>) -> i32 {
    // Fail open: any problem means "print nothing, exit 0" -- but record it,
    // so `hk doctor` and the next SessionStart can say so.
    hk::doctor::install_panic_hook("hook panic");
    let Some(input) = hook::read_input() else {
        hk::doctor::record_error(
            event.unwrap_or("hook"),
            "could not read the hook JSON on stdin",
        );
        return 0;
    };
    let mut stdout = std::io::stdout();
    match event {
        Some("pre-tool-use") => {
            let exe = std::env::current_exe()
                .ok()
                .and_then(|p| p.to_str().map(String::from));
            let cwd = input
                .get("cwd")
                .and_then(|c| c.as_str())
                .map(std::path::PathBuf::from)
                .unwrap_or_default();
            let rules = hk::permissions::load(&hk::permissions::settings_files(&cwd));
            if let Some(out) = exe.and_then(|e| hook::pre_tool_use(&input, &e, &rules)) {
                let _ = writeln!(stdout, "{out}");
            }
        }
        Some("post-tool-use") => {
            if let Some(out) = hook::post_tool_use(&input) {
                let _ = writeln!(stdout, "{out}");
            }
        }
        Some("session-start") => {
            if let Some(text) = hook::session_start(&input) {
                let _ = write!(stdout, "{text}");
            }
        }
        Some("stop") => {
            if let Some(out) = hook::stop(&input) {
                let _ = writeln!(stdout, "{out}");
            }
        }
        other => hk::doctor::record_error("hook", &format!("unknown hook event {other:?}")),
    }
    0
}
