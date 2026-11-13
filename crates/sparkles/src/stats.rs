//! What a dataset holds and how it is stored: the counts of `GET /$/stats/{ds}` that
//! describe the data (see [`Dataset::stats`]).

use crate::Dataset;
use crate::error::Result;
use crate::history::{At, HistoryOptions, Resolved};
use crate::id::Id;
use crate::index::Perm;
use crate::store::Snapshot;
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

/// The graphs listed at most.
const MAX_GRAPHS: usize = 1000;
/// The predicates counted at most, and the most listed.
const MAX_PREDICATES: usize = 10_000;
const TOP: usize = 100;

/// Options of [`Dataset::stats`].
#[derive(Clone, Debug, Default)]
pub struct StatsOptions {
    /// A past state (`None`: the head).
    pub at: Option<At>,
    /// Cancels opening a past state.
    pub cancel: Option<Arc<AtomicBool>>,
    /// The deadline of opening a past state.
    pub deadline: Option<Instant>,
}

/// The counts of a dataset's state. Its serde form is that of the members of
/// `GET /$/stats/{ds}` that describe the data.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DatasetStats {
    /// the commit the counts are of
    pub commit: u64,
    /// the `at` selector the state was read at, if a past one was asked for
    pub at: Option<String>,
    pub history: HistorySummary,
    pub quads: u64,
    /// the quads of the base index generation
    pub base_quads: u64,
    /// the inserts and deletes held over the base generation
    pub delta_inserts: usize,
    pub delta_deletes: usize,
    pub terms: u64,
    /// the name of the base index generation
    pub generation: String,
    /// the quads of each graph (`name` `None` for the default graph), at most 1000
    pub graphs: Vec<GraphCount>,
    /// the 100 predicates with the most triples
    pub predicates: Vec<PredicateCount>,
    /// the 100 classes with the most instances
    pub classes: Vec<ClassCount>,
    pub disk_bytes: u64,
    /// the block cache
    pub cache: CacheCounts,
    pub result_cache: ResultCacheCounts,
    pub service_cache: ServiceCacheCounts,
}

/// The history a dataset keeps.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct HistorySummary {
    pub bytes: u64,
    /// the past index generations kept
    pub generations: usize,
    pub snapshots: usize,
    pub oldest_reconstructable: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct GraphCount {
    pub name: Option<String>,
    pub quads: u64,
}

/// A predicate's exact count, and its distinct subjects and objects from the build
/// statistics.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct PredicateCount {
    pub iri: String,
    pub count: u64,
    pub distinct_subjects: u64,
    pub distinct_objects: u64,
}

