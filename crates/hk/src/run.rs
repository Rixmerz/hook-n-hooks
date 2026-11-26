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

/// Logs older than this are pruned on each run.
const MAX_AGE_SECS: u64 = 7 * 24 * 3600;
/// Above this many logs, the oldest are pruned, but never one written in the
/// last hour or by the running session.
const MAX_LOGS: usize = 1000;
const MIN_AGE_SECS: u64 = 3600;

pub struct RunArgs {
    pub filter: Option<String>,
    pub session: String,
    /// The output is piped into `tail`/`head`: print the log path last as
    /// well as first, so it survives whichever end the slice keeps.
    pub piped: bool,
    /// Capture stdout only and let stderr through untouched (for a pipe
    /// without `2>&1`, where the next stage never saw stderr).
    pub stdout_only: bool,
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
    let started = std::time::Instant::now();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut cfg = Config::load(&cwd);
    let parsed = Cmd::from_argv(&args.argv);
    if let Some(n) = parsed
        .as_ref()
        .and_then(|c| cfg.limits.commands.get(&c.program))
    {
        cfg.limits.generic_max_lines = *n;
    }
    let filter = match &args.filter {
        Some(n) => filters::by_name(n),
        None => parsed
            .as_ref()
            .and_then(|c| crate::hook::choose_filter(c, Some(&cwd), &cfg))
            .and_then(filters::by_name),
    };
    let filter_name = filter.map_or(generic::NAME, |f| f.name());
    let log_path = new_log_path(&args.session, filter_name);
    let mut log = open_log(&log_path, &args.argv, &cwd);

    let mut stdout = std::io::stdout();
    if log.is_some() {
        let _ = writeln!(stdout, "[hk] log: {}", log_path.display());
        let _ = stdout.flush();
    }

    let (raw, status) = match execute(&args.argv, log.as_mut(), !args.stdout_only) {
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
    let project = crate::paths::project_root(&cwd);
    let diff = crate::history::compare(&project, &args.argv, &c.text, filter_name);
    let _ = writeln!(stdout, "{}", c.text);
    if let Some(d) = &diff {
        let _ = writeln!(stdout, "{d}");
    }
    let secs = started.elapsed().as_secs_f64();
    let omitted = raw.lines().count().saturating_sub(c.text.lines().count());
    // One fixed, parseable line: scripts can read it, and it shows the time.
    let _ = write!(
        stdout,
        "[hk] filter={} exit={exit} time={secs:.1}s omitted={omitted}",
        c.filter
    );
    if args.piped && log.is_some() {
        let _ = write!(stdout, " log={}", log_path.display());
    }
    let _ = writeln!(stdout);
    let _ = stdout.flush();

    // The log is kept even when nothing was hidden: its path was already printed.
    let rec = Record {
        ts: now(),
        session: args.session.clone(),
        filter: c.filter.to_string(),
        raw: raw.chars().count(),
        out: c.text.chars().count() + log_path.as_os_str().len() + 64,
        project: project.display().to_string(),
        exit: Some(exit),
        ms: Some((secs * 1000.0) as u64),
        kind: String::new(),
    };
    let _ = stats::append(&crate::paths::stats_file(), &rec);
    prune_logs(&crate::paths::logs_dir(), &args.session);
    exit
}

/// Run `argv` with stdout and stderr merged into one pipe (preserving their
/// interleaving), teeing every chunk to `log` as it arrives.
fn execute(
    argv: &[String],
    mut log: Option<&mut std::fs::File>,
    merge: bool,
) -> std::io::Result<(String, ExitStatus)> {
    let (mut reader, writer) = std::io::pipe()?;
    let mut child = spawn(argv, &writer, merge)?;
    drop(writer);
    let mut raw = Vec::new();
    let mut pending: Vec<u8> = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if let Some(f) = log.as_deref_mut() {
            // Masked line by line, so a secret split across reads is still seen.
            pending.extend_from_slice(&buf[..n]);
            if let Some(nl) = pending.iter().rposition(|b| *b == b'\n') {
                let done: Vec<u8> = pending.drain(..=nl).collect();
                let _ = f.write_all(mask(&done).as_bytes());
            }
        }
        raw.extend_from_slice(&buf[..n]);
    }
    if let Some(f) = log {
        let _ = f.write_all(mask(&pending).as_bytes());
    }
    let status = child.wait()?;
    Ok((String::from_utf8_lossy(&raw).into_owned(), status))
}

fn mask(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    for l in text.split_inclusive('\n') {
        let (body, nl) = match l.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (l, ""),
        };
        out.push_str(&crate::redact::line(body));
        out.push_str(nl);
    }
    out
}

fn spawn(
    argv: &[String],
    writer: &std::io::PipeWriter,
    merge: bool,
) -> std::io::Result<std::process::Child> {
    let build = |prog: &str, args: &[String]| -> std::io::Result<Command> {
        let mut c = Command::new(prog);
        c.args(args)
            .stdin(Stdio::inherit())
            .stdout(writer.try_clone()?);
        if merge {
            c.stderr(writer.try_clone()?);
        } else {
            c.stderr(Stdio::inherit());
        }
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

/// `<nanos>-<pid>-<session>-<filter>.log`. The session and filter are
/// sanitised to `[A-Za-z0-9_-]`, so `-` splits are only used for the prefix.
fn new_log_path(session: &str, filter: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let session: String = session
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(64)
        .collect();
    crate::paths::logs_dir().join(format!(
        "{nanos}-{}-{session}-{filter}.log",
        std::process::id()
    ))
}

fn open_log(path: &Path, argv: &[String], cwd: &Path) -> Option<std::fs::File> {
    crate::paths::create_private_dir(path.parent()?).ok()?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).ok()?;
    let _ = writeln!(
        f,
        "[hk] cwd: {}\n[hk] command: {}\n",
        cwd.display(),
        crate::redact::line(&argv.join(" "))
    );
    Some(f)
}

/// Seconds since the epoch encoded in a log name, if any.
pub fn log_time(p: &Path) -> Option<u64> {
    let name = p.file_name()?.to_str()?;
    let nanos: u128 = name.split('-').next()?.parse().ok()?;
    Some((nanos / 1_000_000_000) as u64)
}

fn prune_logs(dir: &Path, session: &str) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let now = now();
    let mut names: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "log"))
        .collect();
    // File names start with a nanosecond timestamp, so lexical order is age order.
    names.sort();
    let tag = format!("-{session}-");
    let mut left = names.len();
    for p in &names {
        let age = log_time(p).map_or(u64::MAX, |t| now.saturating_sub(t));
        let mine = p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains(&tag));
        let expired = age > MAX_AGE_SECS;
        let over = left > MAX_LOGS && age > MIN_AGE_SECS && !mine;
        if expired || over {
            let _ = std::fs::remove_file(p);
            left -= 1;
        }
    }
}
