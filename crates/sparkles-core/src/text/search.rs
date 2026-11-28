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
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::Arc;
use tantivy::collector::{Collector, SegmentCollector, TopDocs};
use tantivy::columnar::BytesColumn;
use tantivy::query::{
    Bm25StatisticsProvider, BooleanQuery, ConstScoreQuery, EnableScoring, Occur, Query, TermQuery,
    TermSetQuery,
};
use tantivy::schema::{Field, IndexRecordOption};
use tantivy::{
    DocAddress, DocId, DocSet, Score, Searcher, SegmentOrdinal, SegmentReader, TERMINATED, Term,
};

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
    if let Some(base) = snap
        .text
        .as_ref()
        .and_then(|view| view.overlay_base.as_deref())
    {
        return search_adjusted(ctx, spec, vars, subjects, lenient, base);
    }
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
    if snap.historical && snap.text.is_none() {
        return Err(Error::HistoryUnsupported(format!(
            "historical full-text search needs the current generation or an indexed named snapshot; this query reads commit {}",
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

/// A transaction or historical query searches the committed corpus and its private
/// overlay with one query and one set of BM25 statistics.
struct Adjusted<'a> {
    sh: &'a Arc<super::imp::Shared>,
    resolved: Resolved,
    field: Field,
    matchers: super::lucene::Matchers,
    query: Box<dyn Query>,
}

fn prepare_adjusted<'a>(
    ctx: &Ctx,
    spec: &TextSpec,
    subjects: Option<&[Id]>,
    lenient: bool,
    base: &'a Snapshot,
    charge: &mut dyn FnMut(u64) -> Result<()>,
) -> Result<Option<Adjusted<'a>>> {
    ctx.check()?;
    let view = base.text.as_ref().expect("adjustment has a base text view");
    let sh = &view.ready(base.commit)?.index;
    if view.seq != base.commit {
        return Err(unavailable("dataset", "stale", view.seq, base.commit));
    }
    for p in &spec.predicates {
        if !sh.config.predicates.contains(p) {
            return Err(Error::invalid(format!(
                "text:query: <{p}> is not text-indexed"
            )));
        }
    }
    let mut resolved = view.resolved()?.clone();
    resolved.overlay = super::imp::build_overlay(ctx, base, sh, &resolved.searcher, charge)?;
    resolved.adjusted = true;
    resolved.uncertain = Arc::new(FxHashSet::default());
    let (field, analyzer) = sh.fields.text_for(spec.lang.as_deref());
    let stopped = std::cell::RefCell::new(None);
    let check = || {
        ctx.check().map_err(|error| {
            let message = error.to_string();
            *stopped.borrow_mut() = Some(error);
            message
        })
    };
    let parsed = super::lucene::parse_adjusted(
        &spec.query,
        &resolved.searcher,
        field,
        analyzer,
        resolved.overlay.as_ref(),
        Some(&check),
    );
    if let Some(error) = stopped.into_inner() {
        return Err(error);
    }
    let parsed = match parsed {
        Ok(p) => p,
        Err(_) if lenient => return Ok(None),
        Err(e) => return Err(Error::invalid(format!("text:query: {e}"))),
    };
    let Some(query) = scoped(&ctx.snap, spec, &sh.fields, parsed.query, subjects) else {
        return Ok(None);
    };
    Ok(Some(Adjusted {
        sh,
        resolved,
        field,
        matchers: parsed.matchers,
        query,
    }))
}

/// An empty committed corpus or field needs a positive average field length so
/// matching documents of a newly indexed term get finite scores.
struct CommittedStats<'a>(&'a Searcher);

impl Bm25StatisticsProvider for CommittedStats<'_> {
    fn total_num_tokens(&self, field: Field) -> tantivy::Result<u64> {
        let tokens = self.0.total_num_tokens(field)?;
        if tokens == 0 {
            self.total_num_docs()
        } else {
            Ok(tokens)
        }
    }

    fn total_num_docs(&self) -> tantivy::Result<u64> {
        Ok(self.0.total_num_docs()?.max(1))
    }

    fn doc_freq(&self, term: &Term) -> tantivy::Result<u64> {
        self.0.doc_freq(term)
    }
}

#[derive(Clone, Copy)]
struct RankedHit(Score, DocAddress);

