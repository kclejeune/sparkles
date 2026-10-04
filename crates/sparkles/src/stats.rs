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
    /// past one.
    pub fn stats_of(&self, snap: &Snapshot, at: Option<&Resolved>) -> Result<DatasetStats> {
        let gen_ = &snap.generation;
        let term = |id: u64| {
            snap.term(Id(id)).map(|t| match t {
                oxrdf::Term::NamedNode(n) => n.into_string(),
                t => t.to_string(),
            })
        };
        let mut graphs = Vec::new();
        for g in snap.distinct_first(Perm::Gspo)? {
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
            preds.push((p, snap.count(Perm::Pso, &[p])?));
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
                PredicateCount {
                    iri: term(p).unwrap_or_default(),
                    count,
                    distinct_subjects: ps.map_or(0, |s| s.distinct_subjects),
                    distinct_objects: ps.map_or(0, |s| s.distinct_objects),
                }
            })
            .collect();
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
            quads: snap.len(),
            base_quads: gen_.meta.quads,
            delta_inserts: snap.delta.inserts(),
            delta_deletes: snap.delta.deletes(),
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
