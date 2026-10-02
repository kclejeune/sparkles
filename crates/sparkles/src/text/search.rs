//! Evaluation of a `text:query` call: the Tantivy query, its hits, and the terms of each
//! hit as ids.
//!
//! A document keeps its quad's terms in columns (fast fields). The terms of the hits are
//! read segment by segment, and each distinct term is decoded and looked up in the
//! vocabulary once, in sorted order. Only the columns the call's outputs need are read:
//! a call whose literal is not used afterwards never reads the object column, and the
//! predicate and graph columns are read only for the property and graph outputs. The
//! output is built column by column.

use super::imp::{Resolved, graph_name, key_hash, term_key, text_err};
use super::{DEFAULT_GRAPH_IRI, unavailable};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::ctx::Ctx;
use crate::sparql::plan::{GraphFilter, PathEnd, TextSpec};
use crate::sparql::table::{Table, VarId};
use crate::store::Snapshot;
use rustc_hash::{FxHashMap, FxHashSet};
use tantivy::collector::{Collector, SegmentCollector, TopDocs};
use tantivy::columnar::BytesColumn;
use tantivy::query::{BooleanQuery, ConstScoreQuery, Occur, Query, TermQuery, TermSetQuery};
use tantivy::schema::{Field, IndexRecordOption};
use tantivy::{DocAddress, DocId, Score, SegmentOrdinal, SegmentReader, Term};

/// Evaluate a `text:query` call against the snapshot's text view.
pub fn search(ctx: &Ctx, spec: &TextSpec, vars: &[VarId]) -> Result<Table> {
    let snap = &ctx.snap;
    if snap.historical {
        return Err(Error::HistoryUnsupported(format!(
            "full-text search is only available at the head; this query reads commit {}",
            snap.commit
        )));
    }
    let Some(view) = &snap.text else {
        return Err(Error::invalid(
            "dataset has no full-text index; enable it with `sparkles text-index` or --text",
        ));
    };
    if view.seq != snap.commit {
        return Err(unavailable("dataset", "stale", view.seq, snap.commit));
    }
    let sh = &view.index;
    for p in &spec.predicates {
        if !sh.config.predicates.contains(p) {
            return Err(Error::invalid(format!(
                "text:query: <{p}> is not text-indexed"
            )));
        }
    }
    ctx.check()?;
    let resolved = view.resolved()?;
    // fuzzy terms expand against the searcher's terms
    let parsed = super::lucene::parse(&spec.query, &resolved.searcher, sh.fields.text)
        .map_err(|e| Error::invalid(format!("text:query: {e}")))?;
    let matchers = parsed.matchers;
    let Some(query) = scoped(snap, spec, sh.fields, parsed.query) else {
        return Ok(Table::empty(vars.to_vec()));
    };
    let out = Outputs::new(spec, vars, resolved, &sh.ids);
    let max = sh.config.max_hits;
    // A limit within maxHits keeps that many of the best hits. Without a limit, or with
    // one above maxHits, every hit is returned, and more than maxHits is an error.
    let mut t = match spec.limit.filter(|&n| n <= max) {
        Some(n) => {
            // with dedup (a merged default graph) or documents filtered out against the
            // snapshot, fetch more until enough hits remain
            let mut fetch = if spec.dedup { n.saturating_mul(2) } else { n }.saturating_add(1);
            loop {
                let hits = resolved
                    .searcher
                    .search(&query, &TopDocs::with_limit(fetch).order_by_score())
                    .map_err(text_err)?;
                ctx.check()?;
                let complete = hits.len() < fetch;
                ctx.check_output(hits.len().min(n), vars.len())?;
                let t = out.rows(ctx, resolved, &hits, n)?;
                if t.len() >= n || complete {
                    break t;
                }
                fetch = fetch.saturating_mul(2);
            }
        }
        None => {
            let (total, mut hits) = resolved
                .searcher
                .search(&query, &AllHits { cap: max })
                .map_err(text_err)?;
            ctx.check()?;
            if total > max as u64 {
                // more hits than a search may return without a limit
                return Err(Error::BudgetExceeded(crate::Budget {
                    kind: crate::BudgetKind::Rows,
                    limit: max as u64,
                    requested: total,
                }));
            }
            ctx.check_output(hits.len(), vars.len())?;
            // best first, as Tantivy's top-k orders them
            hits.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            out.rows(ctx, resolved, &hits, usize::MAX)?
        }
    };
    if let (Some(opts), Some(c)) = (&spec.highlight, out.clit) {
        // Jena's highlight: the literal output becomes its highlighted fragments
        let mut analyzer = resolved
            .searcher
            .index()
            .tokenizer_for_field(sh.fields.text)
            .map_err(text_err)?;
        let mut done: FxHashMap<Id, Id> = Default::default();
        for (i, id) in t.cols[c].iter_mut().enumerate() {
            if i % 4096 == 4095 {
                ctx.check()?;
            }
            if let Some(&h) = done.get(id) {
                *id = h;
                continue;
            }
            let h = match snap.term(*id) {
                Some(oxrdf::Term::Literal(l)) => {
                    match super::highlight::highlight(l.value(), opts, &matchers, &mut analyzer) {
                        Some(text) => {
                            let lit = match l.language() {
                                Some(lang) => {
                                    oxrdf::Literal::new_language_tagged_literal_unchecked(
                                        text, lang,
                                    )
                                }
                                None => oxrdf::Literal::new_typed_literal(text, l.datatype()),
                            };
                            ctx.intern_term(&oxrdf::Term::Literal(lit))
                        }
                        None => *id,
                    }
                }
                _ => *id,
            };
            done.insert(*id, h);
            *id = h;
        }
    }
    Ok(t)
}

