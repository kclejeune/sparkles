//! `POST /{ds}/sparql/diff`: the rows a SELECT query returns at one state and not at
//! another, for results too large to compare in the browser (docs/specs/U01-time-slider.md).
//!
//! The query is the request body (`application/sparql-query`, or a form with `query`). `from`
//! and `to` take the selectors of `at`, `to` defaults to the head, and `branch` and
//! `reasoning` are as for `/{ds}/sparql`. Rows are compared as bags, as the UI does and as
//! C18 §6.7.5 describes: a row is its terms in order, and a row that occurs twice at one
//! state and once at the other differs once.
//!
//! The comparison keeps a 128-bit hash for each distinct row of the first result and no
//! row. Everything it keeps is charged to the request's memory budget and to the server's
//! memory pool, see [`Charge`]. The second result is read against those hashes, which finds the rows only it has
//! and keeps up to `limit` of them as they go by. The rows only the first result has are
//! found by running its query once more. That makes three executions, and the request
//! counts twice against the `query` rate limit. The query must return the same rows each
//! time it runs, and a request whose query does not is refused with `409`.

use super::*;
use sparkles::history::At;
use sparkles::sparql::pool::PoolGrant;
use sparkles::sparql::{QueryKind, QueryResult};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How many rows a loop reads between two looks at the deadline and the cancel flag.
const CHECK_EVERY: usize = 4096;
/// What a distinct row costs in the table of hashes: a 32-byte slot, which the 16-byte
/// alignment of the key makes of a `(u128, u32)`, a control byte, and room for the load
/// factor and for the table's growth.
const SEEN_ENTRY_BYTES: u64 = 48;
/// The rows listed for each side without `limit`.
const DEFAULT_LIMIT: usize = 100;
/// The most rows listed for each side.
const MAX_LIMIT: usize = 10_000;

fn bad(msg: impl Into<String>) -> ApiError {
    err(StatusCode::BAD_REQUEST, msg)
}

/// A 128-bit hash of a row: its terms in order, unbound cells apart from any term.
fn row_hash(r: &QueryResult, i: usize, buf: &mut String) -> u128 {
    buf.clear();
    for col in &r.table.cols {
        match r.term(col[i]) {
            Some(t) => {
                let _ = write!(buf, "{t}\u{1}");
            }
            None => buf.push('\u{2}'),
        }
    }
    let mut a = std::collections::hash_map::DefaultHasher::new();
    let mut b = std::collections::hash_map::DefaultHasher::new();
    0xA5u8.hash(&mut a);
    0x5Au8.hash(&mut b);
    buf.hash(&mut a);
    buf.hash(&mut b);
    (u128::from(a.finish()) << 64) | u128::from(b.finish())
}

/// The row as a JSON object of its bound variables, in SPARQL JSON terms.
fn row_json(r: &QueryResult, i: usize) -> J {
    let mut o = serde_json::Map::new();
    for (v, col) in r.vars.iter().zip(&r.table.cols) {
        if let Some(t) = r.term(col[i]) {
            o.insert(v.clone(), sparkles::sparql::results::term_json(&t));
        }
    }
    J::Object(o)
}

/// The deadline of the whole comparison, which the three executions and the loops between
/// them share, and the flag that a disconnected client sets.
struct Deadline {
    at: Option<Instant>,
    cancel: Arc<AtomicBool>,
}

impl Deadline {
    fn new(timeout: Option<Duration>, cancel: Arc<AtomicBool>) -> Deadline {
        Deadline {
            at: timeout.map(|t| Instant::now() + t),
            cancel,
        }
    }

    /// Fail when the request was cancelled or its time is up.
    fn now(&self) -> Result<(), sparkles::Error> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(sparkles::Error::Cancelled);
        }
        if self.at.is_some_and(|t| Instant::now() >= t) {
            return Err(sparkles::Error::Timeout);
        }
        Ok(())
    }

    /// The look that a loop takes at row `i`: every [`CHECK_EVERY`] rows.
    fn check(&self, i: usize) -> Result<(), sparkles::Error> {
        if i.is_multiple_of(CHECK_EVERY) {
            self.now()
        } else {
            Ok(())
        }
    }

    /// The time that is left for the next execution, which fails when there is none.
    fn left(&self) -> Result<Option<Duration>, sparkles::Error> {
        self.now()?;
        Ok(self.at.map(|t| t.saturating_duration_since(Instant::now())))
    }
}

/// The memory the comparison holds, charged as it grows to the request's budget
/// (`--query-memory-mb`, lowered by `memory-mb`) and to the grant the request was admitted
/// with when the server has a memory pool. The engine charges its own execution and
/// releases it when a run ends, so this counts what the comparison keeps between runs: the
/// table of the result being read, the hashes and the listed rows. Dropping it gives the
/// grant back everything it still holds, whichever way the request ends.
struct Charge {
    grant: Option<Arc<PoolGrant>>,
    limit: Option<u64>,
    held: u64,
}