impl Ord for RankedHit {
    fn cmp(&self, other: &Self) -> Ordering {
        other.0.total_cmp(&self.0).then(self.1.cmp(&other.1))
    }
}

impl PartialOrd for RankedHit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for RankedHit {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for RankedHit {}

fn adjusted_hit_bytes(count: usize, bytes: u64) -> Result<u64> {
    (count as u64).checked_mul(bytes).ok_or_else(|| {
        Error::BudgetExceeded(crate::Budget {
            kind: crate::BudgetKind::Memory,
            limit: u64::MAX,
            requested: u64::MAX,
        })
    })
}

impl Adjusted<'_> {
    /// Collect only actual visible hits. The committed searcher can retain later
    /// additions and earlier removals from its batch, so a hit must belong to the
    /// committed state as well as to the read's target state.
    fn hits(
        &self,
        ctx: &Ctx,
        base: &Snapshot,
        top: Option<usize>,
        charge: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<Vec<(Score, DocAddress)>> {
        const CHECK_PAGE: usize = 128;
        let _page_charge = ctx.charge(CHECK_PAGE as u64 * PAGE_HIT_BYTES)?;
        let resolved = &self.resolved;
        let snap = &ctx.snap;
        let stats = CommittedStats(&resolved.searcher);
        let mut page = Vec::with_capacity(CHECK_PAGE);
        let mut hits = BinaryHeap::new();
        let max = self.sh.config.max_hits;
        let cap = top.unwrap_or(max);
        let mut admit = |hit: RankedHit| -> Result<()> {
            if hits.len() == cap {
                if top.is_none() {
                    return Err(Error::BudgetExceeded(crate::Budget {
                        kind: crate::BudgetKind::Rows,
                        limit: max as u64,
                        requested: (max as u64)
                            .checked_add(1)
                            .ok_or_else(|| text_err("hit count overflow"))?,
                    }));
                }
                if let Some(mut worst) = hits.peek_mut()
                    && hit < *worst
                {
                    *worst = hit;
                }
                return Ok(());
            }
            if hits.len() == hits.capacity() {
                let old = hits.capacity();
                let new = old.checked_mul(2).unwrap_or(cap).max(GROW).min(cap);
                let overlapping = old
                    .checked_add(new)
                    .ok_or_else(|| text_err("hit capacity overflow"))?;
                charge(adjusted_hit_bytes(overlapping, HIT_BYTES)?)?;
                hits.reserve_exact(new - old);
                charge(adjusted_hit_bytes(hits.capacity(), HIT_BYTES)?)?;
            }
            hits.push(hit);
            Ok(())
        };
        let mut flush = |page: &mut Vec<(Score, DocAddress)>, committed| -> Result<()> {
            ctx.check()?;
            let terms = hit_terms(
                ctx,
                resolved,
                &self.sh.ids,
                page,
                Need {
                    s: true,
                    p: true,
                    o: true,
                    g: true,
                    hash: false,
                },
            )?;
            for (i, &(score, address)) in page.iter().enumerate() {
                ctx.check()?;
                let quad = [terms.s[i], terms.p[i], terms.o[i], terms.g[i]];
                if quad.contains(&Id::UNDEF)
                    || (committed && !base.contains(&quad)?)
                    || !snap.contains(&quad)?
                    || snap
                        .mask
                        .as_ref()
                        .is_some_and(|m| m.hides(&quad.map(|id| id.0)))
                {
                    continue;
                }
                admit(RankedHit(score, address))?;
            }
            page.clear();
            Ok(())
        };
        let mut segment = 0u32;
        for (searcher, committed) in std::iter::once((&resolved.searcher, true))
            .chain(resolved.overlay.as_ref().map(|overlay| (overlay, false)))
        {
            ctx.check()?;
            let weight = self
                .query
                .weight(EnableScoring::enabled_from_statistics_provider(
                    &stats, searcher,
                ))
                .map_err(text_err)?;
            for reader in searcher.segment_readers() {
                ctx.check()?;
                let mut scorer = weight.scorer(reader, 1.0).map_err(text_err)?;
                let alive = reader.alive_bitset();
                let mut doc = scorer.doc();
                let mut visited = 0u64;
                while doc != TERMINATED {
                    if visited.is_multiple_of(CHECK_PAGE as u64) {
                        ctx.check()?;
                    }
                    visited += 1;
                    if alive.is_none_or(|set| set.is_alive(doc)) {
                        page.push((scorer.score(), DocAddress::new(segment, doc)));
                        if page.len() == CHECK_PAGE {
                            flush(&mut page, committed)?;
                        }
                    }
                    doc = scorer.advance();
                }
                if !page.is_empty() {
                    flush(&mut page, committed)?;
                }
                segment = segment
                    .checked_add(1)
                    .ok_or_else(|| text_err("segment count overflow"))?;
            }
        }
        ctx.check()?;
        // Converting the retained heap to the reader's ordered list holds two buffers.
        let buffers = hits
            .capacity()
            .checked_add(hits.len())
            .ok_or_else(|| text_err("hit capacity overflow"))?;
        charge(adjusted_hit_bytes(buffers, HIT_BYTES)?)?;
        let hits: Vec<_> = hits
            .into_sorted_vec()
            .into_iter()
            .map(|h| (h.0, h.1))
            .collect();
        charge(adjusted_hit_bytes(hits.capacity(), HIT_BYTES)?)?;
        Ok(hits)
    }
}

fn search_adjusted(
    ctx: &Ctx,
    spec: &TextSpec,
    vars: &[VarId],
    subjects: Option<&[Id]>,
    lenient: bool,
    base: &Snapshot,
) -> Result<Table> {
    let overlay = ctx.charge(0)?;
    let Some(prepared) = prepare_adjusted(ctx, spec, subjects, lenient, base, &mut |n| {
        overlay.resize(n)
    })?
    else {
        return Ok(Table::empty(vars.to_vec()));
    };
    let out = Outputs::new(
        spec,
        vars,
        &prepared.resolved,
        &prepared.sh.ids,
        ctx.snap.mask.is_some(),
    );
    let max = prepared.sh.config.max_hits;
    let limit = spec.limit.filter(|&n| n <= max);
    let mut fetch = limit;
    let mut table = loop {
        let hit_charge = ctx.charge(0)?;
        let hits = prepared.hits(ctx, base, fetch, &mut |n| hit_charge.resize(n))?;
        let wanted = limit.unwrap_or(hits.len());
        ctx.check_output(hits.len().min(wanted), vars.len())?;
        let decode_count = if spec.dedup {
            hits.len()
        } else {
            hits.len().min(wanted)
        };
        let _decode = ctx.charge(adjusted_hit_bytes(decode_count, PAGE_HIT_BYTES + 48)?)?;
        let hits = &hits[..decode_count];
        let terms = out.terms(ctx, &prepared.resolved, hits)?;
        let mut table = Table::new(vars.to_vec());
        let mut state = RowState::default();
        out.rows(
            ctx,
            &prepared.resolved,
            hits,
            &terms,
            0,
            wanted,
            &mut table,
            &mut state,
        )?;
        state.finish();
        if table.len() >= wanted || fetch.is_none_or(|fetch| hits.len() < fetch) {
            break table;
        }
        fetch = fetch.and_then(|n| n.checked_mul(2));
        if fetch.is_none() {
            return Err(text_err("hit capacity overflow"));
        }
    };
    if let (Some(opts), Some(c)) = (&spec.highlight, out.clit) {
        highlight(
            ctx,
            &prepared.resolved,
            prepared.field,
            opts,
            &prepared.matchers,
            &mut table.cols[c],
            usize::MAX,
        )?;
    }
    Ok(table)
}

/// The hits a reader decodes at a time.
const PAGE: usize = 4096;

/// What decoding a page holds per hit at most at once: its four terms and hash (40
/// bytes), its position in document order and its document (12, with growth), a
/// column's ordinal per document and the distinct ordinals collected with growth (32),
/// the values of those ordinals (16), and the id cache's lookup of the missing ones,
/// their positions, ordinals, ids and the cached ids (48, with growth). The buffer that
/// a term's key is decoded into is Tantivy's, outside this count (item 1191).
const PAGE_HIT_BYTES: u64 = 192;

/// The hits a reader's list first holds, and the least it grows by.
const GROW: usize = 1024;

/// The bytes a hit list keeps per hit, a score and a document address.
const HIT_BYTES: u64 = std::mem::size_of::<(Score, DocAddress)>() as u64;

/// A text search without a limit, read a page of hits at a time (item 829).
///
/// Its hits are bounded by the index's `maxHits`, as an eager search's are, so it is a
/// leaf under decision 2 of the X14 plan. The reader collects the scored hits in one
/// pass, as an eager search does, into a list charged before each growth, and refuses
/// more than `maxHits` with the error an eager search gives. It then holds the hit
/// list, best first, and decodes the terms of one page of hits at a time into rows, so
/// what it holds between pulls does not grow with the width of a row. The triples a merged
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
        if let Some(base) = ctx
            .snap
            .text
            .as_ref()
            .and_then(|view| view.overlay_base.as_deref())
        {
            return Self::open_adjusted(ctx, spec, vars, base, charge);
        }
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
        let searcher = &resolved.searcher;
        // One scored pass over the segments in order, as `Searcher::search` runs it on
        // the index's single-thread executor: the same weight, scoring from the
        // searcher's statistics, and only the documents that are alive. The list grows
        // under a charge taken before each growth, for the old list and the new one
        // while the hits move. Past `maxHits`, or once a charge is refused, the pass
        // drops the list and its charge and only counts, so the errors come in eager's
        // order. Cancellation is checked between segments, as an eager search checks it
        // after its whole search.
        let weight = query
            .weight(EnableScoring::enabled_from_searcher(searcher))
            .map_err(text_err)?;
        let mut hits: Vec<(Score, DocAddress)> = Vec::new();
        let mut total = 0u64;
        let mut refused = None;
        for (seg, reader) in searcher.segment_readers().iter().enumerate() {
            let seg = seg as SegmentOrdinal;
            let alive = reader.alive_bitset();
            weight
                .for_each(reader, &mut |doc, score| {
                    if alive.is_some_and(|a| !a.is_alive(doc)) {
                        return;
                    }
                    total += 1;
                    if refused.is_some() || total > max as u64 {
                        if hits.capacity() > 0 {
                            // the list is dropped and its charge with it; a smaller
                            // charge is never refused
                            hits = Vec::new();
                            let _ = charge(0);
                        }
                        return;
                    }
                    if hits.len() == hits.capacity() {
                        // the list holds fewer than `maxHits` hits, so it can grow
                        let old = hits.capacity();
                        let new = old.saturating_mul(2).max(GROW).min(max);
                        let grown = charge((old + new) as u64 * HIT_BYTES).map(|()| {
                            hits.reserve_exact(new - old);
                        });
                        if let Err(e) =
                            grown.and_then(|()| charge(hits.capacity() as u64 * HIT_BYTES))
                        {
                            refused = Some(e);
                            hits = Vec::new();
                            let _ = charge(0);
                            return;
                        }
                    }
                    hits.push((score, DocAddress::new(seg, doc)));
                })
                .map_err(text_err)?;
            ctx.check()?;
        }
        if total > max as u64 {
            return Err(Error::BudgetExceeded(crate::Budget {
                kind: crate::BudgetKind::Rows,
                limit: max as u64,
                requested: total,
            }));
        }
        // the row limits an eager search checks; the reader holds no table of the rows
        ctx.check_output_rows(total as usize)?;
        if let Some(e) = refused {
            return Err(e);
        }
        let page = (total as usize).min(PAGE) as u64 * PAGE_HIT_BYTES;
        let seen = if spec.dedup {
            map_bytes(total as usize, std::mem::size_of::<(Id, Id, Id)>())
        } else {
            0
        };
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

