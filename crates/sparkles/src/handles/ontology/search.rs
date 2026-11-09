//! Typeahead search over the entities of an ontology or thesaurus (item 616): labels,
//! synonyms, short forms and definitions in any language, with typo tolerance, from an
//! entity index that each branch keeps in memory and brings up to date from the changes
//! of each commit.
//!
//! An entity is an IRI with one of the OWL, RDFS or SKOS types of [`Kind`] in some
//! graph. Its values are its labels (`rdfs:label`, `skos:prefLabel`, SKOS-XL preferred
//! labels), synonyms (`skos:altLabel`, `skos:hiddenLabel`, the SKOS-XL ones and the
//! oboInOwl synonym properties), short forms (`oboInOwl:id`, `skos:notation` and the
//! local name of its IRI) and definitions (`skos:definition`, `IAO_0000115`). Each value
//! keeps its graph, so a search over some graphs, or by a caller who may read only some,
//! sees only theirs.
//!
//! Text is compared in a normalized form (case and diacritics folded, punctuation as
//! spaces). An entity is ranked by its best value, in these tiers:
//!
//! 0. a label equals the query
//! 1. a label starts with the query
//! 2. a synonym, short form or the IRI equals the query
//! 3. a synonym, short form or the IRI starts with the query
//! 4. every word of the query is a word of a label, the last one by prefix
//! 5. the same for a synonym or short form
//! 6. a label, synonym or short form within the edits [`max_edits`] allows, by its
//!    beginning or by its words
//! 7. every word of the query is a word of a definition, the last one by prefix
//!
//! and within a tier by the edits, the field, deprecated entities last, the language
//! ([`SearchRequest::languages`]), the text's length, the text and the IRI, so equal
//! requests on equal states give equal answers.
//!
//! The index is built at the first search of a branch, from the type facts and then the
//! watched properties, and each later search first applies the net changes of the
//! commits since the one it reflects ([`crate::incr::DeltaSet`]). A bulk commit, or
//! changes the logs no longer hold, build it again. Nothing is written to disk.
//!
//! ```
//! use sparkles::Dataset;
//! use sparkles::handles::ontology::OntologyRequest;
//! use sparkles::handles::ontology::search::SearchRequest;
//!
//! let ds = Dataset::memory();
//! ds.update(
//!     "PREFIX owl: <http://www.w3.org/2002/07/owl#>
//!      PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
//!      INSERT DATA {
//!        <http://ex.org/Glucose> a owl:Class ; rdfs:label \"glucose\"@en .
//!        <http://ex.org/Fructose> a owl:Class ; rdfs:label \"fructose\"@en .
//!      }",
//! )?;
//! let s = SearchRequest::new("glucsoe");
//! let found = ds.ontology().search(&OntologyRequest::default(), &s, &Default::default())?;
//! assert_eq!(found.hits[0].iri, "http://ex.org/Glucose");
//! # Ok::<_, sparkles::Error>(())
//! ```

mod fold_table;
mod index;
mod query;
#[cfg(test)]
mod tests;
mod text;

use super::{Ontologies, OntologyRequest};
use crate::error::{Error, Result};
use crate::incr::DeltaSet;
use crate::sparql::QueryOptions;
use crate::store::{Snapshot, Store};
use crate::task::Control;
use index::{Index, LOCAL_NAME, NOTATION, OBO_ID, PROPS};
use parking_lot::{Mutex, RwLock, RwLockReadGuard};
use sparkles_reasoner::inputs::{GraphRef, Import};
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub use index::{Field, Kind, MAX_ENTITIES};
pub use query::max_edits;
pub use text::normalize;

/// The hits a page holds by default.
pub const DEFAULT_LIMIT: usize = 10;
/// The most hits a page holds.
pub const MAX_LIMIT: usize = 100;
/// The most hits before the end of a page (`offset + limit`).
pub const MAX_WINDOW: usize = 10_000;
/// The most net changes applied to the index in place; more build it again.
pub const MAX_UPDATE_CHANGES: usize = 500_000;

/// What to search for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchRequest {
    pub query: String,
    /// the kinds of entity to keep (empty: every kind)
    pub kinds: Vec<Kind>,
    /// the caller's languages, most preferred first, as BCP 47 tags. Values in a
    /// preferred language rank first within a tier, then untagged ones, then English,
    /// then the others; without a preference untagged and English values rank alike,
    /// before the others.
    pub languages: Vec<String>,
    /// at most [`MAX_LIMIT`]
    pub limit: usize,
    pub offset: usize,
    /// rank the fuzzy tier (true by default)
    pub fuzzy: bool,
    /// rank the definitions tier (true by default)
    pub definitions: bool,
}