impl Charge {
    fn new(opts: &sparkles::sparql::QueryOptions) -> Charge {
        Charge {
            grant: opts.grant.clone(),
            limit: opts.max_memory_bytes,
            held: 0,
        }
    }

    /// Account for `n` more bytes: `507` with `budget: "memory"` past the request's budget,
    /// and the pool's own refusal when it cannot give the bytes.
    fn charge(&mut self, n: u64) -> Result<(), sparkles::Error> {
        let total = self.held.saturating_add(n);
        if let Some(limit) = self.limit
            && total > limit
        {
            return Err(sparkles::Error::BudgetExceeded(sparkles::Budget {
                kind: sparkles::BudgetKind::Memory,
                limit,
                requested: total,
            }));
        }
        if let Some(g) = &self.grant {
            g.charge(n)?;
        }
        self.held = total;
        Ok(())
    }

    /// Account for `n` fewer bytes.
    fn release(&mut self, n: u64) {
        let n = n.min(self.held);
        if let Some(g) = &self.grant {
            g.release(n);
        }
        self.held -= n;
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.release(self.held);
    }
}

/// The bytes of the table a result keeps: a cell is a term id.
fn table_bytes(r: &QueryResult) -> u64 {
    (r.len() as u64).saturating_mul(r.table.cols.len() as u64) * 8
}

/// The rows listed so far, against the response's byte budget (`--max-result-mb` and the
/// request's `max-result-mb`). Both lists count together, since both go in one body.
struct Listed {
    limit: Option<u64>,
    bytes: u64,
}

impl Listed {
    fn new(limit: Option<u64>) -> Listed {
        Listed { limit, bytes: 0 }
    }

    /// Count a row's JSON, or fail with `507` once the rows alone pass the budget, and
    /// charge the row to the memory the comparison holds.
    fn add(&mut self, row: J, charge: &mut Charge) -> ApiResult<J> {
        // the comma that separates it from the row before
        let len = serde_json::to_vec(&row).map_or(0, |v| v.len() as u64) + 1;
        self.bytes += len;
        self.check(self.bytes)?;
        charge.charge(len)?;
        Ok(row)
    }

    /// The whole body, which also holds the envelope.
    fn check_body(&self, len: u64) -> ApiResult<()> {
        self.check(len)
    }

    fn check(&self, len: u64) -> ApiResult<()> {
        match self.limit {
            Some(limit) if len > limit => Err(sparkles::Error::BudgetExceeded(sparkles::Budget {
                kind: sparkles::BudgetKind::ResultBytes,
                limit,
                requested: len,
            })
            .into()),
            _ => Ok(()),
        }
    }
}

/// Run the query at `at` and check that it is a SELECT.
fn run(
    ds: &Dataset,
    at: &At,
    query: &str,
    opts: &sparkles::sparql::QueryOptions,
) -> ApiResult<(QueryResult, sparkles::history::Resolved)> {
    let (snap, resolved) = history::snapshot_for(ds, Some(at), opts)?;
    let r = sparkles::sparql::query_with_plan(snap, query, opts)
        .map_err(|f| ApiError::from(f.error))?;
    if r.kind != QueryKind::Select {
        return Err(bad("sparql/diff compares the rows of a SELECT query"));
    }
    let resolved = resolved.ok_or_else(|| bad("the state could not be resolved"))?;
    Ok((r, resolved))
}

