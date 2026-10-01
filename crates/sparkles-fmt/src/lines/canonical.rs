//! The term spelling of canonical N-Triples (RDF 1.2 N-Triples §3, and N-Quads likewise),
//! which is how oxrdf displays terms: one space between terms and inside `<<( … )>>`, a
//! final ` .`, no `^^xsd:string` (oxrdf stores such literals as simple ones), language
//! tags lowercase (oxttl lowercases them), IRIs as their characters, and in strings only
//! `"`, `\`, line feed and carriage return as `\"`, `\\`, `\n`, `\r`, backspace, tab and
//! form feed as `\b`, `\t`, `\f`, and the other control characters, U+007F, U+FFFE and
//! U+FFFF as `\uXXXX` with uppercase hex digits. The W3C canonical form tests pin it
//! (`tests/w3c_lines.rs`), so a change in oxrdf's spelling cannot pass unnoticed.

use oxrdf::Quad;
use std::fmt::Write;

/// Append `quad`'s canonical line, without a line break: `s p o .` or `s p o g .`.
pub fn write_quad(out: &mut String, quad: &Quad) {
    let _ = write!(out, "{quad} .");
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::{BlankNode, GraphName, Literal, NamedNode, Triple};

    fn line(quad: &Quad) -> String {
        let mut s = String::new();
        write_quad(&mut s, quad);
        s
    }

    #[test]
    fn canonical_terms() {
        let n = |s: &str| NamedNode::new_unchecked(s);
        let lit = Literal::new_typed_literal(
            "a\"\\\n\r\u{8}\t\u{c}\u{0}\u{1f}\u{7f}é\u{fffe}",
            n("http://www.w3.org/2001/XMLSchema#string"),
        );
        let q = Quad::new(n("http://s"), n("http://p"), lit, GraphName::DefaultGraph);
        assert_eq!(
            line(&q),
            "<http://s> <http://p> \"a\\\"\\\\\\n\\r\\b\\t\\f\\u0000\\u001F\\u007Fé\\uFFFE\" ."
        );
        let tt = Triple::new(
            BlankNode::new_unchecked("x"),
            n("http://p"),
            Literal::new_language_tagged_literal_unchecked("v", "en-us"),
        );
        let q = Quad::new(n("http://s"), n("http://p"), tt, n("http://g"));
        assert_eq!(
            line(&q),
            "<http://s> <http://p> <<( _:x <http://p> \"v\"@en-us )>> <http://g> ."
        );
        let typed = Literal::new_typed_literal("1", n("http://www.w3.org/2001/XMLSchema#integer"));
        let q = Quad::new(
            BlankNode::new_unchecked("b"),
            n("http://p"),
            typed,
            BlankNode::new_unchecked("g"),
        );
        assert_eq!(
            line(&q),
            "_:b <http://p> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer> _:g ."
        );
    }
}
