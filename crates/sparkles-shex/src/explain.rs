//! Reasons for nonconformant results: each one's pair re-evaluated once against the
//! final typing, recording up to 8 [`crate::ShexFailure`]s and a one-line reason. A
//! failing reference is reported as such, without the referenced pair's own reason.
//! Pairs the typing never reached (discovery stops at pairs that fail whatever their
//! references) read as unknown: an arc whose value may conform counts as matching.

use crate::check::{show_iri, show_label};
use crate::ir::{Dir, PairKind, Se, SeId, ShapeClass, ShapeId, Tri};
use crate::matcher::{self, Budget};
use crate::neigh::{self, Neigh};
use crate::typing::{Env, Read, Worker};
use crate::{PrefixMap, ShexFailure};
use oxrdf::Term;
use sparkles::id::Id;
use std::cell::RefCell;

/// The most failures recorded per result.
pub const MAX_FAILURES: usize = 8;

/// Why `node` does not satisfy pair kind `kind` under the final typing `read`.
pub fn explain(
    env: &Env<'_>,
    read: &Read<'_>,
    prefixes: &PrefixMap,
    w: &mut Worker<'_>,
    node: Id,
    kind: PairKind,
) -> anyhow::Result<Vec<ShexFailure>> {
    let mut x = Explainer {
        env,
        read,
        prefixes,
        out: Vec::new(),
        neigh: Neigh::default(),
    };
    x.se(env.ir.pairs[kind.index()].se, node, w)?;
    Ok(x.out)
}

struct Explainer<'e, 'a, 'r> {
    env: &'e Env<'a>,
    read: &'e Read<'r>,
    prefixes: &'e PrefixMap,
    out: Vec<ShexFailure>,
    neigh: Neigh,
}

impl Explainer<'_, '_, '_> {
    fn full(&self) -> bool {
        self.out.len() >= MAX_FAILURES
    }

    fn push(&mut self, f: ShexFailure) {
        if !self.full() {
            self.out.push(f);
        }
    }

    /// The label of a pair kind for messages.
    fn label(&self, kind: PairKind) -> String {
        match &self.env.ir.pairs[kind.index()].label {
            Some(l) => show_label(l, self.prefixes),
            None if self.env.ir.start == Some(kind) => "START".to_string(),
            None => "an anonymous shape".to_string(),
        }
    }

    /// Record why `se` fails at `node` (it does).
    fn se(&mut self, se: SeId, node: Id, w: &mut Worker<'_>) -> anyhow::Result<()> {
        if self.full() {
            return Ok(());
        }
        let env = self.env;
        match &env.ir.ses[se.index()] {
            Se::And(xs) => {
                for &x in xs {
                    if env.eval(x, node, self.read, w)? == Tri::False {
                        self.se(x, node, w)?;
                    }
                }
            }
            Se::Or(xs) => {
                for &x in xs {
                    self.se(x, node, w)?;
                }
            }
            Se::Not(x) => {
                let shape = match &env.ir.ses[x.index()] {
                    Se::Ref(k) => self.label(*k),
                    _ => "a shape expression".to_string(),
                };
                self.push(ShexFailure::Not { shape });
            }
            // a pair the typing never reached (its reader failed for other reasons) is
            // no reason
            Se::Ref(k) if (self.read)(node, *k) != Tri::False => {}
            Se::Ref(k) => {
                let shape = self.label(*k);
                self.push(ShexFailure::Reference {
                    shape,
                    value: env.term(node),
                });
            }
            Se::Nc(n) => {
                let f = match env.absent.get(node.payload() as usize) {
                    Some(t) if node.tag() == sparkles::id::Tag::Local => {
                        env.nc.explain_term(*n, t)?
                    }
                    _ => env.nc.explain(*n, node)?,
                };
                if let Some(f) = f {
                    self.push(f);
                }
            }
            Se::Shape(s) => self.shape(*s, node, w)?,
            Se::External => self.push(ShexFailure::External {
                shape: "an external shape".to_string(),
            }),
        }
        Ok(())
    }

    /// Record why `node` does not match shape `sid`.
    fn shape(&mut self, sid: ShapeId, node: Id, w: &mut Worker<'_>) -> anyhow::Result<()> {
        let env = self.env;
        let shape = &env.ir.shapes[sid.index()];
        let mut neigh = std::mem::take(&mut self.neigh);
        neigh::fetch_all(env.data, &env.plan, node, sid, &mut neigh)?;
        let before = self.out.len();
        if let Some((p, o)) = neigh.closed_violation {
            self.push(ShexFailure::Closed {
                predicate: iri_of(&env.term(p)),
                value: env.term(o),
            });
        }
        let error = RefCell::new(None);
        let flat = shape.class == ShapeClass::Flat;
        for (e, (pred, dir, tcs)) in shape.preds.iter().enumerate() {
            let extra = shape.extra.iter().any(|x| x == pred);
            let mut count = 0u64;
            for &v in &neigh.values[e] {
                let cands = tcs
                    .iter()
                    .filter(|&&tc| env.value(shape, tc, v, self.read, &error) != Tri::False)
                    .count();
                if cands > 0 {
                    count += 1;
                } else if !extra {
                    if let [tc] = tcs[..] {
                        // the value fails the constraint's value expression
                        let t = &shape.tcs[tc.index()];
                        match (t.value, t.pair) {
                            (Some(se), _) if env.is_pure(se) => self.se(se, v, w)?,
                            (_, Some(k)) => {
                                let shape = self.label(k);
                                self.push(ShexFailure::Reference {
                                    shape,
                                    value: env.term(v),
                                });
                            }
                            _ => {}
                        }
                    } else {
                        self.push(ShexFailure::Extra {
                            predicate: pred.clone(),
                            value: env.term(v),
                        });
                    }
                }
            }
            if flat && let [tc] = tcs[..] {
                let t = &shape.tcs[tc.index()];
                if count < u64::from(t.min) || t.max.is_some_and(|m| count > u64::from(m)) {
                    self.push(ShexFailure::Cardinality {
                        predicate: pred.clone(),
                        inverse: *dir == Dir::In,
                        min: t.min,
                        max: t.max,
                        count,
                    });
                }
            }
        }
        if let Some(e) = error.into_inner() {
            return Err(e);
        }
        if self.out.len() == before {
            // nothing local: the partition, or a semantic action
            let value = |v: Id, tc| env.value(shape, tc, v, self.read, &RefCell::new(None));
            let mut budget = Budget::new(env.max_partitions);
            w.cx.failure = None;
            let r = matcher::matches(shape, &neigh, &value, &mut budget, &mut w.cx)?;
            if r != Tri::True {
                let f = match w.cx.failure.take() {
                    Some(a) => ShexFailure::SemAct {
                        extension: a.extension,
                        message: a.message,
                    },
                    None => ShexFailure::NoMatch {
                        detail: "no partition of the arcs matches the triple expression"
                            .to_string(),
                    },
                };
                self.push(f);
            }
        }
        self.neigh = neigh;
        Ok(())
    }
}