    fn open_adjusted(
        ctx: &Ctx,
        spec: &TextSpec,
        vars: &[VarId],
        base: &Snapshot,
        charge: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<Option<Reader>> {
        let mut overlay_bytes = 0;
        let Some(prepared) = prepare_adjusted(ctx, spec, None, false, base, &mut |n| {
            charge(n)?;
            overlay_bytes = n;
            Ok(())
        })?
        else {
            return Ok(None);
        };
        let mut adjust_charge = |n: u64| {
            charge(
                overlay_bytes
                    .checked_add(n)
                    .ok_or_else(|| text_err("text memory size overflow"))?,
            )
        };
        let hits = prepared.hits(ctx, base, None, &mut adjust_charge)?;
        ctx.check_output_rows(hits.len())?;
        let hit_bytes = adjusted_hit_bytes(hits.capacity(), HIT_BYTES)?;
        let page = adjusted_hit_bytes(hits.len().min(PAGE), PAGE_HIT_BYTES)?;
        let seen = if spec.dedup {
            adjusted_hit_bytes(hits.len(), 64)?
        } else {
            0
        };
        let bytes = hit_bytes
            .checked_add(page)
            .and_then(|n| n.checked_add(seen))
            .ok_or_else(|| text_err("text memory size overflow"))?;
        adjust_charge(bytes)?;
        let mut st = RowState::default();
        if spec.dedup {
            st.seen.reserve(hits.len());
        }
        Ok(Some(Self {
            spec: spec.clone(),
            vars: vars.to_vec(),
            sh: prepared.sh.clone(),
            resolved: prepared.resolved,
            field: prepared.field,
            matchers: prepared.matchers,
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
                // no map of the literals highlighted, whose size a batch would set
                0,
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

/// The bytes of a hash table of `n` entries of `entry` bytes reserved at once, as
/// `hashbrown` allocates it.
fn map_bytes(n: usize, entry: usize) -> u64 {
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
    (buckets * (entry + 1) + 16) as u64
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
    let mut analyzer = resolved
        .searcher
        .index()
        .tokenizer_for_field(field)
        .map_err(text_err)?;
    let done_charge = ctx.charge(0)?;
    let mut done: FxHashMap<Id, Id> = Default::default();
    for (i, id) in col.iter_mut().enumerate() {
        if i % 4096 == 4095 {
            ctx.check()?;
        }
        if let Some(&h) = done.get(id) {
            *id = h;
            continue;
        }
        let decode = ctx.charge(0)?;
        let h = match ctx.term_checked_into(*id, &decode)? {
            Some(oxrdf::Term::Literal(l)) => {
                match super::highlight::highlight(l.value(), opts, matchers, &mut analyzer) {
                    Some(text) => {
                        decode.add(text.len() as u64 + 64)?;
                        let lit = match l.language() {
                            Some(lang) => {
                                oxrdf::Literal::new_language_tagged_literal_unchecked(text, lang)
                            }
                            None => oxrdf::Literal::new_typed_literal(text, l.datatype()),
                        };
                        let interned = ctx.intern_term(&oxrdf::Term::Literal(lit));
                        drop(l);
                        drop(decode);
                        ctx.check()?;
                        interned
                    }
                    None => *id,
                }
            }
            _ => *id,
        };
        // a literal highlighted again interns to the same id
        if remember > 0 {
            if done.len() >= remember {
                done.clear();
            } else {
                done_charge.add(32)?;
            }
            done.insert(*id, h);
        }
        *id = h;
    }
    ctx.check()?;
    drop(done);
    drop(done_charge);
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
        GraphFilter::Set(_) | GraphFilter::PreparedSet(_) => {
            let names: Vec<String> = spec
                .graph
                .set_ids()
                .unwrap()
                .iter()
                .filter_map(|g| graph_name(snap, Id(*g)))
                .collect();
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
    /// the hits counted, and those kept
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
        fruits: Vec<(SegmentOrdinal, u64, Vec<(Score, DocAddress)>)>,
    ) -> tantivy::Result<Self::Fruit> {
        let total = fruits.iter().map(|(_, n, _)| n).sum();
        let mut hits = Vec::with_capacity(fruits.iter().map(|(_, _, h)| h.len()).sum());
        for (_, _, h) in fruits {
            if total <= self.cap as u64 {
                hits.extend(h);
            }
        }
        Ok((total, hits))
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
        let ff = resolved.segment(seg).fast_fields();
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
        let segment = resolved.segment(seg).segment_id();
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
            } else if resolved.adjusted {
                let mut vals = Vec::with_capacity(ords.distinct.len());
                decode(&col, &ords.distinct, |k| vals.push(lookup(k)))?;
                for (&i, id) in run.iter().zip(ords.per_doc(&vals, Id::UNDEF)) {
                    ids[i as usize] = id;
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
