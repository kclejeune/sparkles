//! A CONSTRUCT template: each solution row gives the template's triples (spec F13 §4.1).
//! It is a projection to triples, followed by a distinct in the circuit.
//!
//! A template's blank node becomes a node per solution. It is minted from a hash of the
//! template's label and the solution's terms (spec F13 §4.6), so a solution that
//! disappears and returns gives the same node, and a diff never churns it.

use super::super::pass::Pass;
use super::super::zset::{Changes, Row};
use super::Node;
use crate::error::Result;
use crate::id::{Id, Tag};
use crate::store::TermKind;
use oxrdf::{BlankNode, Term};
use sha2::{Digest, Sha256};

/// A position of a template triple.
#[derive(Clone, Debug)]
pub enum TSlot {
    /// this input column (`None`: never bound)
    Var(Option<usize>),
    Const(Term),
    Blank(String),
}

#[derive(Clone, Debug)]
pub struct Template {
    pub inner: Node,
    pub triples: Vec<[TSlot; 3]>,
    /// what blank node hashes start from, so that two views mint different nodes
    pub seed: String,
}

impl Template {
    fn kind(p: &Pass, i: Id) -> TermKind {
        match i.tag() {
            Tag::Local => match p.terms.term(&p.new, i) {
                Some(Term::NamedNode(_)) => TermKind::Iri,
                Some(Term::BlankNode(_)) => TermKind::BNode,
                Some(Term::Literal(_)) => TermKind::Literal,
                Some(Term::Triple(_)) => TermKind::Triple,
                None => TermKind::Other,
            },
            _ => p.new.term_kind(i),
        }
    }

    fn apply(&self, p: &mut Pass, c: Changes) -> Result<Changes> {
        let mut out = Changes::new();
        let blanks = self
            .triples
            .iter()
            .any(|t| t.iter().any(|s| matches!(s, TSlot::Blank(_))));
        for (row, w) in c {
            p.tick()?;
            // the solution's terms, which blank nodes hash
            let keys: Vec<Vec<u8>> = if blanks {
                row.iter()
                    .map(|&i| p.terms.key(&p.new, i).unwrap_or_default())
                    .collect()
            } else {
                Vec::new()
            };
            let mut minted: Vec<(String, Id)> = Vec::new();
            for t in &self.triples {
                let mut ids = [Id::UNDEF; 3];
                let mut ok = true;
                for (k, s) in t.iter().enumerate() {
                    ids[k] = match s {
                        TSlot::Var(c) => c.map_or(Id::UNDEF, |c| row[c]),
                        TSlot::Const(t) => p.intern(t),
                        TSlot::Blank(label) => match minted.iter().find(|m| &m.0 == label) {
                            Some(m) => m.1,
                            None => {
                                let mut h = Sha256::new();
                                h.update(self.seed.as_bytes());
                                h.update([0]);
                                h.update(label.as_bytes());
                                for k in &keys {
                                    h.update((k.len() as u64).to_le_bytes());
                                    h.update(k);
                                }
                                let d = h.finalize();
                                let hex: String =
                                    d[..16].iter().map(|b| format!("{b:02x}")).collect();
                                let b =
                                    Term::BlankNode(BlankNode::new_unchecked(format!("v{hex}")));
                                let i = p.intern(&b);
                                minted.push((label.clone(), i));
                                i
                            }
                        },
                    };
                    if ids[k].is_undef() {
                        ok = false;
                    }
                }
                if !ok {
                    continue;
                }
                let s_ok = matches!(Self::kind(p, ids[0]), TermKind::Iri | TermKind::BNode);
                let p_ok = Self::kind(p, ids[1]) == TermKind::Iri;
                if s_ok && p_ok {
                    let r: Row = ids.to_vec().into();
                    out.push((r, w));
                }
            }
        }
        Ok(out)
    }

    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        let c = self.inner.init(p)?;
        self.apply(p, c)
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        let c = self.inner.delta(p)?;
        self.apply(p, c)
    }
}
