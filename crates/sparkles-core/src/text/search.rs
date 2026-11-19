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
    search_in(ctx, spec, vars, None, false)
}

/// [`search`] restricted to hits whose subject is one of `subjects`, when given (the
/// subjects of a join's other side). The restriction is a filter of the search, so it
/// changes no score. With `lenient`, a query string that does not parse matches
/// nothing instead of failing the query (a value bound by the data, not a constant).
pub fn search_in(
    ctx: &Ctx,
    spec: &TextSpec,
    vars: &[VarId],
    subjects: Option<&[Id]>,
    lenient: bool,
) -> Result<Table> {
    let snap = &ctx.snap;
    let Some(Prepared {
        sh,
        resolved,
        field,
        matchers,
        query,
    }) = prepare(ctx, spec, subjects, lenient)?
    else {
        return Ok(Table::empty(vars.to_vec()));
    };
    let out = Outputs::new(spec, vars, resolved, &sh.ids, snap.mask.is_some());
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
                let mut t = Table::new(vars.to_vec());
                let terms = out.terms(ctx, resolved, &hits)?;
                let mut st = RowState::default();
                out.rows(ctx, resolved, &hits, &terms, 0, n, &mut t, &mut st)?;
                st.finish();
                if t.len() >= n || complete {
                    break t;
                }
                fetch = fetch.saturating_mul(2);
            }
        }
        None => {
            let (total, mut hits, _) = resolved
                .searcher
                .search(
                    &query,
                    &AllHits {
                        cap: max,
                        reserve: None,
                    },
                )
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
            sort_hits(&mut hits);
            let mut t = Table::new(vars.to_vec());
            let terms = out.terms(ctx, resolved, &hits)?;
            let mut st = RowState::default();
            out.rows(ctx, resolved, &hits, &terms, 0, usize::MAX, &mut t, &mut st)?;
            st.finish();
            t
        }
    };
    if let (Some(opts), Some(c)) = (&spec.highlight, out.clit) {
        highlight(
            ctx,
            resolved,
            field,
            opts,
            &matchers,
            &mut t.cols[c],
            usize::MAX,
        )?;
    }
    Ok(t)
}

/// What a search needs once its call is checked against the view and its query parsed.
struct Prepared<'a> {
    sh: &'a std::sync::Arc<super::imp::Shared>,
    resolved: &'a Resolved,
    field: Field,
    matchers: super::lucene::Matchers,
    query: Box<dyn Query>,
}

/// Check a call against the snapshot's text view and parse its query, or `None` when
/// nothing can match (a query string that does not parse, with `lenient`).
fn prepare<'a>(
    ctx: &'a Ctx,
    spec: &TextSpec,
    subjects: Option<&[Id]>,
    lenient: bool,
) -> Result<Option<Prepared<'a>>> {
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
    if view.uncommitted {
        return Err(super::uncommitted_changes());
    }
    let ready = view.ready(snap.commit)?;
    if view.seq != snap.commit {
        return Err(unavailable("dataset", "stale", view.seq, snap.commit));
    }
    let sh = &ready.index;
    for p in &spec.predicates {
        if !sh.config.predicates.contains(p) {
            return Err(Error::invalid(format!(
                "text:query: <{p}> is not text-indexed"
            )));
        }
    }
    ctx.check()?;
    let resolved = view.resolved()?;
    // a language with an analyzer searches its stemmed text; fuzzy terms expand against
    // the searcher's terms of that field
    let (field, analyzer) = sh.fields.text_for(spec.lang.as_deref());
    let parsed = match super::lucene::parse(&spec.query, &resolved.searcher, field, analyzer) {
        Ok(p) => p,
        Err(_) if lenient => return Ok(None),
        Err(e) => return Err(Error::invalid(format!("text:query: {e}"))),
    };
    let Some(query) = scoped(snap, spec, &sh.fields, parsed.query, subjects) else {
        return Ok(None);
    };
    Ok(Some(Prepared {
        sh,
        resolved,
        field,
        matchers: parsed.matchers,
        query,
    }))
}