/// The distinct subjects typed with a class over every graph.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ClassCount {
    pub iri: String,
    pub instances: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct CacheCounts {
    pub entries: usize,
    pub bytes: u64,
    pub hits: u64,
    pub misses: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ResultCacheCounts {
    pub enabled: bool,
    pub entries: usize,
    pub bytes: u64,
    pub hits: u64,
    pub misses: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ServiceCacheCounts {
    pub enabled: bool,
    pub entries: usize,
    pub bytes: u64,
    pub capacity_bytes: u64,
    pub hits: u64,
    pub misses: u64,
}

impl Dataset {
    /// The counts of the state `opts.at` names, or of the head.
    pub fn stats(&self, opts: &StatsOptions) -> Result<DatasetStats> {
        match &opts.at {
            None => self.stats_of(&self.snapshot(), None),
            Some(at) => {
                let o = HistoryOptions {
                    cancel: opts.cancel.clone(),
                    deadline: opts.deadline,
                };
                let (snap, r) = self.store().snapshot_at(at, &o)?;
                self.stats_of(&snap, Some(&r))
            }
        }
    }

    /// The counts of a state the caller has opened, which `at` resolved when it is a
    /// past one. The dataset's hidden control graphs (spec C26 §2.3), such as the job
    /// ledger, are left out of every figure that counts quads: the graph list, the quad,
    /// base and delta totals, the predicate counts and the class instances. The
    /// vocabulary size and the history figures describe the storage as a whole.
    pub fn stats_of(&self, snap: &Snapshot, at: Option<&Resolved>) -> Result<DatasetStats> {
        let gen_ = &snap.generation;
        let term = |id: u64| {
            snap.term(Id(id)).map(|t| match t {
                oxrdf::Term::NamedNode(n) => n.into_string(),
                t => t.to_string(),
            })
        };
        let hidden = Hidden::of(snap, &self.store().hidden_graphs())?;
        let mut graphs = Vec::new();
        for g in snap.distinct_first(Perm::Gspo)? {
            if hidden.graph(g) {
                continue;
            }
            let quads = snap.count(Perm::Gspo, &[g])?;
            let name = if g == Id::DEFAULT_GRAPH.0 {
                None
            } else {
                term(g)
            };
            graphs.push(GraphCount { name, quads });
            if graphs.len() >= MAX_GRAPHS {
                break;
            }
        }
        // predicates: exact counts; distinct subjects and objects from build statistics
        let mut preds: Vec<(u64, u64)> = Vec::new();
        for p in snap.distinct_first(Perm::Pso)? {
            let count = snap
                .count(Perm::Pso, &[p])?
                .saturating_sub(hidden.predicate(p));
            if count == 0 {
                continue;
            }
            preds.push((p, count));
            if preds.len() >= MAX_PREDICATES {
                break;
            }
        }
        preds.sort_by_key(|p| std::cmp::Reverse(p.1));
        let predicates = preds
            .iter()
            .take(TOP)
            .map(|&(p, count)| {
                let ps = gen_.stats.predicate(p);
                // less the subjects and objects only a hidden graph gives the predicate
                let (hs, ho) = if hidden.predicate(p) > 0 {
                    hidden.hidden_terms(snap, p)?
                } else {
                    (0, 0)
                };
                Ok(PredicateCount {
                    iri: term(p).unwrap_or_default(),
                    count,
                    distinct_subjects: ps.map_or(0, |s| s.distinct_subjects).saturating_sub(hs),
                    distinct_objects: ps.map_or(0, |s| s.distinct_objects).saturating_sub(ho),
                })
            })
            .collect::<Result<_>>()?;
        // classes: distinct subjects typed with each class over every graph, as the
        // build statistics count them; after updates, from one ordered pass over
        // POS[rdf:type], where a subject typed in several graphs counts once
        let mut classes: Vec<(u64, u64)> = if snap.delta.is_empty() {
            gen_.stats.classes.clone()
        } else {
            let mut v: Vec<(u64, u64)> = Vec::new();
            if let Some(t) = snap.lookup_iri(oxrdf::vocab::rdf::TYPE.as_str()) {
                let mut prev: Option<(u64, u64)> = None;
                let mut visit = |k: &crate::index::Key| {
                    if prev == Some((k[1], k[2])) {
                        return;
                    }
                    match v.last_mut() {
                        Some((c, n)) if *c == k[1] => *n += 1,
                        _ => v.push((k[1], 1)),
                    }
                    prev = Some((k[1], k[2]));
                };
                snap.scan(Perm::Pos, &[t.0], |c| {
                    match c {
                        crate::store::Chunk::Block(b, s, e) => {
                            (s..e).for_each(|i| visit(&b.key(i)))
                        }
                        crate::store::Chunk::Row(k) => visit(&k),
                    }
                    Ok(true)
                })?;
            }
            v
        };
        hidden.correct_classes(snap, &mut classes)?;
        classes.sort_by_key(|c| std::cmp::Reverse(c.1));
        let classes = classes
            .iter()
            .take(TOP)
            .map(|&(c, instances)| ClassCount {
                iri: term(c).unwrap_or_default(),
                instances,
            })
            .collect();
        let store = self.store();
        let cache = store.cache();
        let rcache = store.result_cache();
        let h = store.history();
        Ok(DatasetStats {
            commit: snap.commit,
            at: at.map(|r| r.at.to_string()),
            history: HistorySummary {
                bytes: h.bytes,
                generations: h.generations.iter().filter(|g| !g.current).count(),
                snapshots: h.snapshots,
                oldest_reconstructable: h.oldest_reconstructable(),
            },
            quads: snap.len().saturating_sub(hidden.quads()),
            base_quads: gen_.meta.quads.saturating_sub(hidden.base),
            delta_inserts: snap.delta.inserts().saturating_sub(hidden.inserts),
            delta_deletes: snap.delta.deletes().saturating_sub(hidden.deletes),
            terms: gen_.vocab.len() + gen_.dvocab.len(),
            generation: gen_.name.clone(),
            graphs,
            predicates,
            classes,
            disk_bytes: store.disk_bytes(),
            cache: CacheCounts {
                entries: cache.entries(),
                bytes: cache.bytes(),
                hits: cache.hits(),
                misses: cache.misses(),
            },
            result_cache: ResultCacheCounts {
                enabled: rcache.enabled(),
                entries: rcache.entries(),
                bytes: rcache.bytes(),
                hits: rcache.hits(),
                misses: rcache.misses(),
            },
            service_cache: ServiceCacheCounts {
                enabled: rcache.service.enabled(),
                entries: rcache.service.entries(),
                bytes: rcache.service.bytes(),
                capacity_bytes: rcache.service.capacity(),
                hits: rcache.service.hits(),
                misses: rcache.service.misses(),
            },
        })
    }
}

/// What the dataset's hidden control graphs hold in one snapshot (spec C26 §2.3), so that
/// the statistics leave it out: their ids, their quads in the base and the delta, their
/// quads per predicate and the (class, subject) pairs they type. A hidden graph is small
/// next to the dataset, so one pass over it costs less than a pass over the visible
/// graphs.
#[derive(Default)]
struct Hidden {
    ids: Vec<u64>,
    base: u64,
    inserts: usize,
    deletes: usize,
    predicates: std::collections::BTreeMap<u64, u64>,
    typed: std::collections::BTreeSet<(u64, u64)>,
    /// the (predicate, subject) and (predicate, object) pairs of their quads in the base,
    /// which the build statistics count among the distinct subjects and objects
    subjects: std::collections::BTreeSet<(u64, u64)>,
    objects: std::collections::BTreeSet<(u64, u64)>,
}

impl Hidden {
    fn of(snap: &Snapshot, graphs: &[String]) -> Result<Hidden> {
        let mut h = Hidden::default();
        if graphs.is_empty() {
            return Ok(h);
        }
        let rdf_type = snap
            .lookup_iri(oxrdf::vocab::rdf::TYPE.as_str())
            .map(|t| t.0);
        for iri in graphs {
            let Some(g) = snap.lookup_iri(iri).map(|g| g.0) else {
                continue;
            };
            let (base, ins, del) = snap.count_split(Perm::Gspo, &[g])?;
            h.base += base;
            h.inserts += ins as usize;
            h.deletes += del as usize;
            // GSPO keys are [graph, subject, predicate, object]
            let inserted = &snap.delta.ins[Perm::Gspo.index()];
            let mut visit = |k: &crate::index::Key| {
                *h.predicates.entry(k[2]).or_default() += 1;
                if Some(k[2]) == rdf_type {
                    h.typed.insert((k[3], k[1]));
                }
                if !inserted.contains(k) {
                    h.subjects.insert((k[2], k[1]));
                    h.objects.insert((k[2], k[3]));
                }
            };
            snap.scan(Perm::Gspo, &[g], |c| {
                match c {
                    crate::store::Chunk::Block(b, s, e) => (s..e).for_each(|i| visit(&b.key(i))),
                    crate::store::Chunk::Row(k) => visit(&k),
                }
                Ok(true)
            })?;
            h.ids.push(g);
        }
        Ok(h)
    }

    fn graph(&self, g: u64) -> bool {
        self.ids.contains(&g)
    }

    fn quads(&self) -> u64 {
        (self.base + self.inserts as u64).saturating_sub(self.deletes as u64)
    }

    fn predicate(&self, p: u64) -> u64 {
        self.predicates.get(&p).copied().unwrap_or(0)
    }

    /// The distinct subjects and objects of predicate `p` that the build statistics count
    /// and that only a hidden graph gives it: those of the base quads of the hidden
    /// graphs that no visible quad of `p` shares.
    fn hidden_terms(&self, snap: &Snapshot, p: u64) -> Result<(u64, u64)> {
        let mut out = (0, 0);
        // PSO keys are [predicate, subject, object, graph], POS keys
        // [predicate, object, subject, graph]
        for (pairs, perm, n) in [
            (&self.subjects, Perm::Pso, &mut out.0),
            (&self.objects, Perm::Pos, &mut out.1),
        ] {
            for &(_, t) in pairs.range((p, 0)..=(p, u64::MAX)) {
                let mut visible = false;
                snap.scan(perm, &[p, t], |c| {
                    let in_visible = |k: &crate::index::Key| !self.graph(k[3]);
                    visible = match c {
                        crate::store::Chunk::Block(b, s, e) => {
                            (s..e).any(|i| in_visible(&b.key(i)))
                        }
                        crate::store::Chunk::Row(k) => in_visible(&k),
                    };
                    Ok(!visible)
                })?;
                if !visible {
                    *n += 1;
                }
            }
        }
        Ok(out)
    }

    /// Take out of `classes`, which count the distinct subjects typed with each class
    /// over every graph, the subjects that only a hidden graph types.
    fn correct_classes(&self, snap: &Snapshot, classes: &mut Vec<(u64, u64)>) -> Result<()> {
        if self.typed.is_empty() {
            return Ok(());
        }
        let Some(t) = snap
            .lookup_iri(oxrdf::vocab::rdf::TYPE.as_str())
            .map(|t| t.0)
        else {
            return Ok(());
        };
        for &(class, subject) in &self.typed {
            // POS keys are [predicate, object, subject, graph]
            let mut visible = false;
            snap.scan(Perm::Pos, &[t, class, subject], |c| {
                let in_visible = |k: &crate::index::Key| !self.graph(k[3]);
                visible = match c {
                    crate::store::Chunk::Block(b, s, e) => (s..e).any(|i| in_visible(&b.key(i))),
                    crate::store::Chunk::Row(k) => in_visible(&k),
                };
                Ok(!visible)
            })?;
            if !visible && let Some(c) = classes.iter_mut().find(|c| c.0 == class) {
                c.1 = c.1.saturating_sub(1);
            }
        }
        classes.retain(|c| c.1 > 0);
        Ok(())
    }
}
