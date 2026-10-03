//! Queries: `SELECT`, `CONSTRUCT`, `DESCRIBE` and `ASK` forms, their clauses and
//! solution modifiers, subqueries and the trailing `VALUES`.
//!
//! The shapes: a `QueryUnit` holds its `Prologue` (if any) and one query node. A query
//! node holds its keyword (or `SelectClause`, `DescribeClause`), its `ConstructTemplate`,
//! `DatasetClause`s, `WhereClause` (`WHERE` and a `GroupGraphPattern`), the solution
//! modifiers and the `ValuesClause`, in source order. The short form `CONSTRUCT WHERE {
//! … }` has a `WhereClause` whose group holds the template's triples.

use super::pattern::{self, group_graph_pattern};
use super::triples::{self, Mode};
use super::{Parser, expr, prologue, term};
use crate::lex::TokenKind;
use crate::sparql::keywords::Kw;
use crate::syntax::NodeKind;

/// `QueryUnit ::= Query`, the root of a query.
pub fn query_unit(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::QueryUnit);
    prologue::prologue(p);
    match p.current_kw() {
        Some(Kw::Select) => select_query(p),
        Some(Kw::Construct) => construct_query(p),
        Some(Kw::Describe) => describe_query(p),
        Some(Kw::Ask) => ask_query(p),
        _ => p.error("expected SELECT, CONSTRUCT, DESCRIBE or ASK"),
    }
    m.complete(p);
}

/// `SelectQuery ::= SelectClause DatasetClause* WhereClause SolutionModifier ValuesClause`
fn select_query(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::SelectQuery);
    select_clause(p);
    dataset_clauses(p);
    where_clause(p);
    solution_modifier(p);
    values_clause(p);
    m.complete(p);
}

/// `SubSelect ::= SelectClause WhereClause SolutionModifier ValuesClause`, inside a
/// group's braces.
pub fn sub_select(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::SubSelect);
    select_clause(p);
    where_clause(p);
    solution_modifier(p);
    values_clause(p);
    m.complete(p);
}

/// `SelectClause ::= 'SELECT' ( 'DISTINCT' | 'REDUCED' )? ( ( Var | ( '(' Expression 'AS'
/// Var ')' ) )+ | '*' )`, each projected item a `ProjectionItem`.
fn select_clause(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::SelectClause);
    p.expect_kw(Kw::Select);
    let _ = p.eat_kw(Kw::Distinct) || p.eat_kw(Kw::Reduced);
    if !p.eat(TokenKind::Star) {
        if !(term::at_var(p) || p.at(TokenKind::LParen)) {
            p.error("expected a variable, (expression AS ?var) or *");
        }
        while term::at_var(p) || p.at(TokenKind::LParen) {
            let item = p.start(NodeKind::ProjectionItem);
            if !p.eat(TokenKind::Var1) && !p.eat(TokenKind::Var2) {
                p.bump();
                expr::expression(p);
                p.expect_kw(Kw::As);
                term::var(p);
                p.expect(TokenKind::RParen);
            }
            item.complete(p);
        }
    }
    m.complete(p);
}

/// `ConstructQuery ::= 'CONSTRUCT' ( ConstructTemplate DatasetClause* WhereClause
/// SolutionModifier | DatasetClause* 'WHERE' '{' TriplesTemplate? '}' SolutionModifier )
/// ValuesClause`
fn construct_query(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::ConstructQuery);
    p.expect_kw(Kw::Construct);
    if p.at(TokenKind::LBrace) {
        construct_template(p);
        p.forget_label_region();
        dataset_clauses(p);
        where_clause(p);
    } else {
        dataset_clauses(p);
        let w = p.start(NodeKind::WhereClause);
        p.expect_kw(Kw::Where);
        let g = p.start(NodeKind::GroupGraphPattern);
        p.expect(TokenKind::LBrace);
        construct_quads(p);
        p.expect(TokenKind::RBrace);
        g.complete(p);
        w.complete(p);
    }
    solution_modifier(p);
    values_clause(p);
    m.complete(p);
}

/// `ConstructTemplate ::= '{' ConstructTriples? '}'`, or Jena ARQ's TriG-like template
/// (`ConstructQuads`). A lone `.` is accepted, as the reference parser does.
fn construct_template(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::ConstructTemplate);
    p.expect(TokenKind::LBrace);
    if !p.eat(TokenKind::Dot) {
        construct_quads(p);
    }
    p.expect(TokenKind::RBrace);
    m.complete(p);
}

/// ARQ's `ConstructQuads`: triples statements, `GRAPH g { … }` blocks (`g` an IRI, a
/// variable or a blank node) and bare `{ … }` blocks for the default graph, up to a
/// `}`. Each block is a `QuadsGraph`, the `.` after it a token of the template.
fn construct_quads(p: &mut Parser<'_>) {
    while !p.at(TokenKind::RBrace) && !p.has_error() {
        if p.at_kw(Kw::Graph) || p.at(TokenKind::LBrace) {
            let g = p.start(NodeKind::QuadsGraph);
            if p.at_kw(Kw::Graph) {
                p.bump_as(TokenKind::Kw(Kw::Graph));
                if term::at_blank_node(p) {
                    p.bump();
                } else {
                    term::var_or_iri(p);
                }
            }
            p.expect(TokenKind::LBrace);
            triples_template(p);
            p.expect(TokenKind::RBrace);
            g.complete(p);
            p.eat(TokenKind::Dot);
        } else if !triples::triples_stmt(p, Mode::Template)
            && !p.at(TokenKind::RBrace)
            && !p.at_kw(Kw::Graph)
            && !p.at(TokenKind::LBrace)
        {
            p.error("expected . or }");
        }
    }
}

