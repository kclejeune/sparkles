//! Pragmas: `# sparkles-fmt: ignore` as the last line of a node's leading comments keeps
//! the node as written; `# sparkles-fmt: ignore-file` in the file header keeps the whole
//! file as written.

use crate::lex::{Token, TokenKind};

pub const IGNORE: &str = "sparkles-fmt: ignore";
pub const IGNORE_FILE: &str = "sparkles-fmt: ignore-file";

/// The pragma a comment's text (`# …`) spells, if any.
fn pragma(comment: &str) -> Option<&str> {
    let body = comment.strip_prefix('#')?.trim();
    (body == IGNORE || body == IGNORE_FILE).then_some(body)
}

/// Whether a comment is `# sparkles-fmt: ignore`.
pub fn is_ignore(comment: &str) -> bool {
    pragma(comment) == Some(IGNORE)
}

/// Whether a comment is `# sparkles-fmt: ignore-file`.
pub fn is_ignore_file(comment: &str) -> bool {
    pragma(comment) == Some(IGNORE_FILE)
}

/// Whether the file header (the comments before the first significant token) holds
/// `# sparkles-fmt: ignore-file`.
pub fn ignore_file(src: &str, tokens: &[Token]) -> bool {
    tokens
        .iter()
        .take_while(|t| t.kind.is_trivia())
        .any(|t| t.kind == TokenKind::Comment && is_ignore_file(t.text(src)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::{LexMode, lex};

    #[test]
    fn pragmas() {
        assert!(is_ignore("# sparkles-fmt: ignore"));
        assert!(is_ignore("#sparkles-fmt: ignore  "));
        assert!(!is_ignore("# sparkles-fmt: ignore-file"));
        assert!(!is_ignore("# sparkles-fmt: ignore this"));
        assert!(is_ignore_file("# sparkles-fmt: ignore-file"));
        let header = |s: &str| ignore_file(s, &lex(s, LexMode::Sparql));
        assert!(header("# x\n\n# sparkles-fmt: ignore-file\nSELECT"));
        assert!(!header("SELECT # sparkles-fmt: ignore-file\n"));
        assert!(!header(""));
    }
}
