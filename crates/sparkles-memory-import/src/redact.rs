//! Redaction (spec C18 §8.10.7): every imported text passes it before it leaves the
//! machine. Each match is replaced with `[redacted:<pattern name>]`. Pattern matching
//! cannot find every secret, so the narrowest content setting is still the advice.

use regex::Regex;

/// A named pattern. When the expression has a group named `v`, only that group is
/// replaced, so `Authorization: Bearer x` keeps its header name.
#[derive(Clone, Debug)]
pub struct Pattern {
    pub name: String,
    pub regex: Regex,
}

impl Pattern {
    /// A pattern from a name and an expression in the syntax of Rust's `regex` crate.
    pub fn new(name: &str, expr: &str) -> Result<Pattern, String> {
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err(format!(
                "invalid pattern name {name:?}: use 1 to 64 letters, digits, - or _"
            ));
        }
        let regex = Regex::new(expr).map_err(|e| format!("pattern {name}: {e}"))?;
        Ok(Pattern {
            name: name.to_string(),
            regex,
        })
    }
}

/// The built-in list of §8.10.7, in priority order: an earlier pattern wins over a later
/// one that matches at the same place.
pub fn builtin() -> Vec<Pattern> {
    const LIST: &[(&str, &str)] = &[
        (
            "private-key",
            r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
        ),
        ("aws-access-key-id", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
        (
            "aws-secret-key",
            r#"(?i)aws_?secret_?access_?key\s*[:=]\s*['"]?(?P<v>[A-Za-z0-9/+=]{40})"#,
        ),
        (
            "github-token",
            r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36,}\b|\bgithub_pat_[A-Za-z0-9_]{22,}",
        ),
        ("slack-token", r"\bxox[abposr]-[A-Za-z0-9-]{10,}"),
        ("anthropic-key", r"\bsk-ant-[A-Za-z0-9_-]{20,}"),
        ("openai-key", r"\bsk-[A-Za-z0-9_-]{20,}"),
        ("google-api-key", r"\bAIza[0-9A-Za-z_-]{35}"),
        (
            "jwt",
            r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
        ),
        (
            "authorization-header",
            r"(?i)authorization\s*:\s*(?:bearer|basic)\s+(?P<v>[A-Za-z0-9._~+/=-]{8,})",
        ),
        (
            "url-password",
            r"\b[A-Za-z][A-Za-z0-9+.-]*://[^\s:/@]+:(?P<v>[^\s@/]+)@",
        ),
        (
            "secret-assignment",
            r#"(?i)\b[A-Za-z0-9_.-]*(?:key|secret|token|password|passwd)[A-Za-z0-9_.-]*["']?\s*[:=]\s*["']?(?P<v>[^\s"'`,;]{12,})"#,
        ),
    ];
    LIST.iter()
        .map(|(n, e)| Pattern::new(n, e).expect("the built-in patterns compile"))
        .collect()
}

/// What redaction did to one text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Redacted {
    pub text: String,
    /// the name of the pattern of each replacement, in order
    pub names: Vec<String>,
}

/// Replace every match of `patterns` in `text`. Matches are found in the original text
/// and do not overlap; at one place the longest match of the earliest pattern wins.
pub fn redact(text: &str, patterns: &[Pattern]) -> Redacted {
    // (start, end, pattern index)
    let mut found: Vec<(usize, usize, usize)> = Vec::new();
    for (pi, p) in patterns.iter().enumerate() {
        for c in p.regex.captures_iter(text) {
            let m = c.name("v").or_else(|| c.get(0)).expect("group 0");
            if m.start() == m.end() || text[m.start()..m.end()].starts_with("[redacted:") {
                continue;
            }
            found.push((m.start(), m.end(), pi));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.cmp(&b.2)).then(b.1.cmp(&a.1)));
    let mut out = String::with_capacity(text.len());
    let mut names = Vec::new();
    let mut at = 0;
    for (s, e, pi) in found {
        if s < at {
            continue;
        }
        out.push_str(&text[at..s]);
        out.push_str("[redacted:");
        out.push_str(&patterns[pi].name);
        out.push(']');
        names.push(patterns[pi].name.clone());
        at = e;
    }
    out.push_str(&text[at..]);
    Redacted { text: out, names }
}