pub(super) async fn sparql_diff(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
    QueryBody(body): QueryBody,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let mut params = Params::from_query(&uri);
    let ct = content_type(&headers);
    let query = match ct.as_str() {
        "application/x-www-form-urlencoded" => {
            params.extend_form(&body);
            params.get("query").unwrap_or_default().to_string()
        }
        "application/json" => {
            let j: J = serde_json::from_slice(&body).map_err(|e| bad(format!("body: {e}")))?;
            for k in ["from", "to", "reasoning", "limit"] {
                if let Some(v) = j.get(k).filter(|v| !v.is_null()) {
                    let s = v
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string());
                    params.0.push((k.to_string(), s));
                }
            }
            j.get("query")
                .and_then(J::as_str)
                .unwrap_or_default()
                .to_string()
        }
        _ => String::from_utf8_lossy(&body).into_owned(),
    };
    if query.trim().is_empty() {
        return Err(bad("missing query: send it as application/sparql-query"));
    }
    let from = super::diff::selector(&params, "from")?
        .ok_or_else(|| bad("from: required, a commit, time or snapshot as for at"))?;
    let to = super::diff::selector(&params, "to")?.unwrap_or(At::Head);
    let limit = match params.get("limit") {
        None => DEFAULT_LIMIT,
        Some(v) => match v.parse::<usize>() {
            Ok(n) if (1..=MAX_LIMIT).contains(&n) => n,
            _ => return Err(bad(format!("limit must be from 1 to {MAX_LIMIT}"))),
        },
    };
    let mut opts = query_options(&st, &ds, &params);
    // budgets the request asked for, never above the server's
    let asked = budgets::Overrides::parse(&params)?;
    asked.apply(&mut opts);
    let max_bytes = asked.result_limit(st.limits.max_result_bytes);
    crate::auth::restrict(&mut opts, &p, &ds.name, crate::auth::Endpoint::Query);
    let (cancel, _cancel_on_drop) = cancel_on_drop();
    opts.cancel = Some(cancel.clone());
    let restricted = p.restricted(&ds.name);
    let out = blocking({
        let ds = ds.clone();
        move || {
            let mut buf = String::new();
            let mut opts = opts;
            // one deadline for all three executions and the loops between them
            let deadline = Deadline::new(opts.timeout, cancel);
            let mut charge = Charge::new(&opts);
            // the earlier state: a hash for each distinct row, and how many of it there are
            opts.timeout = deadline.left()?;
            let (left, from_at) = run(&ds, &from, &query, &opts)?;
            let vars = left.vars.clone();
            let left_rows = left.len() as u64;
            // the table is held for as long as `left` is
            let left_bytes = table_bytes(&left);
            charge.charge(left_bytes)?;
            let mut seen: HashMap<u128, u32> = HashMap::new();
            for i in 0..left.len() {
                deadline.check(i)?;
                match seen.entry(row_hash(&left, i, &mut buf)) {
                    Entry::Occupied(mut e) => *e.get_mut() += 1,
                    Entry::Vacant(e) => {
                        charge.charge(SEEN_ENTRY_BYTES)?;
                        e.insert(1);
                    }
                }
            }
            drop(left);
            charge.release(left_bytes);
            // the later state: a row without a partner is only there, and kept as it goes by
            opts.timeout = deadline.left()?;
            let (right, to_at) = run(&ds, &to, &query, &opts)?;
            let right_rows = right.len() as u64;
            let right_bytes = table_bytes(&right);
            charge.charge(right_bytes)?;
            let (mut same, mut added) = (0u64, 0u64);
            let mut added_rows = Vec::new();
            let mut listed = Listed::new(max_bytes);
            for i in 0..right.len() {
                deadline.check(i)?;
                match seen.get_mut(&row_hash(&right, i, &mut buf)) {
                    Some(n) if *n > 0 => {
                        *n -= 1;
                        same += 1;
                    }
                    _ => {
                        added += 1;
                        if added_rows.len() < limit {
                            added_rows.push(listed.add(row_json(&right, i), &mut charge)?);
                        }
                    }
                }
            }
            drop(right);
            charge.release(right_bytes);
            // what is left of the earlier hashes is only there: read that state again for the rows
            let removed: u64 = seen.values().map(|n| u64::from(*n)).sum();
            let mut removed_rows = Vec::new();
            if removed > 0 {
                let again = At::Commit(from_at.commit.seq);
                opts.timeout = deadline.left()?;
                let (left, _) = run(&ds, &again, &query, &opts)?;
                let again_bytes = table_bytes(&left);
                charge.charge(again_bytes)?;
                for i in 0..left.len() {
                    deadline.check(i)?;
                    if removed_rows.len() >= limit {
                        break;
                    }
                    if let Some(n) = seen.get_mut(&row_hash(&left, i, &mut buf))
                        && *n > 0
                    {
                        *n -= 1;
                        removed_rows.push(listed.add(row_json(&left, i), &mut charge)?);
                    }
                }
                drop(left);
                charge.release(again_bytes);
                // a query that does not return the rows it returned before cannot be compared
                // by hashes, and listing less than it counted would misdescribe the answer
                if (removed_rows.len() as u64) < removed.min(limit as u64) {
                    return Err(ApiError(
                        StatusCode::CONFLICT,
                        json!({
                            "error": "the query returned different rows when it ran again, as it does with STRUUID, UUID, RAND, NOW, BNODE or SERVICE",
                            "code": "not-repeatable",
                        }),
                    ));
                }
            }
            // the hashes are no longer needed
            charge.release(seen.len() as u64 * SEEN_ENTRY_BYTES);
            drop(seen);
            let side = |r: &sparkles::history::Resolved| {
                let mut commit = json!(r.commit);
                if restricted {
                    redact_commit_json(&mut commit);
                }
                json!({ "selector": r.at.to_string(), "commit": commit })
            };
            let out = json!({
                "dataset": ds.name,
                "datasetId": ds.store.owner_dataset_id(),
                "from": side(&from_at),
                "to": side(&to_at),
                "vars": vars,
                "fromRows": left_rows,
                "toRows": right_rows,
                "same": same,
                "added": added,
                "removed": removed,
                "addedRows": added_rows,
                "removedRows": removed_rows,
                "truncated": { "added": added > added_rows.len() as u64, "removed": removed > removed_rows.len() as u64 },
            });
            // the envelope is small next to the rows, and the check makes the bound exact
            deadline.now()?;
            let body = serde_json::to_vec(&out).map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            listed.check_body(body.len() as u64)?;
            // the body is held until the response is sent
            charge.charge(body.len() as u64)?;
            Ok(body)
        }
    })
    .await?;
    Ok(([(header::CONTENT_TYPE, "application/json")], out).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles::BudgetKind;
    use sparkles::sparql::pool::QueryPool;

    const MIB: u64 = 1 << 20;

    #[test]
    fn a_deadline_that_has_passed_fails_the_next_look_of_a_loop() {
        let cancel = Arc::new(AtomicBool::new(false));
        let d = Deadline::new(Some(Duration::ZERO), cancel.clone());
        // the look is taken every CHECK_EVERY rows, starting with the first
        assert!(matches!(d.check(0), Err(sparkles::Error::Timeout)));
        assert!(d.check(1).is_ok());
        assert!(matches!(
            d.check(CHECK_EVERY),
            Err(sparkles::Error::Timeout)
        ));
        assert!(matches!(d.left(), Err(sparkles::Error::Timeout)));
        // time that is left goes to the next execution
        let d = Deadline::new(Some(Duration::from_secs(60)), cancel.clone());
        assert!(d.check(0).is_ok());
        let left = d.left().unwrap().unwrap();
        assert!(left > Duration::from_secs(50) && left <= Duration::from_secs(60));
        // no timeout means no deadline
        assert_eq!(Deadline::new(None, cancel.clone()).left().unwrap(), None);
        // a cancelled request stops at the next look, whatever the time
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(d.check(0), Err(sparkles::Error::Cancelled)));
        assert!(matches!(d.left(), Err(sparkles::Error::Cancelled)));
    }

    #[test]
    fn a_charge_rises_in_the_pool_and_falls_back_when_it_ends() {
        let pool = QueryPool::new(8 * MIB);
        let grant = Arc::new(pool.try_grant(MIB).unwrap());
        let mut c = Charge {
            grant: Some(grant.clone()),
            limit: None,
            held: 0,
        };
        c.charge(MIB / 2).unwrap();
        assert_eq!(grant.used(), MIB / 2);
        // past the first granule the grant grows and the pool's granted bytes rise
        c.charge(3 * MIB).unwrap();
        assert_eq!(grant.used(), MIB / 2 + 3 * MIB);
        assert!(pool.granted() >= 3 * MIB + MIB / 2);
        c.release(3 * MIB);
        assert_eq!(grant.used(), MIB / 2);
        // a charge that ends early, as on an error, gives back what it still holds
        drop(c);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn a_charge_past_the_request_budget_is_a_memory_budget_error() {
        let pool = QueryPool::new(8 * MIB);
        let grant = Arc::new(pool.try_grant(MIB).unwrap());
        let mut c = Charge {
            grant: Some(grant.clone()),
            limit: Some(100),
            held: 0,
        };
        c.charge(60).unwrap();
        let sparkles::Error::BudgetExceeded(b) = c.charge(50).unwrap_err() else {
            panic!("not a budget error");
        };
        assert_eq!(
            (b.kind, b.limit, b.requested),
            (BudgetKind::Memory, 100, 110)
        );
        // the refused bytes were not taken
        assert_eq!((c.held, grant.used()), (60, 60));
        // without a pool the request's budget still applies
        let mut c = Charge {
            grant: None,
            limit: Some(10),
            held: 0,
        };
        assert!(c.charge(11).is_err());
        assert!(c.charge(10).is_ok());
    }

    #[test]
    fn a_charge_the_pool_cannot_cover_is_refused_and_not_counted() {
        let pool = QueryPool::new(MIB);
        let grant = Arc::new(pool.try_grant(MIB / 2).unwrap());
        let mut c = Charge {
            grant: Some(grant.clone()),
            limit: None,
            held: 0,
        };
        c.charge(MIB / 4).unwrap();
        assert!(c.charge(2 * MIB).is_err());
        assert_eq!((c.held, grant.used()), (MIB / 4, MIB / 4));
    }
}