/// The parsed query restricted to the call's predicates, language, subject and graphs;
/// `None` when nothing can match (a subject the index cannot hold).
fn scoped(
    snap: &Snapshot,
    spec: &TextSpec,
    f: super::imp::Fields,
    text: Box<dyn Query>,
) -> Option<Box<dyn Query>> {
    let mut filters: Vec<(Occur, Box<dyn Query>)> = Vec::new();
    let str_terms = |field: Field, vals: &mut dyn Iterator<Item = String>| -> Box<dyn Query> {
        Box::new(TermSetQuery::new(
            vals.map(|v| Term::from_field_text(field, &v)),
        ))
    };
    if !spec.predicates.is_empty() {
        filters.push((
            Occur::Must,
            str_terms(f.p, &mut spec.predicates.iter().cloned()),
        ));
    }
    if let Some(lang) = &spec.lang {
        filters.push((
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_text(f.lang, lang),
                IndexRecordOption::Basic,
            )),
        ));
    }
    if let PathEnd::Const(s) = &spec.subject {
        let k = term_key(snap, *s)?;
        filters.push((
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_bytes(f.s, &k),
                IndexRecordOption::Basic,
            )),
        ));
    }
    let default_term = || Term::from_field_text(f.g, DEFAULT_GRAPH_IRI);
    match &spec.graph {
        GraphFilter::All => {}
        GraphFilter::Default => filters.push((
            Occur::Must,
            Box::new(TermQuery::new(default_term(), IndexRecordOption::Basic)),
        )),
        GraphFilter::Named => filters.push((
            Occur::MustNot,
            Box::new(TermQuery::new(default_term(), IndexRecordOption::Basic)),
        )),
        GraphFilter::One(g) => {
            let names: Vec<String> = graph_name(snap, Id(*g)).into_iter().collect();
            filters.push((Occur::Must, str_terms(f.g, &mut names.into_iter())));
        }
        GraphFilter::Set(gs) => {
            let names: Vec<String> = gs.iter().filter_map(|g| graph_name(snap, Id(*g))).collect();
            filters.push((Occur::Must, str_terms(f.g, &mut names.into_iter())));
        }
    }
    if filters.is_empty() {
        return Some(text);
    }
    let mut must = vec![(Occur::Must, text)];
    let has_positive = filters.iter().any(|(o, _)| *o == Occur::Must);
    let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
    for (o, q) in filters {
        match o {
            Occur::Must => clauses.push((Occur::Must, q)),
            other => must.push((other, q)),
        }
    }
    if has_positive {
        // the filters match without adding to the score
        must.push((
            Occur::Must,
            Box::new(ConstScoreQuery::new(
                Box::new(BooleanQuery::new(clauses)),
                0.0,
            )),
        ));
    }
    Some(Box::new(BooleanQuery::new(must)))
}

