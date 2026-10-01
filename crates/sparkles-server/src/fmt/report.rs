//! What `sparkles fmt` prints: compiler-style errors and warnings, unified diffs and the
//! `--check` summary.

use sparkles_fmt::{FormatError, Language, Warning};

/// `path:LINE:COL: error: …`, or `path: error: …` without a position.
pub fn error_line(name: &str, lang: Language, e: &FormatError) -> String {
    match e {
        FormatError::Syntax {
            message,
            line,
            column,
            ..
        } => format!(
            "{name}:{line}:{column}: error: {} syntax error: {}",
            lang.display_name(),
            short_message(message)
        ),
        FormatError::Unsupported {
            message,
            line,
            column,
        } => format!(
            "{name}:{line}:{column}: error: the formatter cannot handle this yet ({message}); input left unchanged; please report"
        ),
        e => format!("{name}: error: {e}"),
    }
}

/// A parser message cut to its first line and about 120 characters, at a list separator.
/// spargebra lists every token it expected, character classes over several lines included;
/// the position says where, and the head of the list is enough to say what.
pub fn short_message(message: &str) -> String {
    const MAX: usize = 120;
    let message = message.trim();
    let first = message.lines().next().unwrap_or("").trim_end();
    if first.len() == message.len() && first.chars().count() <= MAX {
        return first.to_string();
    }
    let end = first
        .char_indices()
        .nth(MAX)
        .map_or(first.len(), |(i, _)| i);
    let head = &first[..end];
    match head.rfind(", ") {
        Some(i) if i > 0 => format!("{}, …", &head[..i]),
        _ => format!("{}…", head.trim_end()),
    }
}

/// An I/O error without Rust's ` (os error N)` suffix.
pub fn io(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(i) if s.ends_with(')') => s[..i].to_string(),
        _ => s,
    }
}

/// `path:LINE:COL: warning: …`, or `path: warning: …` without a position.
pub fn warning_line(name: &str, w: &Warning) -> String {
    match w.line {
        0 => format!("{name}: warning: {}", w.message),
        l => format!("{name}:{l}:{}: warning: {}", w.column, w.message),
    }
}

/// A unified diff from `old` to `new` with three lines of context and `--- a/path`,
/// `+++ b/path` headers.
pub fn diff(name: &str, old: &str, new: &str) -> String {
    similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{name}"), &format!("b/{name}"))
        .to_string()
}

/// The last line of `--check`.
pub fn check_summary(changed: usize, errors: usize) -> Option<String> {
    match (changed, errors) {
        (0, 0) => Some("All matched files are formatted.".to_string()),
        (0, _) => None,
        (1, _) => Some(
            "[warn] Code style issues found in the above file. Run sparkles fmt --write to fix."
                .to_string(),
        ),
        (n, _) => Some(format!(
            "[warn] Code style issues found in {n} files. Run sparkles fmt --write to fix."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_and_warnings() {
        let e = FormatError::Syntax {
            message: "expected one of …".into(),
            line: 3,
            column: 14,
            offset: 40,
        };
        assert_eq!(
            error_line("queries/bad.rq", Language::Sparql, &e),
            "queries/bad.rq:3:14: error: SPARQL syntax error: expected one of …"
        );
        let e = FormatError::Unsafe {
            check: sparkles_fmt::Check::Algebra,
        };
        assert_eq!(
            error_line("q.rq", Language::Sparql, &e),
            "q.rq: error: formatter refused its own output (algebra differs); input left unchanged; please report"
        );
        let w = Warning {
            code: "undeclared-prefix",
            message: "prefix ex: is not declared".into(),
            line: 2,
            column: 7,
        };
        assert_eq!(
            warning_line("q.rq", &w),
            "q.rq:2:7: warning: prefix ex: is not declared"
        );
    }

    #[test]
    fn short_messages() {
        assert_eq!(
            short_message("expected one of BIND, [_]"),
            "expected one of BIND, [_]"
        );
        let long = format!(
            "expected one of {}, ['A' ..= 'Z' | 'a' ..= 'z'\n| '\\u{{00F8}}'..='\\u{{02FF}}'], [_]",
            (0..30)
                .map(|i| format!("K{i}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let s = short_message(&long);
        assert!(s.starts_with("expected one of K0, K1, "), "{s}");
        assert!(
            s.ends_with(", …") && !s.contains('\n') && s.chars().count() <= 123,
            "{s}"
        );
        // a short first line, then more lines: the last item of the first line is cut
        assert_eq!(
            short_message("expected one of A, ['a'..='z'\n| 'é']"),
            "expected one of A, …"
        );
        assert_eq!(
            short_message(&"x".repeat(200)),
            format!("{}…", "x".repeat(120))
        );
    }

    #[test]
    fn io_errors() {
        let e = std::fs::read("/nonexistent/q.rq").unwrap_err();
        assert_eq!(io(&e), "No such file or directory");
    }

    #[test]
    fn unified_diff() {
        let d = diff("q/a.rq", "a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(
            d,
            "--- a/q/a.rq\n+++ b/q/a.rq\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n"
        );
    }

    #[test]
    fn summaries() {
        assert_eq!(
            check_summary(2, 0).unwrap(),
            "[warn] Code style issues found in 2 files. Run sparkles fmt --write to fix."
        );
        assert!(check_summary(1, 3).unwrap().contains("the above file"));
        assert_eq!(check_summary(0, 1), None);
        assert!(check_summary(0, 0).is_some());
    }
}
