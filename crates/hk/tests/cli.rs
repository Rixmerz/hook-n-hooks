//! End-to-end tests of the `hk` binary. They spawn hk itself (via the hidden
//! `__emit` helper) instead of `sh`/`echo`, so they run on Windows too.

use std::io::Write;
use std::process::{Command, Stdio};

const HK: &str = env!("CARGO_BIN_EXE_hk");

fn data_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("hk-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn run_preserves_exit_code_and_prints_log_path() {
    let dir = data_dir("exit");
    for code in [0, 1, 3, 101] {
        let out = Command::new(HK)
            .args([
                "run",
                "--session",
                "t1",
                "--",
                HK,
                "__emit",
                &code.to_string(),
                "hello",
            ])
            .env("HK_DATA_DIR", &dir)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(code));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.starts_with("[hk] log: "), "{stdout}");
        assert!(stdout.contains("hello\n"), "{stdout}");
        assert!(stdout.contains("hello (stderr)"), "stderr merged: {stdout}");
        let log = stdout
            .lines()
            .next()
            .unwrap()
            .trim_start_matches("[hk] log: ");
        let log = std::fs::read_to_string(log).unwrap();
        assert!(log.contains("hello (stderr)"));
        assert!(log.contains(&format!("[hk] exit code: {code}")));
    }
    let stats = Command::new(HK)
        .args(["stats", "--session", "t1"])
        .env("HK_DATA_DIR", &dir)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&stats.stdout).contains("over 4 run(s)"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_reports_missing_program() {
    let dir = data_dir("missing");
    let out = Command::new(HK)
        .args(["run", "--", "hk-definitely-not-a-program"])
        .env("HK_DATA_DIR", &dir)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(127));
    let _ = std::fs::remove_dir_all(&dir);
}

fn hook(event: &str, input: &str) -> (i32, String) {
    let mut child = Command::new(HK)
        .args(["hook", event])
        .env("HK_DATA_DIR", data_dir(&format!("hook-{event}")))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn pre_tool_use_hook_rewrites_and_fails_open() {
    let (code, out) = hook(
        "pre-tool-use",
        r#"{"session_id":"s","permission_mode":"bypassPermissions","tool_name":"Bash","tool_input":{"command":"cargo test"}}"#,
    );
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let cmd = v["hookSpecificOutput"]["updatedInput"]["command"]
        .as_str()
        .unwrap();
    assert!(
        cmd.ends_with(" run --filter cargo_test --session s -- cargo test"),
        "{cmd}"
    );
    assert!(v["hookSpecificOutput"].get("permissionDecision").is_none());

    for bad in ["", "not json", "{}", r#"{"tool_name":"Bash"}"#] {
        assert_eq!(hook("pre-tool-use", bad), (0, String::new()), "{bad}");
    }
}

#[test]
fn session_start_prints_repo_map() {
    let cwd = env!("CARGO_MANIFEST_DIR").replace('\\', "\\\\");
    let (code, out) = hook("session-start", &format!(r#"{{"cwd":"{cwd}"}}"#));
    assert_eq!(code, 0);
    assert!(out.starts_with("[hk] repo map"), "{out}");
    assert!(out.contains("stack: rust"), "{out}");
    assert!(out.contains("src/"), "{out}");
}

#[test]
fn stdout_only_lets_stderr_through() {
    let dir = data_dir("stdout-only");
    let out = Command::new(HK)
        .args([
            "run",
            "--stdout-only",
            "--piped",
            "--",
            HK,
            "__emit",
            "0",
            "hi",
        ])
        .env("HK_DATA_DIR", &dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stdout.contains("(stderr)"), "{stdout}");
    assert!(stderr.contains("hi (stderr)"), "{stderr}");
    let last = stdout.lines().last().unwrap();
    assert!(
        last.starts_with("[hk] filter=") && last.contains(" exit=0 ") && last.contains(" log="),
        "{last}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn logs_are_private_and_masked() {
    let dir = data_dir("private");
    let out = Command::new(HK)
        .args([
            "run",
            "--session",
            "s9",
            "--",
            HK,
            "__emit",
            "0",
            "Authorization: Bearer abcdefghijklmnopqrstuv",
        ])
        .env("HK_DATA_DIR", &dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let log = stdout
        .lines()
        .next()
        .unwrap()
        .trim_start_matches("[hk] log: ")
        .to_string();
    assert!(log.contains("-s9-"), "{log}");
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(!text.contains("abcdefghijklmnop"), "{text}");
    assert!(text.contains("Bearer ***"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&log).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    // `hk log last --grep` finds it without opening the file by hand.
    let shown = Command::new(HK)
        .args(["log", "last", "--session", "s9", "--grep", "Bearer"])
        .env("HK_DATA_DIR", &dir)
        .output()
        .unwrap();
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown.contains("     4  Authorization: Bearer ***"),
        "{shown}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn explain_says_why() {
    let ex = |c: &str| {
        let o = Command::new(HK)
            .args(["explain", c])
            .env("HK_DATA_DIR", data_dir("explain"))
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    assert!(ex("cargo test && cargo build").contains("cargo_test, build"));
    assert!(ex("cat <<EOF\nx\nEOF").contains("heredoc"));
    assert!(ex("ls -la > out.txt").contains("redirection"));
    assert!(ex("openspec validate").contains("no filter for `openspec`"));
}

#[test]
fn hook_handles_chains_and_stdout_only_pipes() {
    let input = r#"{"session_id":"s","cwd":"/tmp","permission_mode":"auto","tool_name":"Bash","tool_input":{"command":"cargo build && cargo test | tail -40"}}"#;
    let mut child = Command::new(HK)
        .args(["hook", "pre-tool-use"])
        .env("HK_DATA_DIR", data_dir("hook-chain"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let cmd = v["hookSpecificOutput"]["updatedInput"]["command"]
        .as_str()
        .unwrap();
    assert!(
        cmd.contains("--filter build --session s -- cargo build && "),
        "{cmd}"
    );
    assert!(
        cmd.ends_with("--piped --stdout-only -- cargo test | tail -40"),
        "{cmd}"
    );
}
