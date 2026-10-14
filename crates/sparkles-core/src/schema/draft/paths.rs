//! Inverse and sequence property shapes of the draft.
//!
//! An inverse property shape describes the subjects that point at the instances of a
//! class over one predicate, counted in the pass over `POS[p]`. A sequence `p/q` describes
//! the values of `q` at the blank nodes `p` leads to, for the blank nodes that no drafted
//! class describes (an address node, say), found by one more pass over the predicates
//! after the direct profiles.

use super::{
    Acc, Budget, Ctx, DraftOptions, GraphFilter, InlineDts, Iris, K_BLANK, K_IRI, K_TRIPLE, Names,
    PatNames, PropertyDraft, SchemaError, Src, Summary, Types, decide, in_phase, is_iri,
    object_infos, summarize,
};
use crate::id::{Id, Tag};
use crate::index::Perm;
use rustc_hash::FxHashMap;

/// Blank nodes tracked for sequence paths; past it there are none.
const MAX_SEQUENCE_NODES: usize = 1_000_000;

/// The kind bits of a subject.
fn subject_kind(snap: &Src, id: u64) -> u8 {
    let id = Id(id);
    match id.tag() {
        Tag::BNode => K_BLANK,
        Tag::Vocab => {
            let v = &snap.generation.vocab;
            if v.is_iri(id.payload()) {
                K_IRI
            } else if v.is_triple(id.payload()) {
                K_TRIPLE
            } else {
                0
            }
        }
        Tag::Delta => match snap.key(id).as_deref().and_then(|k| k.first()) {
            Some(b'<') => K_IRI,
            Some(b'_') => K_BLANK,
            Some(b'(') => K_TRIPLE,
            _ => 0,
        },
        _ => 0,
    }
}

/// The profile, for each drafted class, of the subjects that point at its instances over
/// `p`.
pub(super) fn inverse_accs(
    snap: &Src,
    p: u64,
    filter: &GraphFilter,
    budget: &Budget,
    types: &Types,
    drafted: &[bool],
    phase: &dyn Fn() -> String,
) -> Result<FxHashMap<u32, Acc>, SchemaError> {
    let mut accs: FxHashMap<u32, Acc> = FxHashMap::default();
    let mut object: Option<u64> = None;
    let mut subjects: Vec<u64> = Vec::new();
    let flush = |o: u64, subjects: &[u64], accs: &mut FxHashMap<u32, Acc>| {
        let set = types.set_of(o);
        if !set.iter().any(|c| drafted[*c as usize]) {
            return;
        }
        let mut mask = 0u8;
        let mut classes: Option<Vec<u32>> = None;
        for &s in subjects {
            let kind = subject_kind(snap, s);
            mask |= kind;
            if kind & (K_IRI | K_BLANK) != 0 {
                let cs = types.set_of(s);
                classes = Some(match classes {
                    None => cs.to_vec(),
                    Some(prev) => prev.into_iter().filter(|c| cs.contains(c)).collect(),
                });
            }
        }
        let sum = Summary {
            n: subjects.len() as u64,
            mask,
            dt: None,
            classes: if mask & !(K_IRI | K_BLANK) == 0 {
                classes.unwrap_or_default()
            } else {
                Vec::new()
            },
            values: None,
            langs: None,
            tagged: 0,
            dup_lang: false,
            dts: None,
            strings: false,
            skels: None,
            heads: Vec::new(),
            span: None,
        };
        for c in set {
            if drafted[*c as usize] {
                accs.entry(*c).or_insert_with(Acc::new).add(&sum);
            }
        }
    };
    let mut prev = None;
    in_phase(
        snap.for_each_key(Perm::Pos, &[p], budget, |k| {
            // POS: the object, then the subject
            if !filter.accepts(k[3]) || prev == Some((k[1], k[2])) {
                return;
            }
            prev = Some((k[1], k[2]));
            if object != Some(k[1]) {
                if let Some(o) = object {
                    flush(o, &subjects, &mut accs);
                }
                object = Some(k[1]);
                subjects.clear();
            }
            subjects.push(k[2]);
        }),
        phase,
    )?;
    if let Some(o) = object {
        flush(o, &subjects, &mut accs);
    }
    Ok(accs)
}