/// Collects every hit with its score, keeping at most `cap` + 1 of them (more is an
/// error, so only the count matters then). Unlike a top-k collector it keeps no heap:
/// each hit is one push.
struct AllHits {
    cap: usize,
}

struct SegmentHits {
    seg: SegmentOrdinal,
    keep: usize,
    total: u64,
    hits: Vec<(Score, DocAddress)>,
}

impl Collector for AllHits {
    type Fruit = (u64, Vec<(Score, DocAddress)>);
    type Child = SegmentHits;

    fn for_segment(&self, seg: SegmentOrdinal, _: &SegmentReader) -> tantivy::Result<SegmentHits> {
        Ok(SegmentHits {
            seg,
            keep: self.cap.saturating_add(1),
            total: 0,
            hits: Vec::new(),
        })
    }

    fn requires_scoring(&self) -> bool {
        true
    }

    fn merge_fruits(
        &self,
        fruits: Vec<(u64, Vec<(Score, DocAddress)>)>,
    ) -> tantivy::Result<Self::Fruit> {
        let total = fruits.iter().map(|(n, _)| n).sum();
        let mut hits = Vec::with_capacity(fruits.iter().map(|(_, h)| h.len()).sum());
        for (_, h) in fruits {
            if total <= self.cap as u64 {
                hits.extend(h);
            }
        }
        Ok((total, hits))
    }
}

impl SegmentCollector for SegmentHits {
    type Fruit = (u64, Vec<(Score, DocAddress)>);

    fn collect(&mut self, doc: DocId, score: Score) {
        self.total += 1;
        if self.hits.len() < self.keep {
            self.hits.push((score, DocAddress::new(self.seg, doc)));
        }
    }

    fn harvest(self) -> Self::Fruit {
        (self.total, self.hits)
    }
}

/// Where a call's outputs go, and which of a hit's terms they need.
struct Outputs<'a> {
    spec: &'a TextSpec,
    vars: &'a [VarId],
    /// output columns of the subject, score, literal, graph slot, `GRAPH ?g` and property
    cs: Option<usize>,
    cscore: Option<usize>,
    clit: Option<usize>,
    cg_out: Option<usize>,
    cgv: Option<usize>,
    cprop: Option<usize>,
    need: Need,
    ids: &'a IdCache,
}

/// The columns a search reads. A document the searcher may hold for a quad the snapshot
/// lacks needs all four terms and the hash of its subject and object, to be checked.
#[derive(Clone, Copy)]
struct Need {
    s: bool,
    p: bool,
    o: bool,
    g: bool,
    hash: bool,
}

/// The terms of a list of hits (`Id::UNDEF` where a term is missing or not read).
#[derive(Default)]
struct HitTerms {
    s: Vec<Id>,
    p: Vec<Id>,
    o: Vec<Id>,
    g: Vec<Id>,
    /// see [`super::imp::doc_hash`] (only with `Need::hash`)
    hash: Vec<u64>,
}

