//! `GET /$/commits/{ds}/timeline`: the commit history of a branch in buckets, for a time
//! slider over a history too long to list.
//!
//! The commits from `from` to `to` (commit numbers; the whole history by default) are cut
//! into at most `buckets` runs of consecutive numbers (default 200, at most 2,000). Each
//! bucket says which commits it covers, when, and how many quads they added and removed.
//! `readable` lists the ranges of commits whose state can still be read. A caller whose
//! grants cover only some graphs gets no quad counts, which would count every graph.

use super::*;
use sparkles::commit::{CommitInfo, CommitKind, CommitRange};
use std::collections::{BTreeMap, HashMap};

/// The buckets of a request without `buckets`.
const DEFAULT_BUCKETS: u64 = 200;
/// The most buckets a request may ask for.
const MAX_BUCKETS: u64 = 2_000;

/// How many commits are bucketed between two looks at the cancel flag.
const CHECK_EVERY: usize = 65_536;

/// One run of consecutive commits.
struct Bucket {
    first_seq: u64,
    last_seq: u64,
    first_time: i64,
    last_time: i64,
    commits: u64,
    inserted: u64,
    deleted: u64,
    kinds: BTreeMap<&'static str, u64>,
    merges: u64,
    snapshots: u64,
}

impl Bucket {
    fn new(c: &CommitInfo) -> Bucket {
        Bucket {
            first_seq: c.seq,
            last_seq: c.seq,
            first_time: c.timestamp_ms,
            last_time: c.timestamp_ms,
            commits: 0,
            inserted: 0,
            deleted: 0,
            kinds: BTreeMap::new(),
            merges: 0,
            snapshots: 0,
        }
    }

    fn add(&mut self, c: &CommitInfo, snapshots: u64) {
        self.last_seq = c.seq;
        self.last_time = c.timestamp_ms;
        self.commits += 1;
        self.inserted += c.inserted;
        self.deleted += c.deleted;
        *self.kinds.entry(c.kind.name()).or_default() += 1;
        if c.kind == CommitKind::Merge {
            self.merges += 1;
        }
        self.snapshots += snapshots;
    }

