//! Update requests: operations separated by `;`, each with its own prologue.
//!
//! The shapes: an `UpdateUnit` holds `Prologue`s, operation nodes and the `;`s between
//! them. An operation holds its keywords and graph references as tokens, and its
//! `QuadPattern`s, `WithClause`, `DeleteClause`, `InsertClause`, `UsingClause`s and
//! `WhereClause`. A `QuadPattern` holds `{`, `TriplesStmt`s and `QuadsGraph`s (with the
//! optional `.` after a `QuadsGraph` as its own token), then `}`.

use super::query::{triples_template, where_clause};
use super::triples::{self, Mode};
use super::{Ground, Parser, prologue, term};
use crate::lex::TokenKind;
use crate::sparql::keywords::Kw;
use crate::syntax::NodeKind;

/// `UpdateUnit ::= Update`, the root of an update request: `Prologue ( Update1 ( ';'
/// Update )? )?`. A lone `;` after the prologue is accepted, as the reference parser
/// does.
pub fn update_unit(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::UpdateUnit);
    prologue::prologue(p);
    if at_operation(p) {
        loop {
            operation(p);
            if !p.eat(TokenKind::Semicolon) {
                break;
            }
            prologue::prologue(p);
            if !at_operation(p) {
                break;
            }
        }
    } else {
        p.eat(TokenKind::Semicolon);
    }
    m.complete(p);
}

fn at_operation(p: &Parser<'_>) -> bool {
    matches!(
        p.current_kw(),
        Some(
            Kw::Load
                | Kw::Clear
                | Kw::Drop
                | Kw::Create
                | Kw::Add
                | Kw::Move
                | Kw::Copy
                | Kw::Insert
                | Kw::Delete
                | Kw::With
        )
    )
}

/// `Update1 ::= Load | Clear | Drop | Add | Move | Copy | Create | InsertData |
/// DeleteData | DeleteWhere | Modify`
fn operation(p: &mut Parser<'_>) {
    let Some(kw) = p.current_kw() else {
        p.error("expected an update operation");
        return;
    };
    let kind = match kw {
        Kw::Load => NodeKind::LoadOp,
        Kw::Clear => NodeKind::ClearOp,
        Kw::Drop => NodeKind::DropOp,
        Kw::Create => NodeKind::CreateOp,
        Kw::Add => NodeKind::AddOp,
        Kw::Move => NodeKind::MoveOp,
        Kw::Copy => NodeKind::CopyOp,
        Kw::Insert if p.nth_at_kw(1, Kw::Data) => NodeKind::InsertDataOp,
        Kw::Delete if p.nth_at_kw(1, Kw::Data) => NodeKind::DeleteDataOp,
        Kw::Delete if p.nth_at_kw(1, Kw::Where) => NodeKind::DeleteWhereOp,
        _ => {
            modify(p);
            p.end_operation();
            return;
        }
    };
    let m = p.start(kind);
    p.bump_as(TokenKind::Kw(kw));
    match kind {
        NodeKind::LoadOp => {
            p.eat_kw(Kw::Silent);
            term::iri(p);
            if p.eat_kw(Kw::Into) {
                p.expect_kw(Kw::Graph);
                term::iri(p);
            }
        }
        NodeKind::ClearOp | NodeKind::DropOp => {
            p.eat_kw(Kw::Silent);
            if !(p.eat_kw(Kw::Default) || p.eat_kw(Kw::Named) || p.eat_kw(Kw::All)) {
                p.expect_kw(Kw::Graph);
                term::iri(p);
            }
        }
        NodeKind::CreateOp => {
            p.eat_kw(Kw::Silent);
            p.expect_kw(Kw::Graph);
            term::iri(p);
        }
        NodeKind::AddOp | NodeKind::MoveOp | NodeKind::CopyOp => {
            p.eat_kw(Kw::Silent);
            graph_or_default(p);
            p.expect_kw(Kw::To);
            graph_or_default(p);
        }
        NodeKind::InsertDataOp => {
            p.bump_as(TokenKind::Kw(Kw::Data));
            p.start_insert_data();
            ground_quad_pattern(p, true, false);
        }
        NodeKind::DeleteDataOp => {
            p.bump_as(TokenKind::Kw(Kw::Data));
            ground_quad_pattern(p, true, true);
        }
        _ => {
            p.bump_as(TokenKind::Kw(Kw::Where));
            ground_quad_pattern(p, false, true);
        }
    }
    m.complete(p);
    p.end_operation();
}

/// A quad block without variables or blank nodes.
fn ground_quad_pattern(p: &mut Parser<'_>, no_vars: bool, no_blank_nodes: bool) {
    let before = p.set_ground(Ground {
        no_vars,
        no_blank_nodes,
    });
    quad_pattern(p);
    p.set_ground(before);
}

/// `GraphOrDefault ::= 'DEFAULT' | 'GRAPH'? iri`
fn graph_or_default(p: &mut Parser<'_>) {
    if !p.eat_kw(Kw::Default) {
        p.eat_kw(Kw::Graph);
        term::iri(p);
    }
}

/// `Modify ::= ( 'WITH' iri )? ( DeleteClause InsertClause? | InsertClause ) UsingClause*
/// 'WHERE' GroupGraphPattern`
fn modify(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::ModifyOp);
    if p.at_kw(Kw::With) {
        let w = p.start(NodeKind::WithClause);
        p.bump_as(TokenKind::Kw(Kw::With));
        term::iri(p);
        w.complete(p);
    }
    let mut any = false;
    for (kw, kind) in [
        (Kw::Delete, NodeKind::DeleteClause),
        (Kw::Insert, NodeKind::InsertClause),
    ] {
        if p.at_kw(kw) {
            let c = p.start(kind);
            p.bump_as(TokenKind::Kw(kw));
            ground_quad_pattern(p, false, kw == Kw::Delete);
            c.complete(p);
            p.forget_label_region();
            any = true;
        }
    }
    if !any {
        p.error("expected DELETE or INSERT");
    }
    while p.at_kw(Kw::Using) {
        let u = p.start(NodeKind::UsingClause);
        p.bump_as(TokenKind::Kw(Kw::Using));
        p.eat_kw(Kw::Named);
        term::iri(p);
        u.complete(p);
    }
    if !p.at_kw(Kw::Where) {
        p.error("expected WHERE");
    }
    where_clause(p);
    m.complete(p);
}

/// `QuadPattern ::= '{' Quads '}'` (and `QuadData`). Statements need no `.` between
/// them here, as the reference parser reads `Quads`.
fn quad_pattern(p: &mut Parser<'_>) {
    let m = p.start(NodeKind::QuadPattern);
    p.expect(TokenKind::LBrace);
    while !p.at(TokenKind::RBrace) && !p.has_error() {
        if p.at_kw(Kw::Graph) {
            let g = p.start(NodeKind::QuadsGraph);
            p.bump_as(TokenKind::Kw(Kw::Graph));
            term::var_or_iri(p);
            p.expect(TokenKind::LBrace);
            triples_template(p);
            p.expect(TokenKind::RBrace);
            g.complete(p);
            p.eat(TokenKind::Dot);
        } else {
            triples::triples_stmt(p, Mode::Template);
        }
    }
    p.expect(TokenKind::RBrace);
    m.complete(p);
}