impl SearchRequest {
    /// A search for `query` with the defaults.
    pub fn new(query: impl Into<String>) -> SearchRequest {
        SearchRequest {
            query: query.into(),
            kinds: Vec::new(),
            languages: Vec::new(),
            limit: DEFAULT_LIMIT,
            offset: 0,
            fuzzy: true,
            definitions: true,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.limit == 0 || self.limit > MAX_LIMIT {
            return Err(Error::invalid(format!(
                "limit must be an integer from 1 to {MAX_LIMIT}"
            )));
        }
        if self.offset.saturating_add(self.limit) > MAX_WINDOW {
            return Err(Error::invalid(format!(
                "offset and limit together reach at most {MAX_WINDOW} hits"
            )));
        }
        if self.query.len() > 1000 {
            return Err(Error::invalid("the query is at most 1,000 bytes"));
        }
        Ok(())
    }
}

/// How a value matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MatchKind {
    /// the whole text equals the query
    Exact,
    /// the text starts with the query
    Prefix,
    /// the text's words include the query's
    Words,
    /// with edits
    Fuzzy,
}

impl MatchKind {
    pub fn name(self) -> &'static str {
        match self {
            MatchKind::Exact => "exact",
            MatchKind::Prefix => "prefix",
            MatchKind::Words => "words",
            MatchKind::Fuzzy => "fuzzy",
        }
    }
}

/// The value of a hit that matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub field: Field,
    /// the property of the value; `None` for the IRI and its local name
    pub property: Option<String>,
    pub text: String,
    pub lang: Option<String>,
    pub kind: MatchKind,
    /// the edits the match took
    pub distance: u8,
    /// the tier of the ranking (0 to 7, see the module documentation)
    pub tier: u8,
    /// the byte ranges of `text` that matched the query
    pub highlight: Vec<Range<usize>>,
}

/// One entity a search found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub iri: String,
    /// its kinds in the graphs read
    pub kinds: Vec<Kind>,
    /// its label in the best language, when it has one
    pub label: Option<String>,
    pub label_lang: Option<String>,
    /// its `oboInOwl:id`, `skos:notation`, prefixed name or local name
    pub short_form: Option<String>,
    pub deprecated: bool,
    pub matched: Match,
}

/// How the search found the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexUse {
    /// it reflected the head
    Current,
    /// it applied the changes of the commits since the one it reflected
    Updated,
    /// it was built from the state
    Built,
}

/// The answer of [`Ontologies::search`].
#[derive(Clone, Debug)]
pub struct SearchResults {
    /// the commit the index and the answer reflect
    pub commit: u64,
    pub hits: Vec<SearchHit>,
    /// whether hits follow the page
    pub more: bool,
    /// the graphs searched: those requested, then those their imports resolved to
    pub graphs: Vec<GraphRef>,
    pub imports: Vec<Import>,
    pub warnings: Vec<String>,
    pub index: IndexUse,
}

/// What a branch's entity index holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchStats {
    /// the commit it reflects (`None`: not built)
    pub commit: Option<u64>,
    pub entities: usize,
    pub values: usize,
    /// distinct normalized whole texts of labels, synonyms and short forms
    pub texts: usize,
    /// distinct normalized words
    pub words: usize,
    /// an estimate of the memory it holds, in bytes
    pub bytes: usize,
    /// the builds and in-place updates since the dataset opened
    pub builds: u64,
    pub updates: u64,
    /// how long the last build took, in milliseconds
    pub build_ms: u64,
}

/// A branch's entity index, which its [`crate::dataset::DatasetState`] keeps.
#[doc(hidden)]
#[derive(Default)]
pub struct SearchSlot {
    index: RwLock<Option<Index>>,
    /// held while the index is brought up to date, so one caller does it
    update: Mutex<()>,
    builds: AtomicU64,
    updates: AtomicU64,
    build_ms: AtomicU64,
}