    fn json(&self, restricted: bool) -> J {
        let time = |ms: i64| sparkles::commit::rfc3339_ms(ms);
        let mut j = json!({
            "firstSeq": self.first_seq,
            "lastSeq": self.last_seq,
            "firstTime": time(self.first_time),
            "lastTime": time(self.last_time),
            "commits": self.commits,
            "inserted": self.inserted,
            "deleted": self.deleted,
            "kinds": self.kinds,
            "merges": self.merges,
            "snapshots": self.snapshots,
        });
        if restricted && let Some(m) = j.as_object_mut() {
            m.remove("inserted");
            m.remove("deleted");
        }
        j
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    err(StatusCode::BAD_REQUEST, msg)
}

/// The commit numbers from `lo` to `hi` (the head at most): how many commits a request
/// reads, none when the range starts after the head, and a `400` beyond `max`, which the
/// catalog's way of handing the commits over as one list makes the bound of a request's memory.
fn range_len(lo: u64, hi: u64, max: u64) -> ApiResult<u64> {
    let wanted = if lo > hi {
        0
    } else {
        (hi - lo).saturating_add(1)
    };
    if wanted > max {
        return Err(bad(format!(
            "the range holds {wanted} commits, more than the {max} a timeline reads: narrow it with from and to"
        )));
    }
    Ok(wanted)
}

pub(super) async fn timeline(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let num = |k: &str| -> ApiResult<Option<u64>> {
        params
            .get(k)
            .map(|v| {
                v.parse::<u64>()
                    .map_err(|_| bad(format!("{k} must be a non-negative integer, not {v:?}")))
            })
            .transpose()
    };
    let buckets = num("buckets")?.unwrap_or(DEFAULT_BUCKETS);
    if buckets == 0 || buckets > MAX_BUCKETS {
        return Err(bad(format!("buckets must be from 1 to {MAX_BUCKETS}")));
    }
    let (lo, hi) = (num("from")?.unwrap_or(0), num("to")?.unwrap_or(u64::MAX));
    if lo > hi {
        return Err(bad("from is after to"));
    }
    // the head first, and no commit after it: a commit made while the catalog is read
    // would otherwise give a bucket past the head the response names
    let head = ds.store.head_commit().seq;
    let hi = hi.min(head);
    let wanted = range_len(lo, hi, st.limits.max_timeline_commits)?;
    let (cancel, _cancel_on_drop) = cancel_on_drop();
    let restricted = p.restricted(&ds.name);
    let (width, first, last, count, out) = blocking({
        let ds = ds.clone();
        move || {
            let n = wanted as usize;
            // only the range, newest first, from the catalog of the branch
            let mut commits: Vec<CommitInfo> = if n == 0 {
                Vec::new()
            } else if ds.kind == DbType::Persistent {
                let main = ds.main().unwrap_or_else(|| ds.clone());
                let branch = ds.branch_name().to_string();
                main.store
                    .branch_commits(&branch, CommitRange::Before(hi + 1), n)?
                    .into_iter()
                    .map(|c| c.commit)
                    .collect()
            } else {
                ds.dataset
                    .history()
                    .commits(CommitRange::Before(hi + 1), n)
                    .commits
            };
            // the catalog may skip commits, and a store that reads its own newer head can
            // return more than was asked for
            commits.retain(|c| c.seq >= lo && c.seq <= hi);
            commits.reverse();
            let mut snapshots: HashMap<u64, u64> = HashMap::new();
            for s in ds.dataset.snapshots().list() {
                *snapshots.entry(s.seq).or_default() += 1;
            }
            let first = commits.first().map_or(0, |c| c.seq);
            let last = commits.last().map_or(0, |c| c.seq);
            // buckets of equal width in commit numbers, so a slider maps a position to a number
            let span = last.saturating_sub(first) + 1;
            let width = span.div_ceil(buckets).max(1);
            let mut out: Vec<Bucket> = Vec::new();
            let mut current = None;
            for (i, c) in commits.iter().enumerate() {
                if i % CHECK_EVERY == 0 && cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(sparkles::Error::Cancelled.into());
                }
                let index = (c.seq - first) / width;
                if current != Some(index) {
                    out.push(Bucket::new(c));
                    current = Some(index);
                }
                if let Some(b) = out.last_mut() {
                    b.add(c, snapshots.get(&c.seq).copied().unwrap_or(0));
                }
            }
            Ok((width, first, last, commits.len(), out))
        }
    })
    .await?;
    let (oldest, readable, _) = history::commit_list_extras(&ds, &[]);
    Ok(Json(json!({
        "dataset": name,
        "datasetId": ds.store.owner_dataset_id(),
        "head": head,
        "firstSeq": first,
        "lastSeq": last,
        "commits": count,
        "bucketWidth": width,
        "oldestReconstructable": oldest,
        "readable": readable,
        "buckets": out.iter().map(|b| b.json(restricted)).collect::<Vec<_>>(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_is_counted_and_capped() {
        const MAX: u64 = 10_000_000;
        assert_eq!(range_len(0, 0, MAX).ok(), Some(1));
        assert_eq!(range_len(2, 3, MAX).ok(), Some(2));
        // from after the clamped head: nothing to read
        assert_eq!(range_len(50, 4, MAX).ok(), Some(0));
        assert_eq!(range_len(0, MAX - 1, MAX).ok(), Some(MAX));
        let Err(e) = range_len(0, MAX, MAX) else {
            panic!("a range past the ceiling is refused");
        };
        assert_eq!(e.0, StatusCode::BAD_REQUEST);
        assert!(e.1["error"].as_str().unwrap().contains("10000000"));
        assert!(range_len(0, u64::MAX, MAX).is_err());
    }
}
