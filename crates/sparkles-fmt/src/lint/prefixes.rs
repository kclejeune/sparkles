//! `unused-prefix` and `undefined-prefix`, over SPARQL and Turtle alike: both trees hold
//! `PrefixDecl` nodes and `PnameNs`/`PnameLn` tokens. A declaration is in scope from its
//! IRI on, until a later declaration of its label (Turtle, TriG) or to the end of the
//! request (SPARQL, whose update operations keep the prologues before them).

use super::{Cst, Edit, Fix, Out};
use crate::Options;
use crate::lex::TokenKind;
use crate::syntax::NodeKind;
use crate::tree::TokenId;
use std::collections::HashSet;

pub(super) fn run(c: &Cst<'_, '_>, out: &mut Out<'_>) {
    let decls: Vec<_> = c
        .nodes()
        .into_iter()
        .filter(|&n| c.kind(n) == NodeKind::PrefixDecl)
        .collect();
    let labels: HashSet<TokenId> = decls
        .iter()
        .flat_map(|&n| c.own_tokens(n))
        .filter(|&t| c.token_kind(t) == TokenKind::PnameNs)
        .collect();

    if out.on("unused-prefix") {
        // uses as written: every node verbatim, nothing compacted or dropped
        let opts = Options {
            compact_iris: false,
            type_shorthand: false,
            ..Options::default()
        };
        let unused =
            crate::normalize::prune::unused_declarations_with(c.tree, &c.scope, &opts, |_| true);
        for &n in &decls {
            if !unused.contains(&n) {
                continue;
            }
            let label = c
                .own_tokens(n)
                .into_iter()
                .find(|&t| c.token_kind(t) == TokenKind::PnameNs)
                .map_or("", |t| c.text(t));
            let (start, end) = c.node_span(n);
            out.report_fix(
                "unused-prefix",
                start,
                end,
                format!("the prefix {label} is declared but never used"),
                Some(Fix {
                    title: format!("Remove the unused prefix {label}"),
                    edits: vec![removal(out.text, start, end)],
                }),
            );
        }
    }

    if out.on("undefined-prefix") {
        for i in 0..c.tree.tokens.len() {
            let t = TokenId(i as u32);
            if !matches!(c.token_kind(t), TokenKind::PnameNs | TokenKind::PnameLn)
                || labels.contains(&t)
            {
                continue;
            }
            let text = c.text(t);
            let label = &text[..text.find(':').unwrap_or(text.len())];
            if c.scope.resolve(label, t).is_none() {
                let (start, end) = c.span(t);
                out.report(
                    "undefined-prefix",
                    start,
                    end,
                    format!("the prefix {label}: is not declared before this use"),
                );
            }
        }
    }
}

/// The edit that removes bytes `start..end`: the whole line when nothing else is on it,
/// else the span and the spaces after it.
fn removal(text: &str, start: usize, end: usize) -> Edit {
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let before_blank = text[line_start..start].trim().is_empty();
    let rest = &text[end..];
    let line_end = rest.find('\n').map_or(text.len(), |i| end + i + 1);
    let after_blank = text[end..line_end].trim().is_empty();
    if before_blank && after_blank {
        return Edit {
            start: line_start,
            end: line_end,
            insert: String::new(),
        };
    }
    let spaces = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    Edit {
        start,
        end: end + spaces,
        insert: String::new(),
    }
}