impl<'a> Outputs<'a> {
    fn new(
        spec: &'a TextSpec,
        vars: &'a [VarId],
        resolved: &Resolved,
        ids: &'a IdCache,
    ) -> Outputs<'a> {
        let col = |v: Option<VarId>| v.and_then(|v| vars.iter().position(|x| *x == v));
        let cs = match spec.subject {
            PathEnd::Var(v) => col(Some(v)),
            PathEnd::Const(_) => None,
        };
        let (cscore, clit, cg_out, cgv, cprop) = (
            col(spec.score),
            col(spec.literal),
            col(spec.graph_out),
            col(spec.graph_var),
            col(spec.prop),
        );
        let hash = !resolved.uncertain.is_empty();
        let need = Need {
            s: cs.is_some() || spec.dedup || hash,
            p: cprop.is_some() || spec.dedup || hash,
            o: clit.is_some() || spec.dedup || hash,
            g: cg_out.is_some() || cgv.is_some() || hash,
            hash,
        };
        Outputs {
            spec,
            vars,
            cs,
            cscore,
            clit,
            cg_out,
            cgv,
            cprop,
            need,
            ids,
        }
    }

    /// The output rows of `hits` (best first), at most `want` of them.
    fn rows(
        &self,
        ctx: &Ctx,
        resolved: &Resolved,
        hits: &[(Score, DocAddress)],
        want: usize,
    ) -> Result<Table> {
        let snap = &ctx.snap;
        let need = self.need;
        let terms = hit_terms(ctx, resolved, self.ids, hits, need)?;
        let mut t = Table::new(self.vars.to_vec());
        let mut seen: FxHashSet<(Id, Id, Id)> = Default::default();
        let mut scores: FxHashMap<u32, Id> = Default::default();
        let mut default_graph = None;
        let mut stale_hits = 0usize;
        let mut row = vec![Id::UNDEF; self.vars.len()];
        for (i, (score, _)) in hits.iter().enumerate() {
            if i % 4096 == 4095 {
                ctx.check()?;
            }
            if t.len >= want {
                break;
            }
            let (s, p, o, g) = (terms.s[i], terms.p[i], terms.o[i], terms.g[i]);
            // a document the searcher may hold for a quad this snapshot does not have
            // (whose terms it may not have either)
            let uncertain = need.hash && resolved.uncertain.contains(&terms.hash[i]);
            let missing = (need.s && s == Id::UNDEF)
                || (need.p && p == Id::UNDEF)
                || (need.o && o == Id::UNDEF)
                || (need.g && g == Id::UNDEF);
            if missing {
                stale_hits += usize::from(!uncertain);
                continue;
            }
            if uncertain && !snap.contains(&[s, p, o, g])? {
                continue;
            }
            if self.spec.dedup && !seen.insert((s, p, o)) {
                continue;
            }
            row.fill(Id::UNDEF);
            if let Some(c) = self.cs {
                row[c] = s;
            }
            if let Some(c) = self.cscore {
                row[c] = *scores.entry(score.to_bits()).or_insert_with(|| {
                    ctx.intern_value(&crate::sparql::value::Value::Float((*score).into()))
                });
            }
            if let Some(c) = self.clit {
                row[c] = o;
            }
            if let Some(c) = self.cg_out {
                row[c] = graph_term(ctx, g, &mut default_graph);
            }
            if let Some(c) = self.cgv {
                if row[c] != Id::UNDEF && row[c] != g {
                    continue; // ?g used both as GRAPH ?g and as the graph slot
                }
                row[c] = g;
            }
            if let Some(c) = self.cprop {
                row[c] = p;
            }
            t.push_row(&row);
        }
        if stale_hits > 0 {
            tracing::warn!(
                "text:query skipped {stale_hits} hits whose terms are not in the snapshot"
            );
        }
        Ok(t)
    }
}

/// The graph slot's value for graph `g`: the default graph is named by its IRI.
fn graph_term(ctx: &Ctx, g: Id, default_graph: &mut Option<Id>) -> Id {
    if g != Id::DEFAULT_GRAPH {
        return g;
    }
    *default_graph.get_or_insert_with(|| {
        ctx.intern_term(&oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
            DEFAULT_GRAPH_IRI,
        )))
    })
}

