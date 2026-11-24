//! Minimal, conservative parsing of Bash tool commands for the rewrite.
//!
//! We only rewrite *simple* commands: optional `cd DIR &&` prefix, optional
//! `NAME=value` assignments, then words. Anything with pipes, redirections,
//! lists, subshells, command substitution, globs we would mis-tokenise, or
//! newlines is left untouched. The suffix after the assignments is passed to
//! `hk run` verbatim, so Bash still does the real quoting and expansion; our
//! tokenisation is only used to pick a filter.

#[derive(Debug, PartialEq, Eq)]
pub struct Simple<'a> {
    /// `cd DIR && ` including the trailing space, or empty.
    pub prefix: &'a str,
    /// `A=1 B=2 ` including the trailing space, or empty.
    pub env: &'a str,
    /// The command itself, verbatim.
    pub rest: &'a str,
    /// `rest` split into words with quotes removed.
    pub words: Vec<String>,
}

pub fn parse(command: &str) -> Option<Simple<'_>> {
    let command = command.trim();
    let (prefix, body) = split_cd_prefix(command);
    if has_metachars(body) {
        return None;
    }
    let words = split_words(body)?;
    let n_env = words.iter().take_while(|w| is_assignment(w)).count();
    if n_env == words.len() {
        return None;
    }
    // Locate the verbatim start of the first non-assignment word.
    let mut idx = 0;
    for _ in 0..n_env {
        idx = skip_word(body, idx);
    }
    let rest_start = idx + body[idx..].len() - body[idx..].trim_start().len();
    let first = &words[n_env];
    if first.contains('$') || first.contains('`') {
        return None;
    }
    Some(Simple {
        prefix,
        env: &body[..rest_start],
        rest: &body[rest_start..],
        words: words[n_env..].to_vec(),
    })
}

/// Split `cd some/dir && cmd` into (`cd some/dir && `, `cmd`).
fn split_cd_prefix(command: &str) -> (&str, &str) {
    if let Some(after_cd) = command.strip_prefix("cd ") {
        if let Some(pos) = after_cd.find("&&") {
            let dir = after_cd[..pos].trim();
            if !dir.is_empty() && !has_metachars(dir) && !dir.contains(char::is_whitespace) {
                let split = 3 + pos + 2;
                let body = command[split..].trim_start();
                let prefix_len = command.len() - body.len();
                return (&command[..prefix_len], body);
            }
        }
    }
    ("", command)
}

/// True when `s` contains shell syntax outside of single quotes that we do
/// not want to reason about.
fn has_metachars(s: &str) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '\\' if !in_single => {
                chars.next();
            }
            '\n' | '\r' => return true,
            _ if in_single => {}
            '`' => return true,
            '$' if chars.peek() == Some(&'(') => return true,
            '|' | '&' | ';' | '<' | '>' | '(' | ')' if !in_double => return true,
            _ => {}
        }
    }
    in_single || in_double
}

fn is_assignment(w: &str) -> bool {
    match w.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit())
        }
        None => false,
    }
}

/// Split into words, honouring single quotes, double quotes and backslashes.
fn split_words(s: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                has = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        x => cur.push(x),
                    }
                }
            }
            '"' => {
                has = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => cur.push(chars.next()?),
                        x => cur.push(x),
                    }
                }
            }
            '\\' => {
                has = true;
                cur.push(chars.next()?);
            }
            c if c.is_whitespace() => {
                if has {
                    words.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                has = true;
                cur.push(c);
            }
        }
    }
    if has {
        words.push(cur);
    }
    Some(words)
}

/// Byte index just past the word starting at or after `i` (quotes respected).
fn skip_word(s: &str, i: usize) -> usize {
    let bytes = s.as_bytes();
    let mut j = i;
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    let mut quote: Option<u8> = None;
    while j < bytes.len() {
        let b = bytes[j];
        match quote {
            Some(q) if b == q => quote = None,
            Some(b'"') if b == b'\\' => j += 1,
            Some(_) => {}
            None if b == b'\'' || b == b'"' => quote = Some(b),
            None if b == b'\\' => j += 1,
            None if b.is_ascii_whitespace() => break,
            None => {}
        }
        j += 1;
    }
    j.min(s.len())
}

/// Quote a path for Bash (single quotes; works in Git Bash on Windows too).
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_commands() {
        let p = parse("cargo test --lib").unwrap();
        assert_eq!((p.prefix, p.env, p.rest), ("", "", "cargo test --lib"));
        assert_eq!(p.words, vec!["cargo", "test", "--lib"]);
    }

    #[test]
    fn keeps_cd_and_env_outside() {
        let p =
            parse("cd crates/hk && RUST_LOG=debug FOO='a b' cargo test -- --nocapture").unwrap();
        assert_eq!(p.prefix, "cd crates/hk && ");
        assert_eq!(p.env, "RUST_LOG=debug FOO='a b' ");
        assert_eq!(p.rest, "cargo test -- --nocapture");
    }

    #[test]
    fn keeps_quoted_args() {
        let p = parse(r#"pytest -k "not slow" tests/"#).unwrap();
        assert_eq!(p.words, vec!["pytest", "-k", "not slow", "tests/"]);
        assert!(parse("pytest -k 'a|b'").is_some());
    }

    #[test]
    fn rejects_compound_commands() {
        for c in [
            "cargo test | tail",
            "cargo test 2>&1",
            "cargo test; echo done",
            "cargo build && cargo test",
            "cargo test > out.txt",
            "echo $(cargo test)",
            "(cargo test)",
            "cargo test &",
            "cargo test\necho",
            "FOO=1",
            "$CARGO test",
            "cd a b && cargo test",
        ] {
            assert!(parse(c).is_none(), "{c}");
        }
    }

    #[test]
    fn quotes_paths() {
        assert_eq!(quote("/a b/hk"), "'/a b/hk'");
        assert_eq!(quote("it's"), r"'it'\''s'");
    }
}