/// The hits a reader decodes at a time.
const PAGE: usize = 4096;

/// What decoding a page holds per hit at most at once: its four terms and hash (40
/// bytes), its position in document order and its document (12, with growth), a
/// column's ordinal per document and the distinct ordinals collected with growth (32),
/// the values of those ordinals (16), and the id cache's lookup of the missing ones,
/// their positions, ordinals, ids and the cached ids (48, with growth). The buffer that
/// a term's key is decoded into is Tantivy's, outside this count (row 1156).
const PAGE_HIT_BYTES: u64 = 192;

/// The literals a reader's highlighting remembers at most, and what that map holds.
const HIGHLIGHT_DONE: usize = PAGE;
const HIGHLIGHT_DONE_BYTES: u64 = (HIGHLIGHT_DONE * 8 / 7).next_power_of_two() as u64
    * (std::mem::size_of::<(Id, Id)>() as u64 + 1);

/// The bytes a hit list keeps per hit, a score and a document address.
const HIT_BYTES: u64 = std::mem::size_of::<(Score, DocAddress)>() as u64;

/// A text search without a limit, read a page of hits at a time (item 829).
///
/// Its hits are bounded by the index's `maxHits`, as an eager search's are, so it is a
/// leaf under decision 2 of the X14 plan. The reader counts the hits first, without
/// scoring them, and refuses more than `maxHits` before it collects any, with the
/// error an eager search gives. It then holds the scored hit list, best first, under a
/// charge, and decodes the terms of one page of hits at a time into rows, so what it
/// holds between pulls does not grow with the width of a row. The triples a merged
/// default graph has seen are kept across pages, in a set reserved and charged for the
/// whole hit list at the start.
pub(crate) struct Reader {
    spec: TextSpec,
    vars: Vec<VarId>,
    /// The searcher that the hits' document addresses belong to.
    resolved: Resolved,
    sh: std::sync::Arc<super::imp::Shared>,
    field: Field,
    matchers: super::lucene::Matchers,
    hits: Vec<(Score, DocAddress)>,
    /// the first hit of the decoded page, and its terms
    page: Option<(usize, HitTerms)>,
    /// the next hit to read
    at: usize,
    st: RowState,
}

