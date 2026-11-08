//! One admitted compact response, with only one row of transient cell text.
use super::{
    bounded::{TermFormat, term_text},
    *,
};
use oxrdf::{NamedOrBlankNodeRef, TermRef};
use serde::ser::{Error as _, SerializeMap, SerializeSeq};
use sparkles::{
    Error, Result,
    sparql::output::{NativeBudget, NativeReservation, OwnedJson, OwnedText},
};
use std::{cell::RefCell, fmt, io};

pub(in crate::mcp) enum CompactPage {
    Table(OwnedText),
    Json(OwnedJson),
}
impl CompactPage {
    pub(super) fn as_str(&self) -> &str {
        match self {
            Self::Table(text) => text.as_str(),
            Self::Json(json) => json.as_str(),
        }
    }
}

struct Workspace<T> {
    values: Vec<T>,
    _credit: NativeReservation,
}
impl<T> Workspace<T> {
    fn new(count: usize, budget: &NativeBudget) -> Result<Self> {
        let bytes = std::mem::size_of::<T>()
            .checked_mul(count)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| Error::invalid("compact page workspace overflow"))?;
        let credit = budget.reserve(bytes)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| Error::invalid("compact page workspace allocation failed"))?;
        if std::mem::size_of::<T>() != 0 && values.capacity() != count {
            return Err(Error::invalid("compact page workspace exceeds admission"));
        }
        Ok(Self {
            values,
            _credit: credit,
        })
    }
}

