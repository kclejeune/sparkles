//! The answers of the groups route, kept per branch, commit, view and request.
//!
//! A commit's data never changes, so an answer stays right for as long as its key can be
//! asked for again. The key holds the branch's id (unique across datasets and their
//! recreation), the commit, the caller's read view and the dataset's inferred overlay,
//! so one caller never reads an answer computed for a wider view. The cache keeps at most
//! [`MAX_ENTRIES`] answers and [`MAX_BYTES`] of their JSON, and drops the oldest first.

use parking_lot::Mutex;
use serde_json::Value as J;
use std::collections::VecDeque;
use std::sync::Arc;

/// Answers kept at most.
pub(super) const MAX_ENTRIES: usize = 64;
/// Bytes of serialized answers kept at most.
pub(super) const MAX_BYTES: usize = 16 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::http) struct Key {
    pub branch: uuid::Uuid,
    pub commit: u64,
    pub view: String,
    pub extra: String,
    pub request: String,
}

struct Entry {
    key: Key,
    answer: Arc<J>,
    bytes: usize,
}

#[derive(Default)]
struct Cache {
    entries: VecDeque<Entry>,
    bytes: usize,
}

static CACHE: Mutex<Cache> = parking_lot::const_mutex(Cache {
    entries: VecDeque::new(),
    bytes: 0,
});

/// The key of what a caller's view reads: its graphs and the protections of triples it
/// keeps (C12 and C12b). Two callers with the same graphs but different lifted
/// protections read different data, so both parts are in the key.
pub(in crate::http) fn view_key(
    graphs: Option<&std::sync::Arc<sparkles::access::GraphAccess>>,
) -> String {
    match graphs {
        None => String::new(),
        Some(g) => match g.triples.as_ref().filter(|t| t.hides()) {
            Some(t) => format!("{}\u{1f}{}", g.read_key(), t.read_key()),
            None => g.read_key(),
        },
    }
}

/// The answer kept for `key`, moved to the newest end.
pub(in crate::http) fn get(key: &Key) -> Option<Arc<J>> {
    let mut c = CACHE.lock();
    let i = c.entries.iter().position(|e| e.key == *key)?;
    let e = c.entries.remove(i)?;
    let answer = e.answer.clone();
    c.entries.push_back(e);
    Some(answer)
}

/// Keep `answer` for `key`. An answer larger than a quarter of the cache is not kept.
pub(in crate::http) fn put(key: Key, answer: Arc<J>) {
    let bytes = answer.to_string().len() + key.request.len() + key.view.len();
    if bytes > MAX_BYTES / 4 {
        return;
    }
    let mut c = CACHE.lock();
    if let Some(i) = c.entries.iter().position(|e| e.key == key)
        && let Some(old) = c.entries.remove(i)
    {
        c.bytes -= old.bytes;
    }
    while c.entries.len() >= MAX_ENTRIES || (c.bytes + bytes > MAX_BYTES && !c.entries.is_empty()) {
        if let Some(old) = c.entries.pop_front() {
            c.bytes -= old.bytes;
        }
    }
    c.bytes += bytes;
    c.entries.push_back(Entry { key, answer, bytes });
}

#[cfg(test)]
pub(super) fn len() -> usize {
    CACHE.lock().entries.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(commit: u64, view: &str) -> Key {
        Key {
            branch: uuid::Uuid::nil(),
            commit,
            view: view.into(),
            extra: String::new(),
            request: "r".into(),
        }
    }

    #[test]
    fn the_cache_is_keyed_by_view_and_bounded() {
        put(key(u64::MAX - 1, "a"), Arc::new(J::from(1)));
        assert_eq!(get(&key(u64::MAX - 1, "a")).as_deref(), Some(&J::from(1)));
        // another view, or another commit, never reads it
        assert!(get(&key(u64::MAX - 1, "b")).is_none());
        assert!(get(&key(u64::MAX - 2, "a")).is_none());
        for i in 0..(MAX_ENTRIES as u64 + 5) {
            put(key(i, "bound"), Arc::new(J::from(i)));
        }
        assert!(len() <= MAX_ENTRIES);
        assert!(get(&key(0, "bound")).is_none(), "the oldest went first");
        assert!(get(&key(MAX_ENTRIES as u64 + 4, "bound")).is_some());
    }
}
