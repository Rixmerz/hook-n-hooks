//! `hk run`: execute a command, tee its raw output to a local log, print a
//! compact summary, and exit with the command's exact exit code.
//!
//! Golden rule: whenever the printed output differs from the raw output, the
//! path to the full log is printed. It is printed *first* and flushed, so it
//! reaches Claude even if the Bash tool's timeout kills us mid-run.

use crate::config::Config;
use crate::filters::{self, generic, Cmd};
use crate::stats::{self, Record};
use crate::text;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// How many logs to keep; older ones are pruned on each run.
const MAX_LOGS: usize = 200;

pub struct RunArgs {
    pub filter: Option<String>,
    pub session: String,
    /// The output is piped into `tail`/`head`: print the log path last as
    /// well as first, so it survives whichever end the slice keeps.
    pub piped: bool,
    pub argv: Vec<String>,
}

/// Result of compacting one command's output.
#[derive(Debug, PartialEq, Eq)]
pub struct Compacted {
    pub filter: &'static str,
    pub text: String,
    /// True when `text` dropped or rewrote something from the cleaned output.
    pub changed: bool,
}

/// Pure compaction step (no I/O), used by `hk run` and by the fixture tests.
pub fn compact(
    filter: Option<&'static dyn filters::Filter>,
    raw: &str,
    exit: i32,
    cfg: &Config,
) -> Compacted {
    let cleaned = text::clean(raw);
    let cleaned_trim = cleaned.trim_end();
    let specific =
        filter.and_then(|f| f.filter(&cleaned, exit, &cfg.limits).map(|t| (f.name(), t)));
    let (name, text) = match specific {
        // A "summary" longer than the output itself is no summary.
        Some((n, t)) if t.chars().count() < cleaned_trim.chars().count() => (n, t),
        _ => (generic::NAME, generic::filter(&cleaned, &cfg.limits)),
    };
    let changed = text != cleaned_trim || raw.trim_end() != cleaned_trim;
    Compacted {
        filter: name,
        text,
        changed,
    }
}

pub fn run(args: RunArgs) -> i32 {
    if args.argv.is_empty() {
        eprintln!("hk run: missing command (usage: hk run [--filter NAME] [--session ID] -- <cmd> [args...])");
        return 2;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cfg = Config::load(&cwd);
    let filter = match &args.filter {
        Some(n) => filters::by_name(n),
        None => Cmd::from_argv(&args.argv)
            .and_then(|c| crate::hook::choose_filter(&c, Some(&cwd), &cfg))
            .and_then(filters::by_name),
    };
    let log_path = new_log_path(filter.map_or(generic::NAME, |f| f.name()));
    let mut log = open_log(&log_path, &args.argv, &cwd);

    let mut stdout = std::io::stdout();
    if log.is_some() {
        let _ = writeln!(stdout, "[hk] log: {}", log_path.display());
        let _ = stdout.flush();
    }

    let (raw, status) = match execute(&args.argv, log.as_mut()) {
        Ok(v) => v,
        Err(e) => {
            let _ = writeln!(stdout, "hk: failed to run `{}`: {e}", args.argv[0]);
            return 127;
        }
    };
    let exit = exit_code(status);
    if let Some(f) = log.as_mut() {
        let _ = writeln!(f, "\n[hk] exit code: {exit}");
    }

    let c = compact(filter, &raw, exit, &cfg);
    let _ = writeln!(stdout, "{}", c.text);
    if args.piped && log.is_some() {
        let _ = writeln!(stdout, "[hk] log: {}", log_path.display());
    }
    let _ = stdout.flush();

    // The log is kept even when nothing was hidden: its path was already printed.
    let rec = Record {
        ts: now(),
        session: args.session,
        filter: c.filter.to_string(),
        raw: raw.chars().count(),
        out: c.text.chars().count() + log_path.as_os_str().len() + 16,
    };
    let _ = stats::append(&crate::paths::stats_file(), &rec);
    prune_logs(&crate::paths::logs_dir());
    exit
}

/// Run `argv` with stdout and stderr merged into one pipe (preserving their
/// interleaving), teeing every chunk to `log` as it arrives.
fn execute(
    argv: &[String],
    mut log: Option<&mut std::fs::File>,
) -> std::io::Result<(String, ExitStatus)> {
    let (mut reader, writer) = std::io::pipe()?;
    let mut child = spawn(argv, &writer)?;
    drop(writer);
    let mut raw = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if let Some(f) = log.as_deref_mut() {
            let _ = f.write_all(&buf[..n]);
        }
        raw.extend_from_slice(&buf[..n]);
    }
    let status = child.wait()?;
    Ok((String::from_utf8_lossy(&raw).into_owned(), status))
}

fn spawn(argv: &[String], writer: &std::io::PipeWriter) -> std::io::Result<std::process::Child> {
    let build = |prog: &str, args: &[String]| -> std::io::Result<Command> {
        let mut c = Command::new(prog);
        c.args(args)
            .stdin(Stdio::inherit())
            .stdout(writer.try_clone()?)
            .stderr(writer.try_clone()?);
        Ok(c)
    };
    match build(&argv[0], &argv[1..])?.spawn() {
        Ok(c) => Ok(c),
        // Windows: `npm`, `pnpm`, `npx` are `.cmd` shims that CreateProcess
        // cannot start directly; run those through `cmd /C`.
        Err(e) if cfg!(windows) && e.kind() == std::io::ErrorKind::NotFound => {
            let shim = crate::format::which(&argv[0], Path::new(".")).filter(|p| {
                p.extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| x.eq_ignore_ascii_case("cmd") || x.eq_ignore_ascii_case("bat"))
            });
            match shim {
                Some(p) => {
                    let mut args = vec!["/C".to_string(), p.to_string_lossy().into_owned()];
                    args.extend_from_slice(&argv[1..]);
                    build("cmd", &args)?.spawn()
                }
                None => Err(e),
            }
        }
        Err(e) => Err(e),
    }
}

fn exit_code(status: ExitStatus) -> i32 {
    if let Some(c) = status.code() {
        return c;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    1
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn new_log_path(filter: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    crate::paths::logs_dir().join(format!("{nanos}-{}-{filter}.log", std::process::id()))
}

fn open_log(path: &Path, argv: &[String], cwd: &Path) -> Option<std::fs::File> {
    std::fs::create_dir_all(path.parent()?).ok()?;
    let mut f = std::fs::File::create(path).ok()?;
    let _ = writeln!(
        f,
        "[hk] cwd: {}\n[hk] command: {}\n",
        cwd.display(),
        argv.join(" ")
    );
    Some(f)
}

fn prune_logs(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "log"))
        .collect();
    if names.len() <= MAX_LOGS {
        return;
    }
    // File names start with a nanosecond timestamp, so lexical order is age order.
    names.sort();
    for p in &names[..names.len() - MAX_LOGS] {
        let _ = std::fs::remove_file(p);
    }
}
