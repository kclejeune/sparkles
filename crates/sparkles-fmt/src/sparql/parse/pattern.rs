//! Group graph patterns and their elements: triples statements, `OPTIONAL`, `MINUS`,
//! `UNION`, `GRAPH`, `SERVICE`, `FILTER`, `BIND`, `VALUES` and subqueries.
//!
//! The shapes: a `GroupGraphPattern` holds `{`, then either a `SubSelect` or its
//! elements, then `}`. A nested group, alone or in a `UNION` chain, is a `Union` of
//! `UnionBranch`es (one branch when there is no `UNION`); every branch but the first
//! starts with its `UNION`. The optional `.` after an
//! element that is not a triples statement is a token of the group itself (a triples
//! statement holds its own `.`).

use super::term::{self, TripleTermCtx};
use super::triples::{self, Mode};
use super::{Parser, expr, query};
use crate::lex::TokenKind;
use crate::sparql::keywords::Kw;
use crate::syntax::NodeKind;

/// `GroupGraphPattern ::= '{' ( SubSelect | GroupGraphPatternSub ) '}'`.
pub fn group_graph_pattern(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::GroupGraphPattern);
    if p.expect(TokenKind::LBrace) {
        p.close_label_region();
        if p.at_kw(Kw::Select) {
            query::sub_select(p);
        } else {
            group_graph_pattern_sub(p);
        }
        p.close_label_region();
        p.expect(TokenKind::RBrace);
    }
    m.complete(p);
}

/// `GroupGraphPatternSub ::= TriplesBlock? ( GraphPatternNotTriples '.'? TriplesBlock? )*`:
/// triples statements need a `.` between them; other elements may have one after them.
fn group_graph_pattern_sub(p: &mut Parser<'_>) {
    while !p.at(TokenKind::RBrace) && !p.has_error() {
        if at_pattern_not_triples(p) {
            graph_pattern_not_triples(p);
            p.eat(TokenKind::Dot);
        } else {
            let dot = triples::triples_stmt(p, Mode::Path);
            if !dot && !p.at(TokenKind::RBrace) && !at_pattern_not_triples(p) {
                p.error("expected . or }");
            }
        }
    }
}

/// Whether the current token starts a `GraphPatternNotTriples`.
fn at_pattern_not_triples(p: &Parser<'_>) -> bool {
    p.at(TokenKind::LBrace)
        || matches!(
            p.current_kw(),
            Some(
                Kw::Optional
                    | Kw::Minus
                    | Kw::Graph
                    | Kw::Service
                    | Kw::Filter
                    | Kw::Bind
                    | Kw::Values
            )
        )
}

/// `GraphPatternNotTriples`.
fn graph_pattern_not_triples(p: &mut Parser<'_>) {
    if p.at(TokenKind::LBrace) {
        let m = p.start(NodeKind::Union);
        let b = p.start(NodeKind::UnionBranch);
        group_graph_pattern(p);
        b.complete(p);
        // `UNION` starts the next branch, so a comment before it leads that branch
        while p.at_kw(Kw::Union) {
            let b = p.start(NodeKind::UnionBranch);
            p.bump_as(TokenKind::Kw(Kw::Union));
            group_graph_pattern(p);
            b.complete(p);
        }
        m.complete(p);
        return;
    }
    let Some(kw) = p.current_kw() else {
        p.error("expected a graph pattern");
        return;
    };
    let kind = match kw {
        Kw::Optional => NodeKind::Optional,
        Kw::Minus => NodeKind::Minus,
        Kw::Graph => NodeKind::GraphPattern,
        Kw::Service => NodeKind::Service,
        Kw::Filter => NodeKind::Filter,
        Kw::Bind => NodeKind::Bind,
        _ => NodeKind::InlineValues,
    };
    let m = p.start(kind);
    p.bump_as(TokenKind::Kw(kw));
    match kw {
        Kw::Optional | Kw::Minus => group_graph_pattern(p),
        Kw::Graph => {
            term::var_or_iri(p);
            group_graph_pattern(p);
        }
        Kw::Service => {
            p.eat_kw(Kw::Silent);
            term::var_or_iri(p);
            group_graph_pattern(p);
        }
        Kw::Filter => constraint(p),
        Kw::Bind => {
            p.expect(TokenKind::LParen);
            expr::expression(p);
            p.expect_kw(Kw::As);
            term::var(p);
            p.expect(TokenKind::RParen);
        }
        _ => data_block(p),
    }
    m.complete(p);
}

