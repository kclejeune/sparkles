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
use std::collections::BTreeMap;

/// The buckets of a request without `buckets`.
const DEFAULT_BUCKETS: u64 = 200;
/// The most buckets a request may ask for.
const MAX_BUCKETS: u64 = 2_000;

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
    let head = ds.store.head_commit();
    // every commit whose metadata the catalog keeps, oldest first
    let all: Vec<CommitInfo> = if ds.kind == DbType::Persistent {
        let main = ds.main().unwrap_or_else(|| ds.clone());
        let branch = ds.branch_name().to_string();
        let mut v = blocking(move || {
            Ok(main
                .store
                .branch_commits(&branch, CommitRange::Latest, usize::MAX)?
                .into_iter()
                .map(|c| c.commit)
                .collect::<Vec<_>>())
        })
        .await?;
        v.reverse();
        v
    } else {
        let mut v = ds
            .dataset
            .history()
            .commits(CommitRange::Latest, usize::MAX)
            .commits;
        v.reverse();
        v
    };
    let commits: Vec<&CommitInfo> = all.iter().filter(|c| c.seq >= lo && c.seq <= hi).collect();
    let pins = ds.dataset.snapshots().list();
    let first = commits.first().map_or(0, |c| c.seq);
    let last = commits.last().map_or(0, |c| c.seq);
    // buckets of equal width in commit numbers, so a slider maps a position to a number
    let span = last.saturating_sub(first) + 1;
    let width = span.div_ceil(buckets).max(1);
    let mut out: Vec<Bucket> = Vec::new();
    let mut current = None;
    for c in &commits {
        let index = (c.seq - first) / width;
        if current != Some(index) {
            out.push(Bucket::new(c));
            current = Some(index);
        }
        let snaps = pins.iter().filter(|p| p.seq == c.seq).count() as u64;
        if let Some(b) = out.last_mut() {
            b.add(c, snaps);
        }
    }
    let restricted = p.restricted(&ds.name);
    let (oldest, readable, _) = history::commit_list_extras(&ds, &[]);
    Ok(Json(json!({
        "dataset": name,
        "datasetId": ds.store.owner_dataset_id(),
        "head": head.seq,
        "firstSeq": first,
        "lastSeq": last,
        "commits": commits.len(),
        "bucketWidth": width,
        "oldestReconstructable": oldest,
        "readable": readable,
        "buckets": out.iter().map(|b| b.json(restricted)).collect::<Vec<_>>(),
    })))
}
