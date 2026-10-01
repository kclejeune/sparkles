//! Cursor mapping (Prettier's `cursorOffset`): a byte offset in the input to one in the
//! output, through the output position of every printed source token.

use crate::doc::Printed;
use crate::tree::{TokenId, Tree};

/// Map input byte `byte` into `printed`:
/// - inside a token, at distance `d` from its start: the token's output start plus
///   `min(d, printed length)`;
/// - in trivia between two tokens: the end of the first one, or the start of the second
///   when the trivia holds a newline;
/// - before the first token: 0; after the last: the end.
///
/// A token the printer dropped maps to the next printed token.
pub fn map(tree: &Tree<'_>, printed: &Printed, byte: usize) -> usize {
    let end = printed.text.len();
    let out: std::collections::HashMap<TokenId, (usize, usize)> = printed
        .tok_out
        .iter()
        .map(|&(id, start, len)| (id, (start as usize, len as usize)))
        .collect();
    let tokens = &tree.tokens;
    // the token containing `byte` (a token ends where the next starts)
    let i = tokens.partition_point(|t| t.end() <= byte);
    let Some(tok) = tokens.get(i) else {
        return end;
    };
    let printed_from = |from: usize| {
        (from..tokens.len())
            .find_map(|j| out.get(&TokenId(j as u32)).map(|&(s, _)| s))
            .unwrap_or(end)
    };
    let printed_before = |before: usize| {
        (0..before)
            .rev()
            .find_map(|j| out.get(&TokenId(j as u32)).map(|&(s, l)| s + l))
    };
    if tok.kind == crate::lex::TokenKind::Whitespace {
        return match printed_before(i) {
            None => 0,
            Some(prev_end) if !tok.text(tree.src).contains('\n') => prev_end,
            Some(_) => printed_from(i + 1),
        };
    }
    match out.get(&TokenId(i as u32)) {
        Some(&(start, len)) => start + (byte.saturating_sub(tok.start as usize)).min(len),
        None => printed_from(i + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{DocArena, print};
    use crate::lex::{LexMode, lex};
    use crate::tree::NodeData;

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

    #[test]
    fn identity_maps_to_itself() {
        let src = "SELECT * { ?s ?p ?o } # c";
        let t = tree(src);
        let mut a = DocArena::new(&t.tokens);
        let v = a.verbatim(0..src.len());
        let p = print(&a, v, src, 100, 2, None).unwrap();
        for b in 0..=src.len() {
            assert_eq!(map(&t, &p, b), b, "{b}");
        }
    }

    #[test]
    fn reflowed_tokens() {
        // "SELECT   *" printed as "SELECT *": inside tokens, and in trivia
        let src = "SELECT   *";
        let t = tree(src);
        let mut a = DocArena::new(&t.tokens);
        let sel = a.token(TokenId(0), None);
        let sp = a.text(" ");
        let star = a.token(TokenId(2), None);
        let all = a.concat([sel, sp, star]);
        let p = print(&a, all, src, 100, 2, None).unwrap();
        assert_eq!(p.text, "SELECT *");
        assert_eq!(map(&t, &p, 0), 0);
        assert_eq!(map(&t, &p, 3), 3);
        assert_eq!(map(&t, &p, 7), 6);
        assert_eq!(map(&t, &p, 9), 7);
        assert_eq!(map(&t, &p, 10), 8);
    }
}
