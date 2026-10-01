//! Cursor mapping (Prettier's `cursorOffset`): a byte offset in the input to one in the
//! output, through the output position of every printed source token.

use crate::doc::Printed;
use crate::lex::TokenKind;
use crate::tree::Tree;

/// Map input byte `byte` into `printed`:
/// - inside a printed token (a comment, or whitespace a verbatim range copied), at
///   distance `d` from its start: the token's output start plus `min(d, printed
///   length)`;
/// - in trivia between two tokens: the end of the first one, or the start of the second
///   when the trivia holds a line break;
/// - before the first token: 0; after the last: the end.
///
/// A token the printer dropped maps to the next printed token.
pub fn map(tree: &Tree<'_>, printed: &Printed, byte: usize) -> usize {
    let end = printed.text.len();
    let tokens = &tree.tokens;
    let mut out: Vec<Option<(usize, usize)>> = vec![None; tokens.len()];
    for &(id, start, len) in &printed.tok_out {
        if let Some(slot) = out.get_mut(id.0 as usize) {
            *slot = Some((start as usize, len as usize));
        }
    }
    // the token holding `byte` (a token ends where the next starts)
    let i = tokens.partition_point(|t| t.end() <= byte);
    let Some(tok) = tokens.get(i) else {
        return end;
    };
    if let Some((start, len)) = out[i] {
        return start + byte.saturating_sub(tok.start as usize).min(len);
    }
    let next_start = |from: usize| out[from..].iter().flatten().next().map_or(end, |&(s, _)| s);
    if tok.kind == TokenKind::Whitespace {
        let prev_end = out[..i].iter().rev().flatten().next().map(|&(s, l)| s + l);
        return match prev_end {
            None => 0,
            Some(e) if !tok.text(tree.src).contains(['\n', '\r']) => e,
            Some(_) => next_start(i + 1),
        };
    }
    next_start(i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{DocArena, print};
    use crate::lex::{LexMode, lex};
    use crate::tree::{NodeData, TokenId};

    fn tree(src: &str) -> Tree<'_> {
        Tree {
            src,
            tokens: lex(src, LexMode::Sparql),
            nodes: vec![NodeData {
                kind: crate::syntax::NodeKind::QueryUnit,
                parent: None,
                children: Vec::new(),
                range: 0..src.len() as u32,
            }],
        }
    }

    /// The first token (trivia included) of `t` whose text is `text`.
    fn tok(t: &Tree<'_>, text: &str) -> TokenId {
        let i = t.tokens.iter().position(|k| k.text(t.src) == text).unwrap();
        TokenId(i as u32)
    }

    #[test]
    fn identity_maps_to_itself() {
        let src = "\u{feff}SELECT  * {\n  ?s ?p ?o } # c";
        let t = tree(src);
        let mut a = DocArena::new(&t.tokens);
        let v = a.verbatim(3..src.len());
        let p = print(&a, v, src, 100, 2, None).unwrap();
        // whitespace a verbatim range copied maps exactly too
        for b in 3..=src.len() {
            assert_eq!(map(&t, &p, b), b - 3, "{b}");
        }
        // inside the BOM: the start
        assert_eq!(map(&t, &p, 1), 0);
    }

    #[test]
    fn inside_tokens_and_in_trivia() {
        // "SELECT   *\n\n  {}" printed as "SELECT *\n{}"
        let src = "SELECT   *\n\n  {}";
        let t = tree(src);
        let mut a = DocArena::new(&t.tokens);
        let sel = a.token(tok(&t, "SELECT"), None);
        let sp = a.text(" ");
        let star = a.token(tok(&t, "*"), None);
        let hl = a.hard_line();
        let open = a.token(tok(&t, "{"), None);
        let close = a.token(tok(&t, "}"), None);
        let all = a.concat([sel, sp, star, hl, open, close]);
        let p = print(&a, all, src, 100, 2, None).unwrap();
        assert_eq!(p.text, "SELECT *\n{}");
        // inside a token
        assert_eq!(map(&t, &p, 0), 0);
        assert_eq!(map(&t, &p, 3), 3);
        // trivia on one line: the end of the token before
        assert_eq!(map(&t, &p, 6), 6);
        assert_eq!(map(&t, &p, 8), 6);
        assert_eq!(map(&t, &p, 9), 7);
        // trivia across lines: the start of the token after
        assert_eq!(map(&t, &p, 10), 9);
        assert_eq!(map(&t, &p, 12), 9);
        assert_eq!(map(&t, &p, 14), 9);
        assert_eq!(map(&t, &p, 15), 10);
        // after the last token: the end
        assert_eq!(map(&t, &p, 16), 11);
    }

    #[test]
    fn before_the_first_token_and_after_the_last() {
        let src = "  \n ?x  ";
        let t = tree(src);
        let mut a = DocArena::new(&t.tokens);
        let x = a.token(tok(&t, "?x"), None);
        let p = print(&a, x, src, 100, 2, None).unwrap();
        assert_eq!(map(&t, &p, 0), 0);
        assert_eq!(map(&t, &p, 2), 0);
        assert_eq!(map(&t, &p, 5), 1);
        assert_eq!(map(&t, &p, 7), 2);
        assert_eq!(map(&t, &p, src.len()), 2);
    }

    #[test]
    fn normalized_and_dropped_tokens() {
        // `rdf:type` printed `a`, the `;` dropped, a comment moved before the subject
        let src = "?s rdf:type ?o ; # c\n";
        let t = tree(src);
        let mut a = DocArena::new(&t.tokens);
        let c = a.token(tok(&t, "# c"), None);
        let hl = a.hard_line();
        let s = a.token(tok(&t, "?s"), None);
        let sp = a.text(" ");
        let ty = a.token(tok(&t, "rdf:type"), Some("a".into()));
        let o = a.token(tok(&t, "?o"), None);
        let all = a.concat([c, hl, s, sp, ty, sp, o]);
        let p = print(&a, all, src, 100, 2, None).unwrap();
        assert_eq!(p.text, "# c\n?s a ?o");
        // inside `rdf:type`: never past the printed `a`
        assert_eq!(map(&t, &p, 3), 7);
        assert_eq!(map(&t, &p, 4), 8);
        assert_eq!(map(&t, &p, 9), 8);
        // the dropped `;` maps to the next printed token: the comment
        assert_eq!(map(&t, &p, 15), 0);
        // inside the comment, wherever it went
        assert_eq!(map(&t, &p, 18), 1);
        // trivia after a line break, before nothing: the end
        assert_eq!(map(&t, &p, src.len() - 1), p.text.len());
    }
}
