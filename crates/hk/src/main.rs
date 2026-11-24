use hk::{hook, run, stats};
use std::io::Write;

const USAGE: &str = "\
hk - token-saving hooks for Claude Code

Usage:
  hk run [--filter NAME] [--session ID] -- <command> [args...]
      Run a command, save the full log locally, print a compact summary,
      and exit with the command's exit code.
  hk hook <pre-tool-use|post-tool-use|session-start>
      Hook entry points (read the hook JSON on stdin).
  hk stats [--session ID]
      Estimated tokens saved, per filter and per session (local only).
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
            println!(
                "{}",
                stats::report(&stats::load(&hk::paths::stats_file()), session.as_deref())
            );
            0
        }
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
            _ => break,
        }
    }
    run::run(run::RunArgs {
        filter,
        session,
        argv: args[i.min(args.len())..].to_vec(),
    })
}

fn hook_cmd(event: Option<&str>) -> i32 {
    // Fail open: any problem means "print nothing, exit 0".
    let Some(input) = hook::read_input() else {
        return 0;
    };
    let mut stdout = std::io::stdout();
    match event {
        Some("pre-tool-use") => {
            let exe = std::env::current_exe()
                .ok()
                .and_then(|p| p.to_str().map(String::from));
            if let Some(out) = exe.and_then(|e| hook::pre_tool_use(&input, &e)) {
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
        _ => {}
    }
    0
}
