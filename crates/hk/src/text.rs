//! Text helpers shared by every filter: ANSI stripping, line normalisation and
//! head/tail truncation.

/// Remove ANSI escape sequences (CSI, OSC and two-byte escapes), normalise
/// `\r\n` to `\n`, and collapse carriage-return overwrites (progress bars) so
/// that only the last segment of each line survives.
pub fn clean(input: &str) -> String {
    let stripped = strip_ansi(input);
    let mut out = String::with_capacity(stripped.len());
    for (i, line) in stripped.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        // A bare `\r` means "return to column 0": keep the last non-empty segment.
        let seg = line
            .rsplit('\r')
            .find(|s| !s.trim().is_empty())
            .unwrap_or("");
        out.push_str(seg.trim_end());
    }
    out
}

/// Remove ANSI escape sequences only.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                // CSI: parameters and intermediates, then one final byte 0x40..=0x7e.
                for n in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&n) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                // OSC: terminated by BEL or ESC \.
                while let Some(n) = chars.next() {
                    if n == '\u{7}' {
                        break;
                    }
                    if n == '\u{1b}' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

/// Keep the first `head` and last `tail` lines when `lines.len() > max`.
/// Returns `None` when nothing needs to be cut.
pub fn head_tail(text: &str, max: usize, head: usize, tail: usize) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max || lines.len() <= head + tail {
        return None;
    }
    let omitted = lines.len() - head - tail;
    let mut out = lines[..head].join("\n");
    out.push_str(&format!("\n[hk] ... {omitted} lines omitted ...\n"));
    out.push_str(&lines[lines.len() - tail..].join("\n"));
    Some(out)
}

/// Push `items` into `out`, capped at `cap`, adding a `+N more` line.
pub fn push_capped(out: &mut Vec<String>, items: Vec<String>, cap: usize) {
    let total = items.len();
    out.extend(items.into_iter().take(cap));
    if total > cap {
        out.push(format!("+{} more", total - cap));
    }
}

/// `FAIL name` followed by indented body lines.
pub fn fail_block(name: &str, body: &[String]) -> String {
    let mut s = format!("FAIL {name}");
    for b in body {
        s.push_str("\n  ");
        s.push_str(b);
    }
    s
}

/// Order-preserving de-duplication.
pub fn dedup(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|s| seen.insert(s.clone()))
        .collect()
}

/// Extract the integer that directly precedes `word` (e.g. `"3 passed"`).
pub fn count_before(line: &str, word: &str) -> Option<u64> {
    let idx = line.find(word)?;
    let before = line[..idx].trim_end();
    let digits: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_and_osc() {
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m"), "red");
        assert_eq!(
            strip_ansi("\u{1b}]8;;http://x\u{7}link\u{1b}]8;;\u{7}"),
            "link"
        );
    }

    #[test]
    fn collapses_carriage_returns() {
        assert_eq!(clean("10%\r50%\r100%\r\nnext"), "100%\nnext");
    }

    #[test]
    fn head_tail_cuts_middle() {
        let text: String = (1..=100).map(|i| format!("{i}\n")).collect();
        let out = head_tail(&text, 60, 2, 2).unwrap();
        assert_eq!(out, "1\n2\n[hk] ... 96 lines omitted ...\n99\n100");
        assert!(head_tail("a\nb", 60, 30, 30).is_none());
    }

    #[test]
    fn counts_numbers() {
        assert_eq!(
            count_before("== 1 failed, 22 passed in 0.1s", "passed"),
            Some(22)
        );
        assert_eq!(count_before("nothing here", "passed"), None);
    }
}