impl SearchSlot {
    /// The index at the head of `store`, brought up to date, and the snapshot it reflects.
    ///
    /// `check` fails the call when it is cancelled or past its deadline. It runs while
    /// the call waits for another caller's update and during a build, and a failed build
    /// leaves the slot as it was.
    fn current(
        &self,
        store: &Store,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<(RwLockReadGuard<'_, Option<Index>>, Arc<Snapshot>, IndexUse)> {
        let snap = store.snapshot();
        {
            let g = self.index.read();
            if g.as_ref().is_some_and(|ix| ix.seq == snap.commit) {
                return Ok((g, snap, IndexUse::Current));
            }
        }
        check()?;
        let _u = loop {
            if let Some(u) = self.update.try_lock_for(Duration::from_millis(50)) {
                break u;
            }
            check()?;
        };
        let snap = store.snapshot();
        let seq = self.index.read().as_ref().map(|ix| ix.seq);
        if seq != Some(snap.commit) {
            check()?;
        }
        let (snap, used) = match seq {
            Some(s) if s == snap.commit => (snap, IndexUse::Current),
            Some(s) if s < snap.commit => match self.update_from(store, s) {
                Some(snap) => (snap, IndexUse::Updated),
                None => (self.build(&snap, check)?, IndexUse::Built),
            },
            _ => (self.build(&snap, check)?, IndexUse::Built),
        };
        Ok((self.index.read(), snap, used))
    }

    /// Apply the changes after commit `seq`, or `None` when they cannot be.
    fn update_from(&self, store: &Store, seq: u64) -> Option<Arc<Snapshot>> {
        let d = DeltaSet::read(store, seq, &Control::none());
        if !d.complete || d.added.len() + d.removed.len() > MAX_UPDATE_CHANGES {
            return None;
        }
        let mut w = self.index.write();
        let ix = w.as_mut()?;
        match ix.apply(&d.snapshot, &d.added, &d.removed) {
            Ok(true) => {
                if ix.wants_compaction() {
                    *ix = ix.compacted();
                }
                self.updates.fetch_add(1, Ordering::Relaxed);
                Some(d.snapshot.clone())
            }
            _ => {
                *w = None;
                None
            }
        }
    }

    fn build(&self, snap: &Arc<Snapshot>, check: &dyn Fn() -> Result<()>) -> Result<Arc<Snapshot>> {
        let started = Instant::now();
        let ix = Index::build(snap, check)?;
        let ms = started.elapsed().as_millis() as u64;
        tracing::debug!(
            "entity search index at commit {}: {} entities, {} values in {ms} ms",
            snap.commit,
            ix.live_entities,
            ix.values.len()
        );
        *self.index.write() = Some(ix);
        self.builds.fetch_add(1, Ordering::Relaxed);
        self.build_ms.store(ms, Ordering::Relaxed);
        Ok(snap.clone())
    }
}

/// The prefixed name of `iri` with the longest namespace of `prefixes` that leaves a
/// local part without `/`, `#` or `?`.
pub(super) fn prefixed(iri: &str, prefixes: &BTreeMap<String, String>) -> Option<String> {
    prefixes
        .iter()
        .filter(|(_, ns)| !ns.is_empty() && iri.len() > ns.len() && iri.starts_with(ns.as_str()))
        .filter(|(_, ns)| !iri[ns.len()..].contains(['/', '#', '?']))
        .max_by_key(|(p, ns)| (ns.len(), std::cmp::Reverse(p.len())))
        .map(|(p, ns)| format!("{p}:{}", &iri[ns.len()..]))
}

impl Ontologies {
    /// Search the entities of the ontology `req` names for `search`, as the caller of
    /// `opts` may read them: the requested graphs and those their imports resolve to.
    /// The search reads the head; `req.at` and `req.inferences` are refused.
    pub fn search(
        &self,
        req: &OntologyRequest,
        search: &SearchRequest,
        opts: &QueryOptions,
    ) -> Result<SearchResults> {
        req.validate()?;
        // a search of a retained commit or of inferences is roadmap item 658
        if req.at.is_some() {
            return Err(Error::invalid(
                "the entity search reads the head and does not take at",
            ));
        }
        if req.inferences {
            return Err(Error::invalid(
                "the entity search reads the asserted triples and does not take inferences",
            ));
        }
        search.validate()?;
        let opts = self.anchored(opts)?;
        let state = self.ds.state();
        let (guard, snap, used) = state
            .entity_search
            .current(self.ds.store(), &|| super::check(&opts))?;
        let Some(ix) = guard.as_ref() else {
            return Err(Error::invalid("the entity search index is not available"));
        };
        // the graphs read, through the caller's view
        let access = opts.graphs.as_ref().map(|a| a.bound(&snap));
        let view = match &access {
            Some(a) => a.masked(&snap)?,
            None => snap.clone(),
        };
        let readable = |g: crate::id::Id| access.as_ref().is_none_or(|a| a.readable_id(&snap, g));
        let (resolved, _) = req.resolve(&view, &readable);
        let mut graphs = vec![false; ix.graphs.len()];
        for g in &resolved.graphs {
            let Some(id) = g.id(&view) else { continue };
            if !readable(id) {
                continue;
            }
            let name = match g {
                GraphRef::Default => "",
                GraphRef::Named(n) => n.as_str(),
            };
            if let Some(&n) = ix.graph_ids.get(name) {
                graphs[n as usize] = true;
            }
        }
        let kinds = search.kinds.iter().fold(0u16, |k, x| k | x.bit());
        let protected = access.as_ref().is_some_and(|a| a.hides_triples());
        let scope = query::Scope::new(ix, graphs, kinds, protected.then_some(&*view));
        let langs = query::language_ranks(ix, &search.languages);
        let prefixes = self.ds.prefixes();
        let (cands, more, q) = query::run(&scope, search, &langs, &prefixes);
        super::check(&opts)?;
        let hits = cands
            .iter()
            .map(|c| hit(&scope, &langs, &prefixes, c, &q))
            .collect();
        let mut warnings = resolved.warnings;
        if q.iri_truncated {
            warnings.push(format!(
                "the IRI prefix matches more than {} IRIs, and the first {0} in IRI order are ranked",
                query::MAX_IRI_PREFIX
            ));
        }
        Ok(SearchResults {
            commit: snap.commit,
            hits,
            more,
            graphs: resolved.graphs,
            imports: resolved.imports,
            warnings,
            index: used,
        })
    }