fn width(page: &QueryPage<'_>) -> usize {
    match &page.rows {
        RowSource::Solutions(rows) => rows.width(),
        RowSource::Triples(_) => 3,
    }
}
fn subject(term: NamedOrBlankNodeRef<'_>) -> TermRef<'_> {
    match term {
        NamedOrBlankNodeRef::NamedNode(node) => TermRef::NamedNode(node),
        NamedOrBlankNodeRef::BlankNode(node) => TermRef::BlankNode(node),
    }
}
fn visit_row<'a>(
    page: &'a QueryPage<'_>,
    index: usize,
    mut visit: impl FnMut(Option<TermRef<'a>>) -> Result<()>,
) -> Result<()> {
    match &page.rows {
        RowSource::Solutions(rows) => {
            for cell in rows.row(index)?.iter() {
                visit(cell)?;
            }
        }
        RowSource::Triples(rows) => {
            let row = &rows[index];
            visit(Some(subject(row.subject.as_ref())))?;
            visit(Some(TermRef::NamedNode(row.predicate.as_ref())))?;
            visit(Some(row.object.as_ref()))?;
        }
    }
    Ok(())
}
fn cells(
    page: &QueryPage<'_>,
    index: usize,
    prefixes: &Prefixes,
    budget: &NativeBudget,
) -> Result<Workspace<Option<OwnedText>>> {
    let mut row = Workspace::new(width(page), budget)?;
    visit_row(page, index, |term| {
        row.values.push(
            term.map(|term| term_text(term, prefixes, page.max_chars, budget))
                .transpose()?,
        );
        Ok(())
    })?;
    Ok(row)
}

// Observe the same borrowed lexical walk used by TermFormat. It never copies a
// term or an inherited namespace map, including for deeply nested RDF-star.
fn iri_stats(
    iri: &str,
    maximum: usize,
    prefixes: &Prefixes,
    flags: &mut [u8],
    budget: &NativeBudget,
) -> Result<usize> {
    let mut count = 0usize;
    for _ in iri.chars() {
        if count.is_multiple_of(1024) {
            budget.check()?;
        }
        count = count
            .checked_add(1)
            .ok_or_else(|| Error::invalid("compact term size overflow"))?;
    }
    if count > maximum {
        return Ok(1);
    }
    for (i, (ns, _)) in prefixes.by_ns.iter().enumerate() {
        budget.check()?;
        if iri.strip_prefix(ns.as_str()).is_some_and(valid_local) {
            flags[i] = 1;
            break;
        }
    }
    Ok(0)
}
fn term_stats(
    mut term: TermRef<'_>,
    maximum: usize,
    prefixes: &Prefixes,
    flags: &mut [u8],
    budget: &NativeBudget,
) -> Result<usize> {
    let mut shortened = 0;
    loop {
        budget.check()?;
        match term {
            TermRef::NamedNode(node) => {
                shortened += iri_stats(node.as_str(), maximum, prefixes, flags, budget)?
            }
            TermRef::BlankNode(_) => {}
            TermRef::Literal(literal) => {
                let mut count = 0usize;
                for _ in literal.value().chars() {
                    if count.is_multiple_of(1024) {
                        budget.check()?;
                    }
                    count = count
                        .checked_add(1)
                        .ok_or_else(|| Error::invalid("compact literal size overflow"))?;
                }
                shortened += usize::from(count > maximum);
                let datatype = literal.datatype();
                let dt = datatype.as_str();
                if literal.language().is_none()
                    && !matches!(dt, XSD_STRING | RDF_LANG_STRING | RDF_DIR_LANG_STRING)
                {
                    // Bare canonical numbers do not render their datatype.
                    let bare = count <= maximum
                        && match dt {
                            XSD_INTEGER => canonical_integer(literal.value()),
                            XSD_DECIMAL => canonical_decimal(literal.value()),
                            XSD_BOOLEAN => matches!(literal.value(), "true" | "false"),
                            _ => false,
                        };
                    if !bare {
                        iri_stats(dt, usize::MAX, prefixes, flags, budget)?;
                    }
                }
            }
            TermRef::Triple(triple) => {
                if let NamedOrBlankNode::NamedNode(node) = &triple.subject {
                    shortened += iri_stats(node.as_str(), maximum, prefixes, flags, budget)?;
                }
                shortened +=
                    iri_stats(triple.predicate.as_str(), maximum, prefixes, flags, budget)?;
                term = triple.object.as_ref();
                continue;
            }
        }
        return Ok(shortened);
    }
}
fn observe(
    page: &QueryPage<'_>,
    row: usize,
    prefixes: &Prefixes,
    flags: &mut [u8],
    budget: &NativeBudget,
) -> Result<usize> {
    let mut shortened = 0usize;
    visit_row(page, row, |term| {
        if let Some(term) = term {
            shortened = shortened
                .checked_add(term_stats(term, page.max_chars, prefixes, flags, budget)?)
                .ok_or_else(|| Error::invalid("compact shortened count overflow"))?;
        }
        Ok(())
    })?;
    Ok(shortened)
}

// Preserve a typed admission/control failure across serde/fmt's secondary error.
fn finish<T>(
    budget: &NativeBudget,
    first: &RefCell<Option<Error>>,
    result: Result<T>,
) -> Result<T> {
    budget.check()?;
    match first.borrow_mut().take() {
        Some(error) => Err(error),
        None => result,
    }
}
fn failed(first: &RefCell<Option<Error>>, error: Error) {
    let mut slot = first.borrow_mut();
    if slot.is_none() {
        *slot = Some(error);
    }
}
struct Counter<'a> {
    bytes: usize,
    budget: &'a NativeBudget,
    first: &'a RefCell<Option<Error>>,
}
impl Counter<'_> {
    fn add(&mut self, bytes: usize) -> Result<()> {
        self.budget.check()?;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|n| *n <= isize::MAX as usize)
            .ok_or_else(|| Error::invalid("compact document size overflow"))?;
        Ok(())
    }
}
impl io::Write for Counter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Err(error) = self.add(bytes.len()) {
            failed(self.first, error);
            return Err(io::Error::other("compact document counting failed"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl fmt::Write for Counter<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.add(value.len()).map_err(|error| {
            failed(self.first, error);
            fmt::Error
        })
    }
}
fn json_size(
    value: &impl Serialize,
    budget: &NativeBudget,
    first: &RefCell<Option<Error>>,
) -> Result<usize> {
    let mut counter = Counter {
        bytes: 0,
        budget,
        first,
    };
    let result = serde_json::to_writer(&mut counter, value)
        .map_err(|_| Error::invalid("compact JSON counting failed"));
    finish(budget, first, result)?;
    Ok(counter.bytes)
}

struct PrefixMap<'a> {
    prefixes: &'a Prefixes,
    flags: &'a [u8],
    budget: &'a NativeBudget,
    first: &'a RefCell<Option<Error>>,
}
impl PrefixMap<'_> {
    // The existing vector is namespace-ordered. Select the next name without a
    // second vector/map of cloned names or an unadmitted sorting workspace.
    fn in_name_order(&self, mut visit: impl FnMut(&str, &str) -> fmt::Result) -> fmt::Result {
        let mut previous: Option<&str> = None;
        loop {
            let mut next: Option<&(String, String)> = None;
            for (index, entry) in self.prefixes.by_ns.iter().enumerate() {
                self.budget.check().map_err(|error| {
                    failed(self.first, error);
                    fmt::Error
                })?;
                if self.flags[index] != 0
                    && previous.is_none_or(|p| entry.1.as_str() > p)
                    && next.is_none_or(|old| entry.1 < old.1)
                {
                    next = Some(entry);
                }
            }
            let Some((ns, name)) = next else {
                return Ok(());
            };
            visit(name, ns)?;
            previous = Some(name);
        }
    }
}
impl Serialize for PrefixMap<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let mut map =
            serializer.serialize_map(Some(self.flags.iter().filter(|&&v| v != 0).count()))?;
        let mut first = None;
        self.in_name_order(|name, ns| {
            map.serialize_entry(name, ns).map_err(|error| {
                first = Some(error);
                fmt::Error
            })
        })
        .map_err(|_| {
            first.unwrap_or_else(|| S::Error::custom("compact prefixes control failed"))
        })?;
        map.end()
    }
}
struct Rows<'a> {
    page: &'a QueryPage<'a>,
    prefixes: &'a Prefixes,
    count: usize,
    budget: &'a NativeBudget,
    first: &'a RefCell<Option<Error>>,
}
impl Serialize for Rows<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let mut rows = serializer.serialize_seq(Some(self.count))?;
        for index in 0..self.count {
            let row = cells(self.page, index, self.prefixes, self.budget).map_err(|error| {
                failed(self.first, error);
                S::Error::custom("compact row admission failed")
            })?;
            rows.serialize_element(&row.values)?;
        }
        rows.end()
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PageDoc<'a> {
    dataset: &'a str,
    commit: u64,
    query_type: &'static str,
    vars: JsonVars<'a>,
    rows: Rows<'a>,
    total: Option<usize>,
    offset: usize,
    returned: usize,
    truncated: Option<Truncated>,
    terms_shortened: usize,
    prefixes: PrefixMap<'a>,
    elapsed_ms: f64,
}
fn document<'a>(
    page: &'a QueryPage<'a>,
    prefixes: &'a Prefixes,
    flags: &'a [u8],
    count: usize,
    shortened: usize,
    truncated: Option<Reason>,
    empty: bool,
    budget: &'a NativeBudget,
    first: &'a RefCell<Option<Error>>,
) -> PageDoc<'a> {
    PageDoc {
        dataset: page.dataset,
        commit: page.commit,
        query_type: page.query_type,
        vars: page.vars.map_or(JsonVars::Triples, JsonVars::Solutions),
        rows: Rows {
            page,
            prefixes,
            count: if empty { 0 } else { count },
            budget,
            first,
        },
        total: page.total.filter(|_| !page.total_is_lower_bound),
        offset: page.offset,
        returned: count,
        truncated: truncated.map(|reason| Truncated {
            reason,
            next: Next {
                offset: page.offset + count,
                at_commit: page.commit,
            },
        }),
        terms_shortened: shortened,
        prefixes: PrefixMap {
            prefixes,
            flags,
            budget,
            first,
        },
        elapsed_ms: page.elapsed_ms,
    }
}