/// The terms of `hits` that `need` asks for, segment by segment.
fn hit_terms(
    ctx: &Ctx,
    resolved: &Resolved,
    cache: &IdCache,
    hits: &[(Score, DocAddress)],
    need: Need,
) -> Result<HitTerms> {
    let snap = &ctx.snap;
    let n = hits.len();
    let mut out = HitTerms {
        s: vec![Id::UNDEF; n],
        p: vec![Id::UNDEF; n],
        o: vec![Id::UNDEF; n],
        g: vec![Id::UNDEF; n],
        hash: if need.hash { vec![0; n] } else { Vec::new() },
    };
    if !(need.s || need.p || need.o || need.g) {
        return Ok(out);
    }
    // hit positions in (segment, doc) order
    let mut order: Vec<u32> = (0..n as u32).collect();
    order.sort_unstable_by_key(|&i| hits[i as usize].1);
    let mut docs = Vec::new();
    let mut start = 0;
    while start < n {
        ctx.check()?;
        let seg = hits[order[start] as usize].1.segment_ord;
        let end = start
            + order[start..]
                .iter()
                .take_while(|&&i| hits[i as usize].1.segment_ord == seg)
                .count();
        let run = &order[start..end];
        docs.clear();
        docs.extend(run.iter().map(|&i| hits[i as usize].1.doc_id));
        let ff = resolved.searcher.segment_reader(seg).fast_fields();
        let bytes = |name: &str| -> Result<BytesColumn> {
            ff.bytes(name)
                .map_err(text_err)?
                .ok_or_else(|| text_err(format!("no {name} column")))
        };
        let strs = |name: &str| -> Result<BytesColumn> {
            Ok(ff
                .str(name)
                .map_err(text_err)?
                .ok_or_else(|| text_err(format!("no {name} column")))?
                .into())
        };
        let segment = resolved.searcher.segment_reader(seg).segment_id();
        // subjects and objects: from the id cache, or looked up and cached; with
        // documents to check against the snapshot, their keys are hashed too
        for (on, column, ids) in [
            (need.s, Column::S, &mut out.s),
            (need.o, Column::O, &mut out.o),
        ] {
            if !on {
                continue;
            }
            let col = bytes(column.name())?;
            let ords = Ords::of(&col, &docs);
            let lookup = |k: &[u8]| match column {
                Column::S => subject_id(snap, k),
                Column::O => snap.lookup_key(k).unwrap_or(Id::UNDEF),
            };
            if need.hash {
                let mut vals = Vec::with_capacity(ords.distinct.len());
                decode(&col, &ords.distinct, |k| {
                    let h = key_hash(k);
                    let h = if column == Column::O {
                        h.rotate_left(32)
                    } else {
                        h
                    };
                    vals.push((lookup(k), h));
                })?;
                for (&i, v) in run.iter().zip(ords.per_doc(&vals, (Id::UNDEF, 0))) {
                    ids[i as usize] = v.0;
                    out.hash[i as usize] ^= v.1;
                }
            } else {
                let key = (segment, column);
                let vals = cache.ids(snap.generation.uid, key, &ords.distinct, |missing| {
                    let mut v = Vec::with_capacity(missing.len());
                    decode(&col, missing, |k| v.push(lookup(k)))?;
                    Ok(v)
                })?;
                for (&i, id) in run.iter().zip(ords.per_doc(&vals, Id::UNDEF)) {
                    ids[i as usize] = id;
                }
            }
        }
        if need.p {
            let vals = per_doc(&strs("p")?, &docs, Id::UNDEF, |k| {
                std::str::from_utf8(k)
                    .ok()
                    .and_then(|p| snap.lookup_iri(p))
                    .unwrap_or(Id::UNDEF)
            })?;
            for (&i, id) in run.iter().zip(vals) {
                out.p[i as usize] = id;
            }
        }
        if need.g {
            let vals = per_doc(&strs("g")?, &docs, Id::UNDEF, |k| {
                std::str::from_utf8(k)
                    .ok()
                    .and_then(|g| graph_id(snap, g))
                    .unwrap_or(Id::UNDEF)
            })?;
            for (&i, id) in run.iter().zip(vals) {
                out.g[i as usize] = id;
            }
        }
        start = end;
    }
    Ok(out)
}

/// The term ordinals some documents have in a dictionary-encoded column.
struct Ords {
    /// the distinct ordinals, ascending
    distinct: Vec<u64>,
    /// each document's ordinal
    of_doc: Vec<Option<u64>>,
}

impl Ords {
    fn of(col: &BytesColumn, docs: &[DocId]) -> Ords {
        let mut of_doc = vec![None; docs.len()];
        col.ords().first_vals(docs, &mut of_doc);
        let mut distinct: Vec<u64> = of_doc.iter().flatten().copied().collect();
        distinct.sort_unstable();
        distinct.dedup();
        Ords { distinct, of_doc }
    }

    /// Each document's value, given the values of the distinct ordinals.
    fn per_doc<'a, T: Copy>(&'a self, vals: &'a [T], missing: T) -> impl Iterator<Item = T> + 'a {
        self.of_doc.iter().map(move |o| {
            o.and_then(|o| vals.get(self.distinct.binary_search(&o).ok()?).copied())
                .unwrap_or(missing)
        })
    }
}

/// Decode the terms of ascending ordinals, in order: the vocabulary's order too, so the
/// lookups that follow touch its blocks in order.
fn decode(col: &BytesColumn, ords: &[u64], mut f: impl FnMut(&[u8])) -> Result<()> {
    col.dictionary()
        .sorted_ords_to_term_cb(ords.iter().copied(), |k| {
            f(k);
            Ok(())
        })
        .map_err(text_err)?;
    Ok(())
}