/// `TriplesTemplate`: statements separated by `.`, up to a `}`.
pub fn triples_template(p: &mut Parser<'_>) {
    while !p.at(TokenKind::RBrace) && !p.has_error() {
        if !triples::triples_stmt(p, Mode::Template) && !p.at(TokenKind::RBrace) {
            p.error("expected . or }");
        }
    }
}

/// `DescribeQuery ::= 'DESCRIBE' ( VarOrIri+ | '*' ) DatasetClause* WhereClause?
/// SolutionModifier ValuesClause`
fn describe_query(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::DescribeQuery);
    let c = p.start(NodeKind::DescribeClause);
    p.expect_kw(Kw::Describe);
    if !p.eat(TokenKind::Star) {
        term::var_or_iri(p);
        while term::at_var_or_iri(p) {
            term::var_or_iri(p);
        }
    }
    c.complete(p);
    dataset_clauses(p);
    if p.at_kw(Kw::Where) || p.at(TokenKind::LBrace) {
        where_clause(p);
    }
    solution_modifier(p);
    values_clause(p);
    m.complete(p);
}

/// `AskQuery ::= 'ASK' DatasetClause* WhereClause SolutionModifier ValuesClause`
fn ask_query(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::AskQuery);
    p.expect_kw(Kw::Ask);
    dataset_clauses(p);
    where_clause(p);
    solution_modifier(p);
    values_clause(p);
    m.complete(p);
}

/// `DatasetClause ::= 'FROM' 'NAMED'? iri`, any number.
fn dataset_clauses(p: &mut Parser<'_>) {
    while p.at_kw(Kw::From) {
        let m = p.start(NodeKind::DatasetClause);
        p.bump_as(TokenKind::Kw(Kw::From));
        p.eat_kw(Kw::Named);
        term::iri(p);
        m.complete(p);
    }
}

/// `WhereClause ::= 'WHERE'? GroupGraphPattern`
pub fn where_clause(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::WhereClause);
    p.eat_kw(Kw::Where);
    group_graph_pattern(p);
    m.complete(p);
}

/// `SolutionModifier ::= GroupClause? HavingClause? OrderClause? LimitOffsetClauses?`
fn solution_modifier(p: &mut Parser<'_>) {
    if p.at_kw(Kw::Group) {
        let m = p.start(NodeKind::GroupBy);
        p.bump_as(TokenKind::Kw(Kw::Group));
        p.expect_kw(Kw::By);
        loop {
            group_condition(p);
            if !at_group_condition(p) || p.has_error() {
                break;
            }
        }
        m.complete(p);
    }
    if p.at_kw(Kw::Having) {
        let m = p.start(NodeKind::Having);
        p.bump_as(TokenKind::Kw(Kw::Having));
        loop {
            pattern::constraint(p);
            if !pattern::at_constraint(p) || p.has_error() {
                break;
            }
        }
        m.complete(p);
    }
    if p.at_kw(Kw::Order) {
        let m = p.start(NodeKind::OrderBy);
        p.bump_as(TokenKind::Kw(Kw::Order));
        p.expect_kw(Kw::By);
        loop {
            order_condition(p);
            if !at_order_condition(p) || p.has_error() {
                break;
            }
        }
        m.complete(p);
    }
    // LIMIT and OFFSET, in either order
    if p.at_kw(Kw::Limit) {
        limit_or_offset(p, Kw::Limit, NodeKind::Limit);
        if p.at_kw(Kw::Offset) {
            limit_or_offset(p, Kw::Offset, NodeKind::Offset);
        }
    } else if p.at_kw(Kw::Offset) {
        limit_or_offset(p, Kw::Offset, NodeKind::Offset);
        if p.at_kw(Kw::Limit) {
            limit_or_offset(p, Kw::Limit, NodeKind::Limit);
        }
    }
}

fn at_group_condition(p: &Parser<'_>) -> bool {
    term::at_var(p) || pattern::at_constraint(p)
}

/// `GroupCondition ::= BuiltInCall | FunctionCall | '(' Expression ( 'AS' Var )? ')' |
/// Var`
fn group_condition(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::GroupCondition);
    if term::at_var(p) {
        p.bump();
    } else if p.at(TokenKind::LParen) {
        p.bump();
        expr::expression(p);
        if p.eat_kw(Kw::As) {
            term::var(p);
        }
        p.expect(TokenKind::RParen);
    } else {
        pattern::call(p);
    }
    m.complete(p);
}

fn at_order_condition(p: &Parser<'_>) -> bool {
    term::at_var(p) || p.at_kw(Kw::Asc) || p.at_kw(Kw::Desc) || pattern::at_constraint(p)
}

/// `OrderCondition ::= ( ( 'ASC' | 'DESC' ) BrackettedExpression ) | ( Constraint | Var )`
pub(super) fn order_condition(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::OrderCondition);
    if p.eat_kw(Kw::Asc) || p.eat_kw(Kw::Desc) {
        expr::bracketted(p);
    } else if term::at_var(p) {
        p.bump();
    } else {
        pattern::constraint(p);
    }
    m.complete(p);
}

/// `LimitClause ::= 'LIMIT' INTEGER`, `OffsetClause ::= 'OFFSET' INTEGER`
fn limit_or_offset(p: &mut Parser<'_>, kw: Kw, kind: NodeKind) {
    let m = p.start(kind);
    p.bump_as(TokenKind::Kw(kw));
    p.expect(TokenKind::Integer);
    m.complete(p);
}

/// `ValuesClause ::= ( 'VALUES' DataBlock )?`
fn values_clause(p: &mut Parser<'_>) {
    if p.at_kw(Kw::Values) {
        let m = p.start(NodeKind::ValuesClause);
        p.bump_as(TokenKind::Kw(Kw::Values));
        pattern::data_block(p);
        m.complete(p);
    }
}
