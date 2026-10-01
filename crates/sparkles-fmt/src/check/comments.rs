//! The comment check: every comment of the input is in the output.

use crate::lex::{LexMode, TokenKind, lex};
use crate::{Check, FormatError};

/// The comment texts of `text`, trailing whitespace removed (the printer drops it).
pub fn texts(text: &str, mode: LexMode) -> Vec<&str> {
    lex(text, mode)
        .into_iter()
        .filter(|t| t.kind == TokenKind::Comment)
        .map(|t| t.text(text).trim_end())
        .collect()
}

/// `Ok` when the output has the same comments as the input. They are compared as a
/// multiset: sorting prefix declarations moves their comments with them.
pub fn same(input: &str, output: &str, mode: LexMode) -> Result<(), FormatError> {
    let mut a = texts(input, mode);
    let mut b = texts(output, mode);
    a.sort_unstable();
    b.sort_unstable();
    if a == b {
        Ok(())
    } else {
        Err(FormatError::Unsafe {
            check: Check::Comments,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_comment_texts() {
        let m = LexMode::Sparql;
        assert!(same("# a  \nSELECT # b\n", "# a\nSELECT\n# b", m).is_ok());
        assert!(same("# a\n# b", "# b\n# a", m).is_ok());
        assert_eq!(
            same("# a\n# b", "# a", m),
            Err(FormatError::Unsafe {
                check: Check::Comments
            })
        );
        // a `#` inside a string or an IRI is not a comment
        assert!(
            same(
                "SELECT * { ?s ?p \"# x\" }",
                "SELECT * { ?s ?p \"# x\" }",
                m
            )
            .is_ok()
        );
        assert_eq!(texts("<http://e/#a> \"#b\" # c", m), ["# c"]);
    }
}
