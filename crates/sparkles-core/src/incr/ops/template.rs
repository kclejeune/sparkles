//! A CONSTRUCT template: each solution row gives the template's triples (spec F13 §4.1).
//! It is a projection to triples, followed by a distinct in the circuit.
//!
//! A template's blank node becomes a node per solution. It is minted from a hash of the
//! template's label and the solution's terms (spec F13 §4.6), so a solution that
//! disappears and returns gives the same node, and a diff never churns it. A solution
//! that occurs more than once gives one node per occurrence, as a one-off evaluation
//! does: the template keeps each solution's count and hashes the occurrence's number
//! after the first.

use super::super::pass::Pass;
use super::super::persist::{Reader, Writer};
use super::super::zset::{Changes, Row, ZSet};
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
    /// each solution's count, when the template has blank nodes
    pub counts: ZSet,
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

    fn apply(&mut self, p: &mut Pass, c: Changes) -> Result<Changes> {
        let mut out = Changes::new();
        let blanks = self
            .triples
            .iter()
            .any(|t| t.iter().any(|s| matches!(s, TSlot::Blank(_))));
        for (row, w) in c {
            p.tick()?;
            if !blanks {
                self.emit(p, &row, &[], None, w, &mut out);
                continue;
            }
            // the solution's terms, which blank nodes hash
            let keys: Vec<Vec<u8>> = row
                .iter()
                .map(|&i| p.terms.key(&p.new, i).unwrap_or_default())
                .collect();
            // the occurrences added or removed, the newest removed first
            let before = self.counts.get(&row);
            self.counts.add(row.clone(), w);
            let (range, sign) = if w > 0 {
                (before..before + w, 1)
            } else {
                (before + w..before, -1)
            };
            for k in range {
                p.tick()?;
                let k = (k > 0).then_some(k);
                self.emit(p, &row, &keys, k, sign, &mut out);
            }
        }
        Ok(out)
    }

    /// The triples of occurrence `k` of a solution (`None`: the first), with weight `w`.
    fn emit(
        &self,
        p: &mut Pass,
        row: &Row,
        keys: &[Vec<u8>],
        occurrence: Option<i64>,
        w: i64,
        out: &mut Changes,
    ) {
        {
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
                                for k in keys {
                                    h.update((k.len() as u64).to_le_bytes());
                                    h.update(k);
                                }
                                if let Some(n) = occurrence {
                                    h.update([1]);
                                    h.update(n.to_le_bytes());
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
    }

    pub fn bytes(&self) -> u64 {
        self.counts.bytes(self.inner.schema.len())
    }

    pub fn save(&self, w: &mut Writer) -> Result<()> {
        w.zset(&self.counts)
    }

    pub fn load(&mut self, r: &mut Reader) -> Result<()> {
        self.counts = r.zset()?;
        Ok(())
    }

    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        self.counts = ZSet::new();
        let c = self.inner.init(p)?;
        self.apply(p, c)
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        let c = self.inner.delta(p)?;
        self.apply(p, c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incr::ops::Op;
    use crate::incr::ops::linear::Values;
    use crate::incr::pass::Vis;
    use crate::incr::terms::Terms;
    use crate::store::{Store, StoreOptions};
    use crate::task::Control;
    use oxrdf::NamedNode;

    /// Minting the occurrences of one solution with a large count checks the deadline.
    #[test]
    fn minting_occurrences_meets_the_deadline() {
        let s = Store::in_memory(StoreOptions::default());
        let snap = s.snapshot();
        let mut t = Template {
            inner: Node {
                op: Op::Values(Box::new(Values { rows: Vec::new() })),
                schema: vec![0],
                certain: vec![0],
            },
            triples: vec![[
                TSlot::Blank("b".into()),
                TSlot::Const(Term::NamedNode(NamedNode::new_unchecked("urn:p"))),
                TSlot::Var(Some(0)),
            ]],
            seed: String::new(),
            counts: ZSet::new(),
        };
        let mut terms = Terms::default();
        let vis = Vis::new(None, &snap);
        let control = Control {
            deadline: Some(std::time::Instant::now()),
            ..Control::none()
        };
        let vars = vec!["o".to_string()];
        let mut p = Pass::new(
            snap,
            None,
            &[],
            &[],
            &mut terms,
            &vis,
            None,
            &control,
            &vars,
        );
        let row: Row = vec![Id::from_i64(1).expect("inline")].into();
        assert!(t.apply(&mut p, vec![(row, 100_000)]).is_err());
    }
}