impl Reader {
    /// Run the search of a call without a limit and keep its hits, or `None` when nothing
    /// can match. `charge` sets the bytes the reader holds, before it allocates them.
    pub(crate) fn open(
        ctx: &Ctx,
        spec: &TextSpec,
        vars: &[VarId],
        charge: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<Option<Reader>> {
        debug_assert!(
            spec.limit.is_none(),
            "a reader serves a search without a limit"
        );
        let Some(Prepared {
            sh,
            resolved,
            field,
            matchers,
            query,
        }) = prepare(ctx, spec, None, false)?
        else {
            return Ok(None);
        };
        let max = sh.config.max_hits;
        let counts = resolved
            .searcher
            .search(&query, &SegmentCounts)
            .map_err(text_err)?;
        ctx.check()?;
        let total: u64 = counts.iter().sum();
        if total > max as u64 {
            return Err(Error::BudgetExceeded(crate::Budget {
                kind: crate::BudgetKind::Rows,
                limit: max as u64,
                requested: total,
            }));
        }
        // the row limits an eager search checks; the reader holds no table of the rows
        ctx.check_output_rows(total as usize)?;
        let mut page = (total as usize).min(PAGE) as u64 * PAGE_HIT_BYTES;
        if spec.highlight.is_some() {
            page += HIGHLIGHT_DONE_BYTES;
        }
        let seen = if spec.dedup {
            seen_bytes(total as usize)
        } else {
            0
        };
        // the segments' lists and the merged one, until the merge ends
        charge(
            (2 * total * HIT_BYTES)
                .saturating_add(page)
                .saturating_add(seen),
        )?;
        let (collected, mut hits, agreed) = resolved
            .searcher
            .search(
                &query,
                &AllHits {
                    cap: max,
                    reserve: Some(counts),
                },
            )
            .map_err(text_err)?;
        ctx.check()?;
        if !agreed || collected != total {
            return Err(text_err(format!(
                "text:query counted {total} hits and collected {collected}"
            )));
        }
        charge(
            (hits.capacity() as u64 * HIT_BYTES)
                .saturating_add(page)
                .saturating_add(seen),
        )?;
        sort_hits(&mut hits);
        let mut st = RowState::default();
        if spec.dedup {
            st.seen.reserve(hits.len());
        }
        Ok(Some(Reader {
            spec: spec.clone(),
            vars: vars.to_vec(),
            resolved: resolved.clone(),
            sh: sh.clone(),
            field,
            matchers,
            hits,
            page: None,
            at: 0,
            st,
        }))
    }

    /// The hits not read yet, which bound the rows still to come.
    pub(crate) fn remaining(&self) -> usize {
        self.hits.len() - self.at
    }

    /// Append the next rows to `t` until it holds `cap` of them, and return whether the
    /// hits have ended.
    pub(crate) fn next(&mut self, ctx: &Ctx, cap: usize, t: &mut Table) -> Result<bool> {
        let out = Outputs::new(
            &self.spec,
            &self.vars,
            &self.resolved,
            &self.sh.ids,
            ctx.snap.mask.is_some(),
        );
        let first = t.len();
        loop {
            if t.len() >= cap || self.at >= self.hits.len() {
                break;
            }
            let start = match &self.page {
                Some((start, terms)) if self.at < start + terms.len() => *start,
                _ => {
                    self.page = None;
                    let end = self.hits.len().min(self.at + PAGE);
                    let terms = out.terms(ctx, &self.resolved, &self.hits[self.at..end])?;
                    self.page = Some((self.at, terms));
                    self.at
                }
            };
            let (_, terms) = self.page.as_ref().expect("a decoded page");
            let hits = &self.hits[start..start + terms.len()];
            let next = out.rows(
                ctx,
                &self.resolved,
                hits,
                terms,
                self.at - start,
                cap - t.len(),
                t,
                &mut self.st,
            )?;
            self.at = start + next;
        }
        let ended = self.at >= self.hits.len();
        if let (Some(opts), Some(c)) = (&self.spec.highlight, out.clit) {
            highlight(
                ctx,
                &self.resolved,
                self.field,
                opts,
                &self.matchers,
                &mut t.cols[c][first..],
                HIGHLIGHT_DONE,
            )?;
        }
        if ended {
            self.st.finish();
            self.page = None;
        }
        Ok(ended)
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        // the hits skipped before a consumer stopped early
        self.st.finish();
    }
}

/// The bytes of a set of `n` triples reserved at once, as `hashbrown` allocates it.
fn seen_bytes(n: usize) -> u64 {
    if n == 0 {
        return 0;
    }
    let buckets = if n < 4 {
        4
    } else if n < 8 {
        8
    } else {
        (n.saturating_mul(8) / 7).next_power_of_two()
    };
    let entry = std::mem::size_of::<(Id, Id, Id)>() + 1;
    (buckets * entry + 16) as u64
}

/// Counts the hits of each segment, without scoring them.
struct SegmentCounts;

struct SegmentCount {
    seg: SegmentOrdinal,
    n: u64,
}

impl Collector for SegmentCounts {
    /// the hits of each segment, by segment ordinal
    type Fruit = Vec<u64>;
    type Child = SegmentCount;