/// Whether the current token starts a `Constraint` (a bracketed expression, a built-in
/// call or a function call).
pub fn at_constraint(p: &Parser<'_>) -> bool {
    at_exists(p)
        || p.at(TokenKind::LParen)
        || p.current_kw().is_some_and(Kw::is_builtin)
        || (term::at_iri(p) && matches!(p.nth(1), TokenKind::LParen | TokenKind::Nil))
}

/// `Constraint ::= BrackettedExpression | BuiltInCall | FunctionCall`, or fail.
pub fn constraint(p: &mut Parser<'_>) {
    if at_exists(p) {
        expr::expression(p);
    } else if at_constraint(p) {
        expr::constraint(p);
    } else {
        p.error("expected a constraint");
    }
}

/// `BuiltInCall | FunctionCall` (a `GroupCondition`), or fail.
pub fn call(p: &mut Parser<'_>) {
    if at_exists(p) {
        expr::expression(p);
    } else if at_constraint(p) && !p.at(TokenKind::LParen) {
        expr::builtin_or_call(p);
    } else {
        p.error("expected a function call");
    }
}

/// Whether the current tokens start `EXISTS {` or `NOT EXISTS {`. These built-in calls
/// take a group, not arguments, and go through [`expr::expression`], which knows them.
fn at_exists(p: &Parser<'_>) -> bool {
    p.at_kw(Kw::Exists) || (p.at_kw(Kw::Not) && p.nth_at_kw(1, Kw::Exists))
}

/// `DataBlock ::= InlineDataOneVar | InlineDataFull`, after `VALUES`: the variables,
/// then `{`, then a `DataValue` per value (one variable) or a `ValuesRow` per row.
pub fn data_block(p: &mut Parser<'_>) {
    if term::at_var(p) {
        p.bump();
        p.expect(TokenKind::LBrace);
        while !p.at(TokenKind::RBrace) && !p.has_error() {
            data_value(p);
        }
        p.expect(TokenKind::RBrace);
        return;
    }
    let mut vars: Vec<&str> = Vec::new();
    if !p.eat(TokenKind::Nil) {
        p.expect(TokenKind::LParen);
        while term::at_var(p) {
            // `?x` and `$x` are the same variable
            let name = &p.nth_text(0)[1..];
            if vars.contains(&name) {
                p.error("a variable is repeated in VALUES");
            }
            vars.push(name);
            p.bump();
        }
        p.expect(TokenKind::RParen);
    }
    p.expect(TokenKind::LBrace);
    while !p.at(TokenKind::RBrace) && !p.has_error() {
        let m = p.start(NodeKind::ValuesRow);
        let mut values = 0usize;
        if !p.eat(TokenKind::Nil) {
            p.expect(TokenKind::LParen);
            while !p.at(TokenKind::RParen) && !p.has_error() {
                data_value(p);
                values += 1;
            }
            p.expect(TokenKind::RParen);
        }
        if values != vars.len() {
            p.error("a row of VALUES needs one value per variable");
        }
        m.complete(p);
    }
    p.expect(TokenKind::RBrace);
}

/// `DataBlockValue ::= iri | RDFLiteral | NumericLiteral | BooleanLiteral | 'UNDEF' |
/// TripleTermData`: a `DataValue` node.
fn data_value(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::DataValue);
    if p.at(TokenKind::LtLtParen) {
        term::triple_term(p, TripleTermCtx::Data);
    } else if !p.eat_kw(Kw::Undef) {
        if term::at_iri(p) || term::at_literal(p) {
            term::iri_or_literal(p);
        } else {
            p.error("expected a value");
        }
    }
    m.complete(p);
}