struct Table<'a> {
    page: &'a QueryPage<'a>,
    prefixes: &'a Prefixes,
    flags: &'a [u8],
    count: usize,
    shortened: usize,
    truncated: Option<Reason>,
    empty: bool,
    budget: &'a NativeBudget,
    first: &'a RefCell<Option<Error>>,
}
impl Table<'_> {
    fn write(&self, out: &mut impl fmt::Write) -> fmt::Result {
        let page = self.page;
        write!(out, "# {} · ", page.query_type)?;
        if self.count > 0 {
            write!(out, "rows {}–{}", page.offset + 1, page.offset + self.count)?;
        } else {
            out.write_str("rows 0")?;
        }
        out.write_str(" of ")?;
        match page.total {
            Some(total) if page.total_is_lower_bound => write!(out, "≥{total}")?,
            Some(total) => write!(out, "{total}")?,
            None => out.write_char('?')?,
        }
        if let Some(reason) = self.truncated {
            let cap = match reason {
                Reason::MaxRows => page.max_rows,
                Reason::MaxBytes => page.max_bytes,
            };
            write!(out, " (TRUNCATED: {}={cap})", reason.name())?;
        }
        write!(out, " · commit {}", page.commit)?;
        if self.shortened > 0 {
            write!(out, " · {} terms shortened", self.shortened)?;
        }
        PrefixMap {
            prefixes: self.prefixes,
            flags: self.flags,
            budget: self.budget,
            first: self.first,
        }
        .in_name_order(|name, ns| {
            write!(out, "\nPREFIX {name}: <")?;
            TermFormat {
                term: TermRef::Literal(oxrdf::LiteralRef::new_simple_literal("")),
                prefixes: self.prefixes,
                max_chars: page.max_chars,
                budget: self.budget,
            }
            .escape(out, ns)?;
            out.write_char('>')
        })?;
        if let Some(vars) = page.vars {
            out.write_char('\n')?;
            for (i, name) in vars.iter().enumerate() {
                if i > 0 {
                    out.write_char('\t')?;
                }
                write!(out, "?{name}")?;
            }
        }
        if !self.empty {
            for index in 0..self.count {
                let row = cells(page, index, self.prefixes, self.budget).map_err(|error| {
                    failed(self.first, error);
                    fmt::Error
                })?;
                out.write_char('\n')?;
                write_row(out, &row.values, page.vars.is_some())?;
            }
        }
        match self.truncated {
            None => {}
            Some(Reason::MaxBytes) if self.count == 0 => write!(
                out,
                "\n# more: the next row alone exceeds maxBytes={}; lower maxTermChars or select fewer variables",
                page.max_bytes
            )?,
            Some(_) => write!(
                out,
                "\n# more: call sparql_query with the same query, offset={}, atCommit={}",
                page.offset + self.count,
                page.commit
            )?,
        }
        Ok(())
    }
}
impl fmt::Display for Table<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write(out)
    }
}
fn write_row(
    out: &mut impl fmt::Write,
    cells: &[Option<OwnedText>],
    solutions: bool,
) -> fmt::Result {
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            out.write_char(if solutions { '\t' } else { ' ' })?;
        }
        if let Some(cell) = cell {
            out.write_str(cell.as_str())?;
        }
    }
    if !solutions {
        out.write_str(" .")?;
    }
    Ok(())
}