    /// What this branch's entity index holds, without building it.
    pub fn search_stats(&self) -> SearchStats {
        let slot = &self.ds.state().entity_search;
        let g = slot.index.read();
        let mut s = SearchStats {
            builds: slot.builds.load(Ordering::Relaxed),
            updates: slot.updates.load(Ordering::Relaxed),
            build_ms: slot.build_ms.load(Ordering::Relaxed),
            ..Default::default()
        };
        if let Some(ix) = g.as_ref() {
            s.commit = Some(ix.seq);
            s.entities = ix.live_entities;
            s.values = ix.values.len() - ix.dead_values;
            s.texts = ix.labels.len() + ix.alts.len();
            s.words = ix.words.len();
            s.bytes = ix.bytes();
        }
        s
    }
}

/// The hit of candidate `c`.
fn hit(
    scope: &query::Scope<'_>,
    langs: &[u8],
    prefixes: &BTreeMap<String, String>,
    c: &query::Candidate<'_>,
    q: &query::Query,
) -> SearchHit {
    let ix = scope.ix;
    let ent = &ix.entities[c.entity as usize];
    let lang_of = |l: u16| {
        let t = &ix.langs[l as usize];
        (!t.is_empty()).then(|| t.to_string())
    };
    let visible = |v: &&u32| scope.value_ok(**v);
    // the label in the best language
    let label = ent
        .values
        .iter()
        .filter(|v| ix.values[**v as usize].field() == Field::Label)
        .filter(visible)
        .min_by_key(|v| {
            let x = &ix.values[**v as usize];
            (
                langs.get(x.lang as usize).copied().unwrap_or(u8::MAX),
                x.prop,
                &*x.text,
            )
        })
        .map(|v| &ix.values[*v as usize]);
    let short = |prop: u8| {
        ent.values
            .iter()
            .filter(|v| ix.values[**v as usize].prop == prop)
            .filter(visible)
            .map(|v| &*ix.values[*v as usize].text)
            .min()
            .map(str::to_string)
    };
    let short_form = short(OBO_ID)
        .or_else(|| short(NOTATION))
        .or_else(|| prefixed(&ent.iri, prefixes))
        .or_else(|| {
            let l = index::local_name(&ent.iri);
            (!l.is_empty()).then(|| l.to_string())
        });
    let kind_bits = scope.kinds_of(c.entity);
    let kinds = Kind::ALL
        .into_iter()
        .filter(|k| kind_bits & k.bit() != 0)
        .collect();
    let matched = match c.value {
        Some(v) => {
            let x = &ix.values[v as usize];
            Match {
                field: x.field(),
                property: (x.prop != LOCAL_NAME).then(|| PROPS[x.prop as usize].iri()),
                text: x.text.to_string(),
                lang: lang_of(x.lang),
                kind: c.kind,
                distance: c.rank.distance,
                tier: c.rank.tier,
                highlight: query::highlight(&x.text, c.kind, c.whole, q),
            }
        }
        None => {
            let iri = q.iri.as_deref().unwrap_or("");
            Match {
                field: Field::ShortForm,
                property: None,
                text: ent.iri.to_string(),
                lang: None,
                kind: c.kind,
                distance: 0,
                tier: c.rank.tier,
                highlight: if ent.iri.starts_with(iri) && !iri.is_empty() {
                    std::iter::once(0..iri.len()).collect()
                } else {
                    Vec::new()
                },
            }
        }
    };
    SearchHit {
        iri: ent.iri.to_string(),
        kinds,
        label: label.map(|x| x.text.to_string()),
        label_lang: label.and_then(|x| lang_of(x.lang)),
        short_form,
        deprecated: scope.deprecated(c.entity),
        matched,
    }
}