/// The value of `f` for the term each of `docs` (ascending) has in a dictionary-encoded
/// column, or `missing`. Each distinct term is decoded and passed to `f` once.
fn per_doc<T: Copy>(
    col: &BytesColumn,
    docs: &[DocId],
    missing: T,
    mut f: impl FnMut(&[u8]) -> T,
) -> Result<Vec<T>> {
    let ords = Ords::of(col, docs);
    let mut vals = Vec::with_capacity(ords.distinct.len());
    decode(col, &ords.distinct, |k| vals.push(f(k)))?;
    Ok(ords.per_doc(&vals, missing).collect())
}

/// A column whose terms the id cache holds.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Column {
    S,
    O,
}

impl Column {
    fn name(self) -> &'static str {
        match self {
            Column::S => "s",
            Column::O => "o",
        }
    }
}

/// Entries the id cache holds at most (about 40 bytes each); it starts over beyond.
const ID_CACHE_MAX: usize = 1 << 21;

/// The ids of subject and object terms found before, by segment and term ordinal, so
/// that a term is looked up in the vocabulary once rather than at every search that hits
/// it. Ids hold within one store generation (compaction renumbers them), and a
/// generation's ids never change: a term is cached only once it is found, and a search
/// sees a cached id only for a document whose terms its snapshot has, or that it checks
/// against the snapshot.
#[derive(Default)]
pub(crate) struct IdCache(parking_lot::Mutex<IdCacheState>);

#[derive(Default)]
struct IdCacheState {
    /// the generation (`Generation::uid`) of the ids
    generation: u64,
    len: usize,
    map: FxHashMap<(tantivy::index::SegmentId, Column), FxHashMap<u64, Id>>,
}

impl IdCache {
    /// The ids of the distinct ascending ordinals `ords` of a segment's column: cached,
    /// or else from `lookup`, which gets the missing ordinals (ascending) and returns
    /// their ids in order (`Id::UNDEF` for a term not found).
    fn ids(
        &self,
        generation: u64,
        key: (tantivy::index::SegmentId, Column),
        ords: &[u64],
        lookup: impl FnOnce(&[u64]) -> Result<Vec<Id>>,
    ) -> Result<Vec<Id>> {
        let mut out = vec![Id::UNDEF; ords.len()];
        let mut missing = Vec::new();
        {
            let mut st = self.0.lock();
            if st.generation != generation {
                *st = IdCacheState {
                    generation,
                    ..Default::default()
                };
            }
            match st.map.get(&key) {
                Some(m) => {
                    for (i, o) in ords.iter().enumerate() {
                        match m.get(o) {
                            Some(&id) => out[i] = id,
                            None => missing.push(i),
                        }
                    }
                }
                None => missing.extend(0..ords.len()),
            }
        }
        if missing.is_empty() {
            return Ok(out);
        }
        let want: Vec<u64> = missing.iter().map(|&i| ords[i]).collect();
        let found = lookup(&want)?;
        let mut st = self.0.lock();
        if st.generation == generation && want.len() <= ID_CACHE_MAX {
            if st.len + want.len() > ID_CACHE_MAX {
                st.map.clear();
                st.len = 0;
            }
            let mut added = 0;
            let m = st.map.entry(key).or_default();
            for (o, &id) in want.iter().zip(&found) {
                if id != Id::UNDEF && m.insert(*o, id).is_none() {
                    added += 1;
                }
            }
            st.len += added;
        }
        drop(st);
        for (i, id) in missing.into_iter().zip(found) {
            out[i] = id;
        }
        Ok(out)
    }
}

/// The id of a document's subject key (`_` and the big-endian id for a blank node).
fn subject_id(snap: &Snapshot, k: &[u8]) -> Id {
    match k.split_first() {
        Some((b'_', rest)) if rest.len() == 8 => {
            Id::bnode(u64::from_be_bytes(rest.try_into().unwrap()))
        }
        _ => snap.lookup_key(k).unwrap_or(Id::UNDEF),
    }
}

/// The id of a document's graph name.
fn graph_id(snap: &Snapshot, g: &str) -> Option<Id> {
    if g == DEFAULT_GRAPH_IRI {
        Some(Id::DEFAULT_GRAPH)
    } else if let Some(label) = g.strip_prefix("_:") {
        crate::store::parse_bnode_label(label)
    } else {
        snap.lookup_iri(g)
    }
}