/// The first place where `patterns` match `text`, as the pattern's name and the byte
/// offset of the match, with the rules of [`redact`]: the earliest match wins, and at
/// one place the earlier pattern. A redaction marker never matches. The server runs this
/// as its `secret-detected` check.
pub fn first_match(text: &str, patterns: &[Pattern]) -> Option<(String, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for (pi, p) in patterns.iter().enumerate() {
        for c in p.regex.captures_iter(text) {
            let m = c.name("v").or_else(|| c.get(0)).expect("group 0");
            if m.start() == m.end() || text[m.start()..].starts_with("[redacted:") {
                continue;
            }
            if best.is_none_or(|b| (m.start(), pi) < b) {
                best = Some((m.start(), pi));
            }
            break;
        }
    }
    best.map(|(at, pi)| (patterns[pi].name.clone(), at))
}

/// Patterns from a file: one `name<TAB or space>regex` per line; `#` starts a comment
/// line.
pub fn parse_pattern_file(text: &str) -> Result<Vec<Pattern>, String> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some((name, expr)) = t.split_once(char::is_whitespace) else {
            return Err(format!(
                "line {}: expected a name and a regular expression",
                n + 1
            ));
        };
        out.push(
            Pattern::new(name.trim(), expr.trim()).map_err(|e| format!("line {}: {e}", n + 1))?,
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_patterns() {
        let p = builtin();
        let gh = format!("token ghp_{}", "a".repeat(36));
        let r = redact(&gh, &p);
        assert_eq!(r.text, "token [redacted:github-token]");
        assert_eq!(r.names, vec!["github-token"]);
        let r = redact("key: sk-ant-api03-abcdefghijklmnopqrstuvwxyz", &p);
        assert!(r.text.contains("[redacted:anthropic-key]"), "{}", r.text);
        assert_eq!(r.names.len(), 1);
        let r = redact("Authorization: Bearer abcdefghijkl.mnop", &p);
        assert_eq!(
            r.text,
            "Authorization: Bearer [redacted:authorization-header]"
        );
        let r = redact("postgres://app:hunter2@db:5432/x", &p);
        assert_eq!(r.text, "postgres://app:[redacted:url-password]@db:5432/x");
        let r = redact("DB_PASSWORD=correct-horse-battery", &p);
        assert_eq!(r.text, "DB_PASSWORD=[redacted:secret-assignment]");
        let r = redact(
            "-----BEGIN RSA PRIVATE KEY-----\nabc\n-----END RSA PRIVATE KEY-----\nafter",
            &p,
        );
        assert_eq!(r.text, "[redacted:private-key]\nafter");
        let r = redact("AKIAABCDEFGHIJKLMNOP and AIza".to_string().as_str(), &p);
        assert_eq!(r.names, vec!["aws-access-key-id"]);
        // short or ordinary values stay
        let r = redact("port 5433, password: short, the token budget", &p);
        assert!(r.names.is_empty(), "{:?}", r.names);
    }

    #[test]
    fn extra_patterns() {
        let mut p = builtin();
        p.extend(
            parse_pattern_file("# a comment\nacme-deploy-key acme_dk_[A-Za-z0-9]{8}\n").unwrap(),
        );
        let r = redact("use acme_dk_ABCDEFGH now", &p);
        assert_eq!(r.text, "use [redacted:acme-deploy-key] now");
        assert!(parse_pattern_file("bad! x").is_err());
        // the server's check finds the first match and never a marker
        let gh = format!("ok\nuse ghp_{} now", "b".repeat(36));
        assert_eq!(first_match(&gh, &p), Some(("github-token".into(), 7)));
        let r = redact(&gh, &p);
        assert_eq!(first_match(&r.text, &p), None);
        assert_eq!(first_match("nothing here", &p), None);
        assert!(parse_pattern_file("x (").is_err());
    }
}