    fn for_segment(&self, seg: SegmentOrdinal, _: &SegmentReader) -> tantivy::Result<SegmentCount> {
        Ok(SegmentCount { seg, n: 0 })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(&self, fruits: Vec<(SegmentOrdinal, u64)>) -> tantivy::Result<Vec<u64>> {
        let len = fruits
            .iter()
            .map(|(s, _)| *s as usize + 1)
            .max()
            .unwrap_or(0);
        let mut counts = vec![0; len];
        for (s, n) in fruits {
            counts[s as usize] += n;
        }
        Ok(counts)
    }
}

impl SegmentCollector for SegmentCount {
    type Fruit = (SegmentOrdinal, u64);

    fn collect(&mut self, _: DocId, _: Score) {
        self.n += 1;
    }

    fn harvest(self) -> Self::Fruit {
        (self.seg, self.n)
    }
}

/// Order hits best first, as Tantivy's top-k orders them.
fn sort_hits(hits: &mut [(Score, DocAddress)]) {
    hits.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
}

/// Jena's highlight: each literal of `col` becomes its highlighted fragments. A literal
/// is highlighted once per call, and a literal interns to the same id at every call.
fn highlight(
    ctx: &Ctx,
    resolved: &Resolved,
    field: Field,
    opts: &super::HighlightOpts,
    matchers: &super::lucene::Matchers,
    col: &mut [Id],
    remember: usize,
) -> Result<()> {
    let snap = &ctx.snap;
    let mut analyzer = resolved
        .searcher
        .index()
        .tokenizer_for_field(field)
        .map_err(text_err)?;
    let mut done: FxHashMap<Id, Id> = Default::default();
    for (i, id) in col.iter_mut().enumerate() {
        if i % 4096 == 4095 {
            ctx.check()?;
        }
        if let Some(&h) = done.get(id) {
            *id = h;
            continue;
        }
        let h = match snap.term(*id) {
            Some(oxrdf::Term::Literal(l)) => {
                match super::highlight::highlight(l.value(), opts, matchers, &mut analyzer) {
                    Some(text) => {
                        let lit = match l.language() {
                            Some(lang) => {
                                oxrdf::Literal::new_language_tagged_literal_unchecked(text, lang)
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
        if done.len() >= remember {
            // a literal highlighted again interns to the same id
            done.clear();
        }
        done.insert(*id, h);
        *id = h;
    }
    Ok(())
}

/// The parsed query restricted to the call's predicates, language, subject, the given
/// subjects and graphs; `None` when nothing can match (a subject the index cannot hold).
fn scoped(
    snap: &Snapshot,
    spec: &TextSpec,
    f: &super::imp::Fields,
    text: Box<dyn Query>,
    subjects: Option<&[Id]>,
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
    if let Some(ids) = subjects {
        // literals and other terms that cannot be subjects have no key
        let keys: Vec<Term> = ids
            .iter()
            .filter_map(|&id| term_key(snap, id))
            .map(|k| Term::from_field_bytes(f.s, &k))
            .collect();
        if keys.is_empty() {
            return None;
        }
        filters.push((Occur::Must, Box::new(TermSetQuery::new(keys))));
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
    /// the hits of each segment, when they were counted first: a segment's list is
    /// allocated at its count and never holds more
    reserve: Option<Vec<u64>>,
}

struct SegmentHits {
    seg: SegmentOrdinal,
    keep: usize,
    total: u64,
    hits: Vec<(Score, DocAddress)>,
}

impl Collector for AllHits {
    /// the hits counted, those kept, and whether each segment's count is the one
    /// reserved for it (always, without a reservation)
    type Fruit = (u64, Vec<(Score, DocAddress)>, bool);
    type Child = SegmentHits;

    fn for_segment(&self, seg: SegmentOrdinal, _: &SegmentReader) -> tantivy::Result<SegmentHits> {
        let keep = self.cap.saturating_add(1);
        Ok(match &self.reserve {
            Some(counts) => {
                let n = counts
                    .get(seg as usize)
                    .map_or(0, |&n| n as usize)
                    .min(keep);
                SegmentHits {
                    seg,
                    keep: n,
                    total: 0,
                    hits: Vec::with_capacity(n),
                }
            }
            None => SegmentHits {
                seg,
                keep,
                total: 0,
                hits: Vec::new(),
            },
        })
    }

    fn requires_scoring(&self) -> bool {
        true
    }

    fn merge_fruits(
        &self,
        fruits: Vec<(SegmentOrdinal, u64, Vec<(Score, DocAddress)>)>,
    ) -> tantivy::Result<Self::Fruit> {
        let agreed = self.reserve.as_ref().is_none_or(|counts| {
            let mut seen = vec![0u64; counts.len()];
            fruits.iter().all(|(seg, n, _)| {
                let at = *seg as usize;
                at < counts.len() && {
                    seen[at] += n;
                    true
                }
            }) && seen == *counts
        });
        let total = fruits.iter().map(|(_, n, _)| n).sum();
        let mut hits = Vec::with_capacity(fruits.iter().map(|(_, _, h)| h.len()).sum());
        for (_, _, h) in fruits {
            if total <= self.cap as u64 {
                hits.extend(h);
            }
        }
        Ok((total, hits, agreed))
    }
}

impl SegmentCollector for SegmentHits {
    type Fruit = (SegmentOrdinal, u64, Vec<(Score, DocAddress)>);

    fn collect(&mut self, doc: DocId, score: Score) {
        self.total += 1;
        if self.hits.len() < self.keep {
            self.hits.push((score, DocAddress::new(self.seg, doc)));
        }
    }

    fn harvest(self) -> Self::Fruit {
        (self.seg, self.total, self.hits)
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
    crank: Option<usize>,
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

impl HitTerms {
    /// The hits whose terms these are.
    fn len(&self) -> usize {
        self.s.len()
    }
}

impl<'a> Outputs<'a> {
    fn new(
        spec: &'a TextSpec,
        vars: &'a [VarId],
        resolved: &Resolved,
        ids: &'a IdCache,
        masked: bool,
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
        let crank = col(spec.rank);
        let hash = !resolved.uncertain.is_empty();
        // a view that hides triples checks every hit's quad against its mask
        let all = hash || masked;
        let need = Need {
            s: cs.is_some() || spec.dedup || all,
            p: cprop.is_some() || spec.dedup || all,
            o: clit.is_some() || spec.dedup || all,
            g: cg_out.is_some() || cgv.is_some() || all,
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
            crank,
            need,
            ids,
        }
    }

    /// The terms of `hits` that the outputs need.
    fn terms(
        &self,
        ctx: &Ctx,
        resolved: &Resolved,
        hits: &[(Score, DocAddress)],
    ) -> Result<HitTerms> {
        hit_terms(ctx, resolved, self.ids, hits, self.need)
    }

    /// Append the output rows of `hits[from..]` (best first) to `t`, at most `want` of
    /// them, and return the position of the first hit not read. `terms` are the terms of
    /// `hits`, and `st` carries what a row depends on from the hits before `from`.
    #[allow(clippy::too_many_arguments)]
    fn rows(
        &self,
        ctx: &Ctx,
        resolved: &Resolved,
        hits: &[(Score, DocAddress)],
        terms: &HitTerms,
        from: usize,
        want: usize,
        t: &mut Table,
        st: &mut RowState,
    ) -> Result<usize> {
        let snap = &ctx.snap;
        let need = self.need;
        // a score interns to the same id at every call
        let mut scores: FxHashMap<u32, Id> = Default::default();
        let mut row = vec![Id::UNDEF; self.vars.len()];
        let mut added = 0;
        let mut i = from;
        while i < hits.len() {
            if (i - from) % 4096 == 4095 {
                ctx.check()?;
            }
            if added >= want {
                break;
            }
            let score = hits[i].0;
            let (s, p, o, g) = (terms.s[i], terms.p[i], terms.o[i], terms.g[i]);
            let hash = if need.hash { terms.hash[i] } else { 0 };
            i += 1;
            // a document the searcher may hold for a quad this snapshot does not have
            // (whose terms it may not have either)
            let uncertain = need.hash && resolved.uncertain.contains(&hash);
            let missing = (need.s && s == Id::UNDEF)
                || (need.p && p == Id::UNDEF)
                || (need.o && o == Id::UNDEF)
                || (need.g && g == Id::UNDEF);
            if missing {
                st.stale_hits += usize::from(!uncertain);
                continue;
            }
            if uncertain && !snap.contains(&[s, p, o, g])? {
                continue;
            }
            if snap
                .mask
                .as_ref()
                .is_some_and(|m| m.hides(&[s.0, p.0, o.0, g.0]))
            {
                continue;
            }
            if self.spec.dedup && !st.dedup(s, p, o) {
                continue;
            }
            row.fill(Id::UNDEF);
            if let Some(c) = self.cs {
                row[c] = s;
            }
            if let Some(c) = self.cscore {
                row[c] = *scores.entry(score.to_bits()).or_insert_with(|| {
                    ctx.intern_value(&crate::sparql::value::Value::Float(score.into()))
                });
            }
            if let Some(c) = self.clit {
                row[c] = o;
            }
            if let Some(c) = self.cg_out {
                row[c] = graph_term(ctx, g, &mut st.default_graph);
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
            if let Some(c) = self.crank {
                let r = match st.rank {
                    Some((bits, r)) if bits == score.to_bits() => r,
                    _ => st.emitted + 1,
                };
                st.rank = Some((score.to_bits(), r));
                row[c] = Id::from_i64(r as i64).expect("a rank fits an inline integer");
            }
            t.push_row(&row);
            added += 1;
            st.emitted += 1;
        }
        Ok(i)
    }
}

/// What a row depends on from the hits before it, kept across the calls that decode one
/// hit list: the triples seen (with a merged default graph), the rank of the last score,
/// and the rows emitted so far.
#[derive(Default)]
struct RowState {
    seen: FxHashSet<(Id, Id, Id)>,
    default_graph: Option<Id>,
    stale_hits: usize,
    /// the rank of the rows with the last score: tied rows share the first one's
    rank: Option<(u32, usize)>,
    emitted: usize,
}

impl RowState {
    /// Whether the triple is new.
    fn dedup(&mut self, s: Id, p: Id, o: Id) -> bool {
        self.seen.insert((s, p, o))
    }

    /// Report the hits skipped because their terms are not in the snapshot.
    fn finish(&mut self) {
        let stale_hits = std::mem::take(&mut self.stale_hits);
        if stale_hits > 0 {
            tracing::warn!(
                target: "sparkles::text::search",
                "text:query skipped {stale_hits} hits whose terms are not in the snapshot"
            );
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fruit(seg: SegmentOrdinal, n: u64) -> (SegmentOrdinal, u64, Vec<(Score, DocAddress)>) {
        let hits = (0..n as u32)
            .map(|d| (1.0, DocAddress::new(seg, d)))
            .collect();
        (seg, n, hits)
    }

    /// A segment whose collected hits differ from its count is reported, so a reader
    /// fails rather than answer a different set of hits.
    #[test]
    fn a_collect_that_disagrees_with_the_count_is_reported() {
        let counted = AllHits {
            cap: 10,
            reserve: Some(vec![2, 1]),
        };
        let agreed = |fruits| counted.merge_fruits(fruits).unwrap().2;
        assert!(agreed(vec![fruit(0, 2), fruit(1, 1)]));
        assert!(!agreed(vec![fruit(0, 2), fruit(1, 2)]));
        assert!(!agreed(vec![fruit(0, 1), fruit(1, 1)]));
        assert!(!agreed(vec![fruit(0, 2)]));
        assert!(!agreed(vec![fruit(0, 2), fruit(1, 1), fruit(2, 1)]));
        let plain = AllHits {
            cap: 10,
            reserve: None,
        };
        assert!(plain.merge_fruits(vec![fruit(0, 3)]).unwrap().2);
    }
}