pub(in crate::mcp) fn owned_page(
    page: &QueryPage<'_>,
    prefixes: &Prefixes,
    json: bool,
    budget: &NativeBudget,
) -> Result<CompactPage> {
    budget.check()?;
    let first = RefCell::new(None);
    let mut flags = Workspace::new(prefixes.by_ns.len(), budget)?;
    flags.values.resize(prefixes.by_ns.len(), 0);
    let n = page.rows.len();
    let mut count = 0;
    let mut shortened = 0;
    let mut row_bytes = 0usize;
    let state = |count| {
        if count < n {
            Some(Reason::MaxBytes)
        } else if page.more_beyond {
            Some(Reason::MaxRows)
        } else {
            None
        }
    };
    let mut truncated = state(0);
    for index in 0..n {
        budget.check()?;
        shortened += observe(page, index, prefixes, &mut flags.values, budget)?;
        let row = cells(page, index, prefixes, budget)?;
        let bytes = if json {
            json_size(&row.values, budget, &first)?
        } else {
            let mut counter = Counter {
                bytes: 0,
                budget,
                first: &first,
            };
            let result = write_row(&mut counter, &row.values, page.vars.is_some())
                .map_err(|_| Error::invalid("compact table row counting failed"));
            finish(budget, &first, result)?;
            counter.bytes + 1
        };
        // Row workspace is retired before final allocation; prefix flags remain.
        drop(row);
        row_bytes = row_bytes
            .checked_add(bytes)
            .ok_or_else(|| Error::invalid("compact rows size overflow"))?;
        let k = index + 1;
        let reason = state(k);
        let metadata = if json {
            json_size(
                &document(
                    page,
                    prefixes,
                    &flags.values,
                    k,
                    shortened,
                    reason,
                    true,
                    budget,
                    &first,
                ),
                budget,
                &first,
            )? + k.saturating_sub(1)
        } else {
            let mut counter = Counter {
                bytes: 0,
                budget,
                first: &first,
            };
            let table = Table {
                page,
                prefixes,
                flags: &flags.values,
                count: k,
                shortened,
                truncated: reason,
                empty: true,
                budget,
                first: &first,
            };
            let result = table
                .write(&mut counter)
                .map_err(|_| Error::invalid("compact table metadata counting failed"));
            finish(budget, &first, result)?;
            counter.bytes
        };
        if metadata
            .checked_add(row_bytes)
            .is_none_or(|bytes| bytes > page.max_bytes)
        {
            truncated = Some(Reason::MaxBytes);
            // Only the rejected row's prefix/shortening contribution is removed.
            flags.values.fill(0);
            shortened = 0;
            for accepted in 0..count {
                shortened += observe(page, accepted, prefixes, &mut flags.values, budget)?;
            }
            break;
        }
        count = k;
        truncated = reason;
    }
    if json {
        let result = budget.json(&document(
            page,
            prefixes,
            &flags.values,
            count,
            shortened,
            truncated,
            false,
            budget,
            &first,
        ));
        finish(budget, &first, result).map(CompactPage::Json)
    } else {
        let result = budget.format_text(&Table {
            page,
            prefixes,
            flags: &flags.values,
            count,
            shortened,
            truncated,
            empty: false,
            budget,
            first: &first,
        });
        finish(budget, &first, result).map(CompactPage::Table)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles::{
        access::Preparation,
        sparql::{self, QueryKind, QueryOptions, pool::QueryPool},
        store::{Store, StoreOptions},
        task::Control,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn owned_compact_pages_match_legacy_wire_caps_and_zero_width_shape() {
        let store = Store::in_memory(StoreOptions::default());
        let prefixes = Prefixes::new(&BTreeMap::from([
            ("ex".into(), "http://ex.org/".into()),
            ("exa".into(), "http://ex.org/a/".into()),
            ("xsd".into(), "http://www.w3.org/2001/XMLSchema#".into()),
        ]));
        for query in [
            "PREFIX ex:<http://ex.org/> PREFIX exa:<http://ex.org/a/> SELECT ?s ?n ?u WHERE { VALUES (?s ?n ?u) { (ex:a 42 UNDEF) (exa:b 5 \"long λ value\\nline\") (<http://ex.org/a.> 1.5 \"last\"@en) } }",
            "SELECT * WHERE {}",
            "SELECT * WHERE { FILTER(false) }",
            "CONSTRUCT { ?s <http://ex.org/p> ?o } WHERE { VALUES (?s ?o) { (<http://ex.org/alice> \"λ long value\") (<http://ex.org/a/b> 42) } }",
        ] {
            let result = sparql::query(store.snapshot(), query, &QueryOptions::default()).unwrap();
            let budget = result.output_budget().native_budget().unwrap();
            for limit in [0, 1, 2, 100] {
                for maximum in [0, 1, 16, 500] {
                    for cap in [1, 100, 250, 512, 4096] {
                        for lower in [false, true] {
                            let end = result.len().min(limit);
                            let page = QueryPage {
                                dataset: "actual",
                                commit: 9,
                                query_type: if result.kind == QueryKind::Select {
                                    "SELECT"
                                } else {
                                    "CONSTRUCT"
                                },
                                vars: (result.kind == QueryKind::Select)
                                    .then(|| result.variables()),
                                rows: if result.kind == QueryKind::Select {
                                    RowSource::Solutions(result.rows_range(0..end).unwrap())
                                } else {
                                    RowSource::Triples(&result.triples()[..end])
                                },
                                more_beyond: result.len() > end,
                                total: Some(result.len()),
                                total_is_lower_bound: lower,
                                offset: 17,
                                max_rows: limit,
                                max_bytes: cap,
                                max_chars: maximum,
                                elapsed_ms: 0.125,
                            };
                            for json in [false, true] {
                                let expected = if json {
                                    super::super::json(&page, &prefixes)
                                } else {
                                    super::super::table(&page, &prefixes)
                                };
                                let actual = owned_page(&page, &prefixes, json, &budget).unwrap();
                                assert_eq!(
                                    actual.as_str(),
                                    expected,
                                    "json={json} cap={cap} maxChars={maximum} limit={limit} lower={lower} query={query}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn compact_page_real_refusal_and_final_clone_preserve_original_receiver() {
        let store = Store::in_memory(StoreOptions::default());
        let query = format!(
            "SELECT ?v WHERE {{ VALUES ?v {{ \"{}\" }} }}",
            "λ".repeat(64 << 10)
        );
        let result = sparql::query(store.snapshot(), &query, &QueryOptions::default()).unwrap();
        let page = QueryPage {
            dataset: "physical",
            commit: 3,
            query_type: "SELECT",
            vars: Some(result.variables()),
            rows: RowSource::Solutions(result.rows().unwrap()),
            more_beyond: false,
            total: Some(1),
            total_is_lower_bound: false,
            offset: 0,
            max_rows: 1,
            max_bytes: 1 << 20,
            max_chars: usize::MAX,
            elapsed_ms: 0.0,
        };
        let prefixes = Prefixes::new(&BTreeMap::new());
        let pool = QueryPool::new(16 << 20);
        let grant = Arc::new(pool.empty_grant());
        let cancel = Arc::new(AtomicBool::new(false));
        let preparation =
            Preparation::with_grant(Control::with_cancel(cancel.clone()), 4 << 20, grant.clone())
                .unwrap();
        let budget = NativeBudget::from_preparation(&preparation).unwrap();
        let before = grant.used();
        let compact = owned_page(&page, &prefixes, true, &budget).unwrap();
        let CompactPage::Json(json) = compact else {
            panic!("compact JSON owner");
        };
        let last = json.clone();
        assert_eq!(json.as_str().as_ptr(), last.as_str().as_ptr());
        let held = grant.used();
        assert!(held >= before + json.as_str().len() as u64);
        drop(json);
        cancel.store(true, Ordering::SeqCst);
        assert!(matches!(
            owned_page(&page, &prefixes, false, &budget),
            Err(Error::Cancelled)
        ));
        assert_eq!(grant.used(), held);
        drop(budget);
        drop(preparation);
        assert!(grant.used() >= last.as_str().len() as u64);
        assert!(last.as_str().contains("λλλλ"));
        drop(last);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
        let small = Preparation::new(Control::default(), 4096).unwrap();
        let small = NativeBudget::from_preparation(&small).unwrap();
        for json in [false, true] {
            assert!(
                matches!(owned_page(&page, &prefixes, json, &small), Err(Error::BudgetExceeded(b))
                if b.kind == sparkles::BudgetKind::Memory && b.limit == 4096)
            );
        }
    }

    #[test]
    fn compact_triple_page_walks_deep_borrowed_terms_on_small_stack() {
        use oxrdf::{Literal, NamedNode, NamedOrBlankNode, Triple};
        let mut object = Term::Literal(Literal::new_simple_literal("leaf"));
        for _ in 0..256 {
            object = Term::Triple(Box::new(Triple {
                subject: NamedOrBlankNode::NamedNode(NamedNode::new_unchecked("urn:s")),
                predicate: NamedNode::new_unchecked("urn:p"),
                object,
            }));
        }
        let triples = [Triple {
            subject: NamedOrBlankNode::NamedNode(NamedNode::new_unchecked("urn:s")),
            predicate: NamedNode::new_unchecked("urn:p"),
            object,
        }];
        let prefixes = Prefixes::new(&BTreeMap::new());
        let preparation = Preparation::new(Control::default(), 4 << 20).unwrap();
        let budget = NativeBudget::from_preparation(&preparation).unwrap();
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(32 << 10)
                .spawn_scoped(scope, || {
                    let page = QueryPage {
                        dataset: "deep",
                        commit: 1,
                        query_type: "CONSTRUCT",
                        vars: None,
                        rows: RowSource::Triples(&triples),
                        more_beyond: false,
                        total: Some(1),
                        total_is_lower_bound: false,
                        offset: 0,
                        max_rows: 1,
                        max_bytes: 1 << 20,
                        max_chars: usize::MAX,
                        elapsed_ms: 0.0,
                    };
                    for json in [false, true] {
                        let output = owned_page(&page, &prefixes, json, &budget).unwrap();
                        assert!(output.as_str().contains("leaf"));
                        assert!(output.as_str().len() > 256 * 20);
                    }
                })
                .unwrap()
                .join()
                .unwrap();
        });
    }
}