/// The blank nodes that sequence paths can lead through: values of a predicate at
/// instances of drafted classes that no drafted class describes.
#[derive(Default)]
pub(super) struct Sequences {
    /// the predicates, by the number the entries use
    preds: Vec<u64>,
    /// blank node → (predicate number, instance) for each instance it is a value of
    by_node: FxHashMap<u64, Vec<(u32, u64)>>,
    entries: usize,
    full: bool,
}

impl Sequences {
    /// The number of predicate `p`, for [`note`](Self::note).
    pub fn predicate(&mut self, p: u64) -> u32 {
        self.preds.push(p);
        (self.preds.len() - 1) as u32
    }

    /// The values of predicate number `pred` at instance `s` (of a drafted class).
    pub fn note(&mut self, pred: u32, s: u64, objs: &[u64], types: &Types, drafted: &[bool]) {
        for &o in objs {
            if self.full {
                return;
            }
            if Id(o).tag() == Tag::BNode && !types.set_of(o).iter().any(|c| drafted[*c as usize]) {
                if self.entries >= MAX_SEQUENCE_NODES {
                    self.full = true;
                    self.by_node.clear();
                    return;
                }
                self.entries += 1;
                self.by_node.entry(o).or_default().push((pred, s));
            }
        }
    }
}

/// The profiles of the sequences `p/q`: the values of `q` at the blank nodes that `p`
/// leads to, per instance of each drafted class. Each is a [`PropertyDraft`] with `then`
/// set, with the class (an index into `types`) it belongs to.
#[allow(clippy::too_many_arguments)]
pub(super) fn sequence_props(
    seq: &Sequences,
    snap: &Src,
    preds: &[u64],
    rdf_type: Option<u64>,
    filter: &GraphFilter,
    budget: &Budget,
    types: &Types,
    drafted: &[bool],
    inline: &InlineDts,
    dts: &mut Names,
    langs: &mut Names,
    iris: &mut Iris<'_>,
    class_iris: &[Option<String>],
    opts: &DraftOptions,
) -> Result<Vec<(u32, PropertyDraft)>, SchemaError> {
    let mut out = Vec::new();
    if seq.by_node.is_empty() {
        return Ok(out);
    }
    for (i, &q) in preds.iter().enumerate() {
        if Some(q) == rdf_type || !is_iri(snap, q) {
            continue;
        }
        let phase = || format!("profiling sequence paths ({}/{})", i + 1, preds.len());
        // the values of q at each blank node, for each instance that reaches it
        let mut hits: FxHashMap<(u32, u64), Vec<u64>> = FxHashMap::default();
        let mut prev = None;
        in_phase(
            snap.for_each_key(Perm::Pso, &[q], budget, |k| {
                if !filter.accepts(k[3]) || prev == Some((k[1], k[2])) {
                    return;
                }
                prev = Some((k[1], k[2]));
                if let Some(owners) = seq.by_node.get(&k[1]) {
                    for &owner in owners {
                        hits.entry(owner).or_default().push(k[2]);
                    }
                }
            }),
            phase,
        )?;
        if hits.is_empty() {
            continue;
        }
        let mut pat = PatNames::default();
        let infos = in_phase(
            object_infos(snap, q, filter, budget, dts, langs, &mut pat),
            phase,
        )?;
        let mut accs: FxHashMap<(u32, u32), Acc> = FxHashMap::default();
        for ((pi, owner), mut objs) in hits {
            objs.sort_unstable();
            objs.dedup();
            let sum = summarize(&objs, snap, types, &infos, inline);
            for c in types.set_of(owner) {
                if drafted[*c as usize] {
                    accs.entry((pi, *c)).or_insert_with(Acc::new).add(&sum);
                }
            }
        }
        let Some(then) = iris.get(q) else { continue };
        let cx = Ctx {
            snap,
            types,
            iris: class_iris,
            dts,
            langs,
            pat: &pat,
            opts,
        };
        for ((pi, c), acc) in accs {
            let Some(path) = iris.get(seq.preds[pi as usize]) else {
                continue;
            };
            let mut d = decide(path, types.instances[c as usize], &acc, &cx);
            // a sequence path counts the instances it reaches: one that few reach says
            // little, and needs no property shape
            if d.constraints.iter().any(|x| x.component == "minCount") {
                d.then = Some(then.clone());
                out.push((c, d));
            }
        }
    }
    Ok(out)
}