fn iri_of(t: &Term) -> String {
    match t {
        Term::NamedNode(n) => n.as_str().to_string(),
        t => t.to_string(),
    }
}

/// A term for messages: IRIs as prefixed names when a prefix fits, integers bare,
/// other literals in N-Triples syntax.
pub fn show_term(t: &Term, prefixes: &PrefixMap) -> String {
    match t {
        Term::NamedNode(n) => show_iri(n.as_str(), prefixes),
        Term::Literal(l) if l.datatype() == oxrdf::vocab::xsd::INTEGER => l.value().to_string(),
        t => t.to_string(),
    }
}

/// The one-line reason of a nonconformant result: its node and first failure.
pub fn reason(node: &Term, f: &ShexFailure, prefixes: &PrefixMap) -> String {
    let term = |t: &Term| show_term(t, prefixes);
    let iri = |i: &str| show_iri(i, prefixes);
    let what = match f {
        ShexFailure::NodeKind { value, constraint }
        | ShexFailure::Datatype { value, constraint }
        | ShexFailure::Facet { value, constraint }
        | ShexFailure::ValueSet { value, constraint } => {
            format!("{} fails {constraint}", term(value))
        }
        ShexFailure::Cardinality {
            predicate,
            inverse,
            min,
            max,
            count,
        } => {
            let max = max.map_or("*".to_string(), |m| m.to_string());
            let hat = if *inverse { "^" } else { "" };
            format!(
                "{count} {hat}{} arcs, {{{min},{max}}} needed",
                iri(predicate)
            )
        }
        ShexFailure::Closed { predicate, value } => {
            format!("CLOSED shape forbids {} {}", iri(predicate), term(value))
        }
        ShexFailure::Extra { predicate, value } => format!(
            "{} {} matches no triple constraint",
            iri(predicate),
            term(value)
        ),
        ShexFailure::NoMatch { detail } => detail.clone(),
        ShexFailure::Reference { shape, value } => {
            format!("{} does not conform to {shape}", term(value))
        }
        ShexFailure::Not { shape } => format!("conforms to {shape} under NOT"),
        ShexFailure::SemAct { extension, message } => {
            format!("semantic action <{extension}> failed: {message}")
        }
        ShexFailure::External { shape } => format!("{shape} has no definition"),
    };
    format!("{}: {what}", term(node))
}
