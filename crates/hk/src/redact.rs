//! Mask well-known secret shapes before a line reaches a log on disk.
//!
//! The Bash tool may print credentials (`curl -v` headers, `env`, a database
//! URL in a connection error). What Claude sees is the command's business;
//! what hk keeps on disk must not outlive it. No regex crate: each shape is a
//! small scanner, and every mask keeps the recognisable prefix so the log
//! still says *what* was there.

const MASK: &str = "***";

/// Prefixes of tokens issued by common providers. Masked when followed by at
/// least 12 token characters.
const TOKEN_PREFIXES: &[&str] = &[
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "sk-ant-",
    "sk-proj-",
    "sk-",
    "ovk_",
    "npm_",
    "sqp_",
    "squ_",
    "sqa_",
    "AKIA",
    "ASIA",
    "AIza",
    "ya29.",
    "hf_",
];

/// Keys whose `key=value` / `key: value` / `"key": "value"` value is masked.
const SECRET_KEYS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "api-key",
    "access_key",
    "private_key",
    "client_secret",
    "authorization",
];

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '+' | '=')
}

/// Mask every secret shape found in `line`.
pub fn line(line: &str) -> String {
    if line.contains("PRIVATE KEY-----") || line.contains("-----BEGIN") {
        return format!("[hk] {MASK} (private key material)");
    }
    let s = bearer(line);
    let s = url_credentials(&s);
    let s = prefixed_tokens(&s);
    key_values(&s)
}

/// `Bearer abc...`, `Basic abc...`, `token abc...` after `Authorization:`.
fn bearer(s: &str) -> String {
    let mut out = s.to_string();
    for scheme in ["Bearer ", "bearer ", "Basic ", "basic "] {
        let mut from = 0;
        while let Some(pos) = out[from..].find(scheme) {
            let start = from + pos + scheme.len();
            let end = start
                + out[start..]
                    .find(|c: char| !is_token_char(c))
                    .unwrap_or(out.len() - start);
            if end - start >= 8 {
                out.replace_range(start..end, MASK);
                from = start + MASK.len();
            } else {
                from = end;
            }
        }
    }
    out
}

/// `scheme://user:password@host` -> `scheme://user:***@host`.
fn url_credentials(s: &str) -> String {
    let mut out = s.to_string();
    let mut from = 0;
    while let Some(pos) = out[from..].find("://") {
        let auth_start = from + pos + 3;
        let rest = &out[auth_start..];
        let stop = rest
            .find(|c: char| c == '/' || c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(rest.len());
        let authority = &rest[..stop];
        match (authority.rfind('@'), authority.find(':')) {
            (Some(at), Some(colon)) if colon < at && at - colon > 1 => {
                let a = auth_start + colon + 1;
                let b = auth_start + at;
                out.replace_range(a..b, MASK);
                from = a + MASK.len();
            }
            _ => from = auth_start,
        }
    }
    out
}

fn prefixed_tokens(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < s.len() {
        let at_boundary = i == 0 || !is_token_char(bytes[i - 1] as char);
        let hit = at_boundary
            .then(|| TOKEN_PREFIXES.iter().find(|p| s[i..].starts_with(*p)))
            .flatten();
        if let Some(p) = hit {
            let body_start = i + p.len();
            let body_len = s[body_start..]
                .find(|c: char| !is_token_char(c))
                .unwrap_or(s.len() - body_start);
            if body_len >= 12 {
                out.push_str(p);
                out.push_str(MASK);
                i = body_start + body_len;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// `PASSWORD=x`, `api_key: x`, `"token": "x"` (case-insensitive key match).
fn key_values(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = s.to_string();
    // Work right to left so earlier byte offsets stay valid.
    let mut hits: Vec<(usize, usize)> = Vec::new();
    for key in SECRET_KEYS {
        let mut from = 0;
        while let Some(pos) = lower[from..].find(key) {
            let k_end = from + pos + key.len();
            from = k_end;
            // The key may be a suffix of a longer name (`DB_PASSWORD`), but
            // must end here: `[A-Za-z0-9_]*key` then quotes/space and = or :.
            let after = &s[k_end..];
            let skip_q = after.len() - after.trim_start_matches(['"', '\'']).len();
            let rest = &after[skip_q..];
            let rest_t = rest.trim_start();
            let Some(sep) = rest_t.chars().next().filter(|c| *c == '=' || *c == ':') else {
                continue;
            };
            let v_off = k_end + skip_q + (rest.len() - rest_t.len()) + sep.len_utf8();
            let value = &s[v_off..];
            let lead = value.len() - value.trim_start_matches([' ', '"', '\'']).len();
            let v_start = v_off + lead;
            let v_len = s[v_start..]
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',' || c == '&')
                .unwrap_or(s.len() - v_start);
            let v = &s[v_start..v_start + v_len];
            let scheme = matches!(
                v,
                "Bearer" | "bearer" | "Basic" | "basic" | "Token" | "token"
            );
            if v_len >= 4 && v != MASK && !scheme {
                hits.push((v_start, v_start + v_len));
            }
        }
    }
    hits.sort();
    hits.dedup();
    for (a, b) in hits.into_iter().rev() {
        out.replace_range(a..b, MASK);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::line;

    #[test]
    fn masks_known_shapes() {
        assert_eq!(
            line("> Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.abc.def"),
            "> Authorization: Bearer ***"
        );
        assert_eq!(
            line("connect postgres://app:s3cr3t-pw@db:5432/x failed"),
            "connect postgres://app:***@db:5432/x failed"
        );
        assert_eq!(line("key ovk_abcdefghijklmnop end"), "key ovk_*** end");
        assert_eq!(line("AKIAIOSFODNN7EXAMPLE"), "AKIA***");
        assert_eq!(line("DB_PASSWORD=hunter2hunter2"), "DB_PASSWORD=***");
        assert_eq!(
            line(r#"{"api_key": "abcd1234efgh"}"#),
            r#"{"api_key": "***"}"#
        );
        assert!(line("-----BEGIN OPENSSH PRIVATE KEY-----").contains("private key"));
    }

    #[test]
    fn leaves_ordinary_text() {
        for s in [
            "test result: ok. 42 passed",
            "https://example.com/path",
            "task-runner: sk-8 done",
            "Tokens  12 passed",
            "",
        ] {
            assert_eq!(line(s), s);
        }
    }
}
