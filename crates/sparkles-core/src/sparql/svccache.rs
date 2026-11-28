//! The cache of remote SERVICE results (Jena's service enhancer, `SERVICE <cache:…>`).
//!
//! An entry holds the solutions an endpoint returned for one request: the endpoint's
//! URL, the SERVICE pattern and the values substituted into it for one input binding.
//! The solutions are kept as terms, because remote terms have no ids in the store's
//! dictionary. Keys start with the caller's scope (see
//! [`QueryOptions::service_scope`](super::QueryOptions::service_scope)), so callers with
//! different credentials or views never read each other's entries.
//!
//! The cache is bounded by bytes, estimated from the terms' lengths, and an entry larger
//! than an eighth of the budget is not kept. Only complete responses are stored. A bulk
//! response that an endpoint cut short is never split into entries.

pub use super::svcfn::{CACHE_LS, CACHE_RM};

use oxrdf::Term;
use std::fmt::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// The solutions of one request, by variable name.
pub struct Rows {
    pub vars: Vec<String>,
    pub rows: Vec<Vec<Option<Term>>>,
}

impl Rows {
    /// The estimated bytes the entry holds.
    pub fn bytes(&self) -> u64 {
        let rows: u64 = self.rows.iter().map(|r| Self::row_bytes(r)).sum();
        Self::vars_bytes(&self.vars) + rows + 64
    }

    /// What one row adds to [`Rows::bytes`].
    pub(crate) fn row_bytes(row: &[Option<Term>]) -> u64 {
        Self::row_bytes_of(row.iter().map(Option::as_ref))
    }

    /// [`Rows::row_bytes`] of a row's borrowed slots, which weighs a row before it is
    /// built.
    pub(crate) fn row_bytes_of<'a>(row: impl IntoIterator<Item = Option<&'a Term>>) -> u64 {
        let terms: usize = row.into_iter().map(|t| t.map_or(0, term_bytes) + 16).sum();
        (terms + 24) as u64
    }

    /// The bytes one row keeps allocated: its slots at their capacity, its vector's
    /// header and what its terms keep outside their slots (see [`term_alloc_bytes`]).
    /// [`Rows::bytes`] is the cache's weight, an estimate that this exceeds for wide
    /// rows of mostly unbound slots and for triple terms.
    #[allow(clippy::ptr_arg, reason = "the capacity of the row is what it keeps")]
    pub(crate) fn retained_row_bytes(row: &Vec<Option<Term>>) -> u64 {
        Self::retained_slots_bytes(row.capacity(), row.iter().flatten())
    }

    /// [`Rows::retained_row_bytes`] of a row of `capacity` slots holding `terms`, which
    /// sizes a row from borrowed terms before it is built.
    pub(crate) fn retained_slots_bytes<'a>(
        capacity: usize,
        terms: impl IntoIterator<Item = &'a Term>,
    ) -> u64 {
        let slots = capacity * std::mem::size_of::<Option<Term>>();
        let terms: usize = terms.into_iter().map(term_alloc_bytes).sum();
        (slots + std::mem::size_of::<Vec<Option<Term>>>() + terms) as u64
    }

    /// The bytes the entry keeps allocated, by [`Rows::retained_row_bytes`] and the
    /// capacities of its vectors.
    pub(crate) fn retained_bytes(&self) -> u64 {
        let rows: u64 = self.rows.iter().map(Self::retained_row_bytes).sum();
        let spare =
            (self.rows.capacity() - self.rows.len()) * std::mem::size_of::<Vec<Option<Term>>>();
        let vars = self.vars.capacity() * std::mem::size_of::<String>()
            + self.vars.iter().map(String::capacity).sum::<usize>();
        rows + (spare + vars + std::mem::size_of::<Self>()) as u64
    }

    /// What the variable names add to [`Rows::bytes`].
    pub(crate) fn vars_bytes(vars: &[String]) -> u64 {
        vars.iter().map(|v| v.len() as u64 + 24).sum()
    }
}

fn term_bytes(t: &Term) -> usize {
    match t {
        Term::NamedNode(n) => n.as_str().len() + 24,
        Term::BlankNode(b) => b.as_str().len() + 24,
        Term::Literal(l) => {
            l.value().len() + l.language().map_or(0, str::len) + l.datatype().as_str().len() + 48
        }
        Term::Triple(t) => {
            subject_text(&t.subject) + 24 + t.predicate.as_str().len() + term_bytes(&t.object) + 24
        }
    }
}

fn subject_text(s: &oxrdf::NamedOrBlankNode) -> usize {
    match s {
        oxrdf::NamedOrBlankNode::NamedNode(n) => n.as_str().len(),
        oxrdf::NamedOrBlankNode::BlankNode(b) => b.as_str().len(),
    }
}

/// The bytes a term keeps allocated outside the slot that holds it: its text with an
/// allowance of 24 bytes for each string, as [`term_bytes`] counts it, and for a
/// triple term the boxed triple and what its parts keep, at every level of nesting.
/// [`term_bytes`] is the cache's weight and leaves the boxes out.
fn term_alloc_bytes(t: &Term) -> usize {
    match t {
        Term::Triple(t) => {
            std::mem::size_of::<oxrdf::Triple>()
                + subject_text(&t.subject)
                + 24
                + t.predicate.as_str().len()
                + 24
                + term_alloc_bytes(&t.object)
        }
        t => term_bytes(t),
    }
}

/// A cache key: the namespace of the store that put the entry, then the request's key.
/// A lookup by [`NsKeyRef`] needs no allocation.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct NsKey {
    pub ns: u32,
    pub key: String,
}

/// A borrowed [`NsKey`]. It hashes as the owned key does.
#[derive(Hash)]
pub(crate) struct NsKeyRef<'a> {
    pub ns: u32,
    pub key: &'a str,
}

impl quick_cache::Equivalent<NsKey> for NsKeyRef<'_> {
    fn equivalent(&self, k: &NsKey) -> bool {
        self.ns == k.ns && self.key == k.key
    }
}

/// A key of the SERVICE cache: the namespace of the store that put the entry, then the
/// request's key, which the entry shares to read its parts (see [`Parts`]).
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct SvcKey {
    pub ns: u32,
    pub key: Arc<str>,
}

impl quick_cache::Equivalent<SvcKey> for NsKeyRef<'_> {
    fn equivalent(&self, k: &SvcKey) -> bool {
        self.ns == k.ns && self.key == &*k.key
    }
}

#[derive(Clone)]
pub(crate) struct Weighter;
impl quick_cache::Weighter<SvcKey, Arc<Entry>> for Weighter {
    fn weight(&self, _: &SvcKey, v: &Arc<Entry>) -> u64 {
        v.weight()
    }
}

type Inner = quick_cache::sync::Cache<SvcKey, Arc<Entry>, Weighter>;

/// The outermost slice of a cached request, which its key leaves out: the request asks
/// for the solutions of its pattern from `offset` on, at most `limit` of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Slice {
    pub offset: usize,
    pub limit: Option<usize>,
}

impl Slice {
    /// Every solution.
    pub const ALL: Slice = Slice {
        offset: 0,
        limit: None,
    };

    /// This slice with a LIMIT of `k` applied after it.
    pub fn limited(self, k: usize) -> Slice {
        Slice {
            offset: self.offset,
            limit: Some(self.limit.map_or(k, |l| l.min(k))),
        }
    }
}

/// What a SERVICE without `loop` keys its cache entry by: its request without the
/// outermost slice, and that slice, which a LIMIT pushed into the request narrows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceKey {
    pub text: String,
    pub slice: Slice,
}

/// The lengths of the parts of a key that [`key`] writes: the scope, the endpoint and
/// the pattern. The values follow them. An entry reads its parts by these lengths, so
/// no part is found by searching the key for a separator that a part could contain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Parts {
    scope: usize,
    url: usize,
    pattern: usize,
}

impl Parts {
    pub fn of(scope: &str, url: &str, pattern: &str) -> Parts {
        Parts::from_lens(scope.len(), url.len(), pattern.len())
    }

    pub(crate) fn from_lens(scope: usize, url: usize, pattern: usize) -> Parts {
        Parts {
            scope,
            url,
            pattern,
        }
    }

    pub(crate) fn scope(self) -> usize {
        self.scope
    }

    pub(crate) fn url(self) -> usize {
        self.url
    }

    pub(crate) fn pattern(self) -> usize {
        self.pattern
    }
}

/// The id of the next entry, unique in this process.
static ENTRY_IDS: AtomicU64 = AtomicU64::new(1);

/// A new entry id.
pub(crate) fn next_id() -> u64 {
    ENTRY_IDS.fetch_add(1, Ordering::Relaxed)
}

/// One cached response: the solutions a request with the slice `slice` got. They are
/// the solutions of the request's pattern from the slice's offset on, as many as the
/// endpoint sent. A response with fewer solutions than its limit, or one without a limit,
/// is not taken to end the result, since an endpoint may cap its responses without
/// saying so.
pub struct Entry {
    key: Arc<str>,
    parts: Parts,
    /// the entry's number, which `se:cacheLs` shows and `se:cacheRm` takes
    pub id: u64,
    /// the slice of the request that got the rows
    pub slice: Slice,
    /// when the entry was stored, for the time to live
    pub born: Instant,
    /// the same moment by the wall clock, which the disk tier keeps
    pub stored: SystemTime,
    pub rows: Arc<Rows>,
}

impl Entry {
    pub(crate) fn new(
        key: Arc<str>,
        parts: Parts,
        slice: Slice,
        rows: Arc<Rows>,
        stored: SystemTime,
    ) -> Entry {
        let age = SystemTime::now().duration_since(stored).unwrap_or_default();
        Entry {
            key,
            parts,
            id: next_id(),
            slice,
            born: Instant::now().checked_sub(age).unwrap_or_else(Instant::now),
            stored,
            rows,
        }
    }

    /// The key without the store's namespace.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The scope the entry belongs to.
    pub fn scope(&self) -> &str {
        self.key.get(..self.parts.scope).unwrap_or("")
    }

    /// The endpoint's URL, credentials included.
    pub fn endpoint(&self) -> &str {
        let at = self.parts.scope + 1;
        self.key.get(at..at + self.parts.url).unwrap_or("")
    }

    /// The pattern or request text, without its outermost slice.
    pub fn pattern(&self) -> &str {
        let at = self.parts.scope + self.parts.url + 2;
        self.key.get(at..at + self.parts.pattern).unwrap_or("")
    }

    /// The values substituted into the pattern, as `?v=term;` by variable name.
    pub fn binding(&self) -> &str {
        let at = self.parts.scope + self.parts.url + self.parts.pattern + 3;
        self.key.get(at..).unwrap_or("")
    }

    pub(crate) fn parts(&self) -> Parts {
        self.parts
    }

    /// The cache's weight of the entry: its rows, its key and its own fields.
    pub fn weight(&self) -> u64 {
        self.rows.bytes() + self.key.len() as u64 + 96
    }

    /// The rows of the entry that a request with the slice `want` gets, if the entry
    /// holds all of them. It does when `want` is the entry's own slice, or when `want`
    /// has a limit and every solution it asks for is among the entry's rows. A request
    /// that reaches past the rows is not answered, whatever the entry's slice was.
    pub(crate) fn range(&self, want: Slice) -> Option<std::ops::Range<usize>> {
        let len = self.rows.rows.len();
        if want == self.slice {
            return Some(0..len);
        }
        let start = want.offset.checked_sub(self.slice.offset)?;
        let stop = start.checked_add(want.limit?)?;
        (stop <= len).then_some(start..stop)
    }

    /// The solutions a request with the slice `want` gets, if the entry holds them: the
    /// entry's own rows when they are exactly those, or a copy of the part asked for.
    pub fn serve(&self, want: Slice) -> Option<Arc<Rows>> {
        self.serve_admitted(want, &|_| Ok(())).ok().flatten()
    }

    /// [`Entry::serve`], admitting the bytes of a copy with `admit` before it is made.
    pub(crate) fn serve_admitted(
        &self,
        want: Slice,
        admit: &dyn Fn(u64) -> crate::error::Result<()>,
    ) -> crate::error::Result<Option<Arc<Rows>>> {
        let Some(std::ops::Range { start, end: stop }) = self.range(want) else {
            return Ok(None);
        };
        if start == 0 && stop == self.rows.rows.len() {
            return Ok(Some(self.rows.clone()));
        }
        let vars = self.rows.vars.len() * std::mem::size_of::<String>()
            + self.rows.vars.iter().map(String::len).sum::<usize>();
        let copy: u64 = self.rows.rows[start..stop]
            .iter()
            .map(Rows::retained_row_bytes)
            .fold(
                (std::mem::size_of::<Rows>() + vars) as u64,
                u64::saturating_add,
            );
        admit(copy)?;
        Ok(Some(Arc::new(Rows {
            vars: self.rows.vars.clone(),
            rows: self.rows.rows[start..stop].to_vec(),
        })))
    }
}

/// One entry of a caller's scope, as `se:cacheLs` lists it.
#[derive(Clone, Debug)]
pub struct Listed {
    pub id: u64,
    /// the endpoint's URL without credentials, as [`listed_endpoint`] gives it
    pub endpoint: String,
    pub pattern: String,
    pub binding: String,
    pub rows: usize,
    pub bytes: u64,
    pub age: Duration,
}

impl Listed {
    /// The bytes [`Listed::of`] allocates for `e`, at most, with its place in the
    /// listing and in the set of keys a listing has seen.
    fn bytes_of(e: &Entry) -> u64 {
        let text = e.endpoint().len() + e.pattern().len() + e.binding().len();
        (text + 2 * std::mem::size_of::<Listed>() + 2 * std::mem::size_of::<Arc<str>>()) as u64
    }

    fn of(e: &Entry) -> Listed {
        Listed {
            id: e.id,
            endpoint: listed_endpoint(e.endpoint()),
            pattern: e.pattern().to_string(),
            binding: e.binding().to_string(),
            rows: e.rows.rows.len(),
            bytes: e.weight(),
            age: e.born.elapsed(),
        }
    }
}

/// What `se:cacheLs` lists, and the disk tier writes, for an endpoint whose URL it does
/// not show.
pub const UNLISTED_ENDPOINT: &str = "urn:x-sparkles:unlisted-endpoint";

/// `url` as `se:cacheLs` lists it and the disk tier writes it: parsed as the outbound
/// client parses it before it sends a request, so every spelling it accepts is read
/// alike, and written without the user name and password of its authority, its query
/// string and its fragment, any of which may hold a credential. A URL that is not one
/// the client sends to, an HTTP or HTTPS URL, is listed as [`UNLISTED_ENDPOINT`].
pub fn listed_endpoint(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut u) if matches!(u.scheme(), "http" | "https") => {
            // an HTTP URL has an authority, so both are always cleared
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.set_query(None);
            u.set_fragment(None);
            u.into()
        }
        _ => UNLISTED_ENDPOINT.to_string(),
    }
}

/// `scope` narrowed to the graph view `view`, decided for one snapshot: the scope with
/// the SHA-256 of the view's debug form, which shows each guard's exemptions once they
/// are decided and is the same in every process of one release.
pub(crate) fn view_scope(scope: &str, view: &crate::access::GraphAccess) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(format!("{view:?}").as_bytes());
    let mut out = String::with_capacity(scope.len() + 1 + 64);
    out.push_str(scope);
    out.push('|');
    for b in d {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// The most shards a cache of query results has. quick_cache splits the byte budget
/// evenly between its shards and refuses an entry heavier than 97% of one shard's share.
/// The SERVICE cache and the result cache keep an entry up to an eighth of the whole
/// budget, so every kept entry fits its shard only when a shard holds at least twice
/// that, which means at most four shards. The default of four shards per CPU gave a
/// shard less than an eighth of a small budget on a host with 32 or more CPUs, and the
/// cache dropped entries that `put` had accepted. Four shards cost little, because each
/// lookup takes a read lock and comes once per cached subtree or remote request, not
/// once per row.
pub(crate) const MAX_SHARDS: usize = 4;

/// A cache of at most `bytes` weighed by `weighter`, with `shards` shards but at most
/// [`MAX_SHARDS`].
pub(crate) fn bounded_cache<K, V, W>(
    bytes: u64,
    shards: usize,
    weighter: W,
) -> quick_cache::sync::Cache<K, V, W>
where
    K: Eq + std::hash::Hash,
    V: Clone,
    W: quick_cache::Weighter<K, V> + Clone,
{
    let options = quick_cache::OptionsBuilder::new()
        .estimated_items_capacity(10_000)
        .weight_capacity(bytes)
        .shards(shards.min(MAX_SHARDS))
        .build()
        .expect("the cache options are complete");
    quick_cache::sync::Cache::with_options(
        options,
        weighter,
        Default::default(),
        Default::default(),
    )
}

/// The shards a cache asks for on this host before [`MAX_SHARDS`] applies: four per CPU,
/// quick_cache's default.
pub(crate) fn host_shards() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get()) * 4
}

/// The storage of remote SERVICE results that stores can share.
pub struct ServiceStore {
    cache: Option<Inner>,
    max_entry_bytes: u64,
}

impl ServiceStore {
    /// Storage of at most `bytes` (0 turns it off).
    pub fn new(bytes: u64) -> ServiceStore {
        Self::with_shards(bytes, host_shards())
    }

    fn with_shards(bytes: u64, shards: usize) -> ServiceStore {
        ServiceStore {
            cache: (bytes > 0).then(|| bounded_cache(bytes, shards, Weighter)),
            max_entry_bytes: bytes / 8,
        }
    }

    pub fn capacity(&self) -> u64 {
        self.cache.as_ref().map_or(0, |c| c.capacity())
    }

    pub fn bytes(&self) -> u64 {
        self.cache.as_ref().map_or(0, |c| c.weight())
    }

    pub fn entries(&self) -> usize {
        self.cache.as_ref().map_or(0, |c| c.len())
    }

    /// Bytes and entries by namespace, in one pass.
    pub fn usage(&self) -> std::collections::HashMap<u32, crate::index::NamespaceUsage> {
        let mut out: std::collections::HashMap<u32, crate::index::NamespaceUsage> =
            Default::default();
        if let Some(c) = &self.cache {
            for (k, v) in c.iter() {
                let u = out.entry(k.ns).or_default();
                u.bytes += v.weight();
                u.entries += 1;
            }
        }
        out
    }
}

/// The default time to live of an entry: one hour.
pub const DEFAULT_TTL: Duration = Duration::from_secs(3600);

// A test's reader that runs between the two tiers of a removal, where a concurrent
// lookup could otherwise find the entry in one tier and not the other.
#[cfg(test)]
thread_local! {
    static BETWEEN_TIERS: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_DISK_PUT: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn between_tiers() {
    BETWEEN_TIERS.with(|h| {
        if let Some(f) = h.borrow().as_ref() {
            f()
        }
    })
}

#[cfg(test)]
fn after_disk_put() {
    AFTER_DISK_PUT.with(|h| {
        if let Some(f) = h.borrow().as_ref() {
            f()
        }
    })
}

#[cfg(not(test))]
#[inline(always)]
fn between_tiers() {}

#[cfg(not(test))]
#[inline(always)]
fn after_disk_put() {}

/// A store's view of the cache of remote SERVICE results.
pub struct ServiceCache {
    store: Arc<ServiceStore>,
    ns: u32,
    /// whether other views share the storage
    shared: bool,
    /// the seconds an entry answers after it was stored (0: until it is evicted)
    ttl: AtomicU64,
    /// the entries kept on disk, for a persistent dataset that sets it
    disk: std::sync::RwLock<Option<Arc<super::svcdisk::DiskTier>>>,
    /// the setting of an in-memory store, which has no file to keep it in
    memory_settings: std::sync::Mutex<crate::store::ServiceCacheSettings>,
    /// moves on every removal and change of the disk tier, so that an entry read from
    /// disk before one is not kept in memory after it
    removals: AtomicU64,
    /// held while the dataset's setting changes, so that two changes apply one after
    /// the other
    settings: std::sync::Mutex<()>,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl ServiceCache {
    /// A cache of its own, of at most `bytes` (0 turns it off).
    pub fn new(bytes: u64) -> ServiceCache {
        ServiceCache::view(Arc::new(ServiceStore::new(bytes)), 0, false)
    }

    /// A view of `store` for namespace `ns`.
    pub fn attach(store: &Arc<ServiceStore>, ns: u32) -> ServiceCache {
        ServiceCache::view(store.clone(), ns, true)
    }

    pub(crate) fn view(store: Arc<ServiceStore>, ns: u32, shared: bool) -> ServiceCache {
        ServiceCache {
            store,
            ns,
            shared,
            ttl: AtomicU64::new(DEFAULT_TTL.as_secs()),
            disk: std::sync::RwLock::new(None),
            memory_settings: Default::default(),
            removals: AtomicU64::new(0),
            settings: std::sync::Mutex::new(()),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Whether the cache keeps anything, in memory or on disk.
    pub fn enabled(&self) -> bool {
        self.store.cache.is_some() || self.disk().is_some()
    }

    /// How long an entry answers after it was stored (`Duration::ZERO`: until it is
    /// evicted).
    pub fn ttl(&self) -> Duration {
        Duration::from_secs(self.ttl.load(Ordering::Relaxed))
    }

    pub fn set_ttl(&self, ttl: Duration) {
        self.ttl.store(ttl.as_secs(), Ordering::Relaxed);
    }

    /// The disk tier, if the dataset keeps one.
    pub fn disk(&self) -> Option<Arc<super::svcdisk::DiskTier>> {
        self.disk.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Keep entries in `disk` too, or no longer on disk with `None`. A tier it
    /// replaces is retired ([`DiskTier::retire`](super::svcdisk::DiskTier::retire)), so a
    /// query that still holds it neither writes nor removes a file after this returns.
    pub fn set_disk(&self, disk: Option<Arc<super::svcdisk::DiskTier>>) {
        let old = {
            let mut w = self.disk.write().unwrap_or_else(|e| e.into_inner());
            std::mem::replace(&mut *w, disk.clone())
        };
        if let Some(old) = old
            && !disk.as_ref().is_some_and(|d| Arc::ptr_eq(d, &old))
        {
            old.retire();
        }
        self.removed();
    }

    /// The lock that a change of the dataset's setting holds.
    pub(crate) fn settings_lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.settings.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record a removal between its disk tier and its memory. A removal takes the
    /// entries off disk first, then moves this counter, and then takes them out of
    /// memory. A lookup reads the counter before it reads a file and keeps what it read
    /// in memory only when the counter has not moved, checking again after, so whatever
    /// a lookup keeps during a removal is either on disk after the removal took it off,
    /// which it cannot be, or in memory before the removal empties memory.
    fn removed(&self) {
        self.removals.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn memory_settings(&self) -> crate::store::ServiceCacheSettings {
        self.memory_settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn set_memory_settings(&self, s: crate::store::ServiceCacheSettings) {
        *self
            .memory_settings
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = s;
    }

    fn expired(&self, born: Instant) -> bool {
        let ttl = self.ttl.load(Ordering::Relaxed);
        ttl > 0 && born.elapsed() >= Duration::from_secs(ttl)
    }

    /// The live entry of `key` in memory, removing an expired one.
    fn live(&self, key: &str) -> Option<Arc<Entry>> {
        let c = self.store.cache.as_ref()?;
        let k = NsKeyRef { ns: self.ns, key };
        let e = c.get(&k)?;
        if self.expired(e.born) {
            c.remove_if(&k, |v| Arc::ptr_eq(v, &e));
            return None;
        }
        Some(e)
    }

    /// The solutions of the request `key` with the slice `want`, from an entry in
    /// memory or on disk that holds them. An entry read from disk is kept in memory too.
    pub fn get(&self, key: &str, want: Slice) -> Option<Arc<Rows>> {
        self.get_admitted(key, want, &|_| Ok(())).ok().flatten()
    }

    /// [`ServiceCache::get`], admitting with `admit` what the lookup allocates before it
    /// allocates it: the bytes of a file read from disk and of its decoded rows, and a
    /// copy of the part of an entry that a slice asks for. A refusal of `admit` is the
    /// lookup's error.
    pub fn get_admitted(
        &self,
        key: &str,
        want: Slice,
        admit: &dyn Fn(u64) -> crate::error::Result<()>,
    ) -> crate::error::Result<Option<Arc<Rows>>> {
        let mut r = match self.live(key) {
            Some(e) => e.serve_admitted(want, admit)?,
            None => None,
        };
        let before = self.removals.load(Ordering::SeqCst);
        if r.is_none()
            && let Some(disk) = self.disk()
            && let Some(e) = disk.get(key, self.ttl(), admit)?
        {
            r = e.serve_admitted(want, admit)?;
            // an entry that a removal may have dropped while it was read is not kept,
            // and one kept as a removal ran is dropped again
            if self.removals.load(Ordering::SeqCst) == before {
                let e = Arc::new(e);
                self.keep(e.clone());
                if self.removals.load(Ordering::SeqCst) != before
                    && let Some(c) = &self.store.cache
                {
                    c.remove_if(&NsKeyRef { ns: self.ns, key }, |v| Arc::ptr_eq(v, &e));
                }
            }
        }
        let n = if r.is_some() {
            &self.hits
        } else {
            &self.misses
        };
        n.fetch_add(1, Ordering::Relaxed);
        Ok(r)
    }

    /// Keep `rows`, the response to the request `key` with the slice `slice`, unless
    /// they are larger than an eighth of the budget, or the entry kept for the request
    /// already holds them or holds as many solutions or more. A removal that happens
    /// between the disk write and the memory insert prevents the entry from being kept
    /// in memory.
    pub fn put(&self, key: String, parts: Parts, slice: Slice, rows: Arc<Rows>) {
        if !self.enabled() {
            return;
        }
        if let Some(old) = self.live(&key)
            && (old.range(slice).is_some() || old.rows.rows.len() >= rows.rows.len())
        {
            return;
        }
        let before = self.removals.load(Ordering::SeqCst);
        let e = Arc::new(Entry::new(
            Arc::from(key),
            parts,
            slice,
            rows,
            SystemTime::now(),
        ));
        if let Some(disk) = self.disk() {
            disk.put(&e);
        }
        after_disk_put();
        if self.removals.load(Ordering::SeqCst) != before {
            return;
        }
        self.keep(e.clone());
        if self.removals.load(Ordering::SeqCst) != before
            && let Some(c) = &self.store.cache
        {
            c.remove_if(
                &NsKeyRef {
                    ns: self.ns,
                    key: &e.key,
                },
                |v| Arc::ptr_eq(v, &e),
            );
        }
    }

    /// Keep `e` in memory, if it is small enough.
    fn keep(&self, e: Arc<Entry>) {
        let Some(c) = &self.store.cache else { return };
        if e.weight() > self.store.max_entry_bytes {
            return;
        }
        c.insert(
            SvcKey {
                ns: self.ns,
                key: e.key.clone(),
            },
            e,
        );
    }

    /// The largest entry, with its key, that [`ServiceCache::put`] keeps in memory or on
    /// disk.
    pub(crate) fn max_entry_bytes(&self) -> u64 {
        let disk = self.disk().map_or(0, |d| d.max_entry_bytes());
        self.store.max_entry_bytes.max(disk)
    }

    /// Drop every entry of the request `key`, whatever its slice, on disk and then in
    /// memory (see [`ServiceCache::removed`]). A file of another version stays. A file
    /// that cannot be removed is logged, since the response that follows replaces it.
    pub fn remove(&self, key: &str) {
        if let Some(disk) = self.disk()
            && let Err(e) = disk.remove(key)
        {
            tracing::warn!(error = %e, "a SERVICE cache entry stays on disk");
        }
        self.removed();
        between_tiers();
        if let Some(c) = &self.store.cache {
            c.remove(&NsKeyRef { ns: self.ns, key });
        }
    }

    /// The live entries of `scope` in memory and on disk, by id.
    pub fn list(&self, scope: &str) -> Vec<Listed> {
        self.list_admitted(scope, &|_| Ok(())).unwrap_or_default()
    }

    /// [`ServiceCache::list`], admitting with `admit` the bytes of each listed entry
    /// before it is made and of each file before it is read. A refusal of `admit` is the
    /// listing's error.
    pub fn list_admitted(
        &self,
        scope: &str,
        admit: &dyn Fn(u64) -> crate::error::Result<()>,
    ) -> crate::error::Result<Vec<Listed>> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        if let Some(c) = &self.store.cache {
            for (k, e) in c.iter() {
                if k.ns == self.ns && e.scope() == scope && !self.expired(e.born) {
                    admit(Listed::bytes_of(&e))?;
                    seen.insert(e.key.clone());
                    out.push(Listed::of(&e));
                }
            }
        }
        if let Some(disk) = self.disk() {
            out.extend(disk.list(scope, self.ttl(), &seen, admit)?);
        }
        out.sort_by_key(|l| l.id);
        Ok(out)
    }

    /// Remove the entries of `scope`, or with `id` only the entry with that id, on disk
    /// and then in memory (see [`ServiceCache::removed`]). Returns how many were
    /// removed, once their removal from disk is durable. An entry whose file cannot be
    /// removed stays in memory too and is not counted, and a failed sync of the
    /// directory is the error, after the entries it removed are out of memory too.
    pub fn remove_in_scope(&self, scope: &str, id: Option<u64>) -> crate::error::Result<u64> {
        let removal = self.disk().map(|d| d.remove_in_scope(scope, id));
        self.removed();
        between_tiers();
        let mut n = removal.as_ref().map_or(0, |r| r.count());
        let hit = |e: &Entry| e.scope() == scope && id.is_none_or(|id| e.id == id);
        if let Some(c) = &self.store.cache {
            let keys: Vec<Arc<str>> = c
                .iter()
                .filter(|(k, e)| k.ns == self.ns && hit(e))
                .map(|(k, _)| k.key)
                .collect();
            for key in keys {
                if removal.as_ref().is_some_and(|r| r.stuck(&key)) {
                    continue;
                }
                let k = NsKeyRef {
                    ns: self.ns,
                    key: &key,
                };
                if c.remove_if(&k, |e| hit(e)).is_some()
                    && !removal.as_ref().is_some_and(|r| r.removed(&key))
                {
                    n += 1;
                }
            }
        }
        if let Some(r) = removal {
            r.synced?;
        }
        Ok(n)
    }

    /// Drop this view's entries, on disk and then in memory. Another namespace's stay.
    /// The error is the disk tier's, after memory is emptied too.
    pub fn clear(&self) -> std::io::Result<()> {
        let disk = self.disk().map_or(Ok(()), |d| d.clear());
        self.removed();
        if let Some(c) = &self.store.cache {
            if self.shared {
                let ns = self.ns;
                c.retain(|k, _| k.ns != ns);
            } else {
                c.clear();
            }
        }
        disk
    }

    /// The entries of this view's namespace in memory.
    pub fn entries(&self) -> usize {
        match &self.store.cache {
            Some(c) if self.shared => c.iter().filter(|(k, _)| k.ns == self.ns).count(),
            Some(c) => c.len(),
            None => 0,
        }
    }

    /// The bytes of this view's namespace in memory.
    pub fn bytes(&self) -> u64 {
        match &self.store.cache {
            Some(c) if self.shared => c
                .iter()
                .filter(|(k, _)| k.ns == self.ns)
                .map(|(_, v)| v.weight())
                .sum(),
            Some(c) => c.weight(),
            None => 0,
        }
    }

    /// The bytes the storage may hold, shared with other views or not.
    pub fn capacity(&self) -> u64 {
        self.store.capacity()
    }

    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }
}

struct ByteCounter(usize);

impl Write for ByteCounter {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0 = self.0.saturating_add(s.len());
        Ok(())
    }
}

/// Exact serialized byte length of [`key`] for the same arguments, without allocating.
pub fn key_len(scope: &str, url: &str, pattern: &str, values: &[(&str, &Term)]) -> usize {
    let mut len = scope
        .len()
        .saturating_add(url.len())
        .saturating_add(pattern.len())
        .saturating_add(3);
    for &(v, t) in values {
        let mut c = ByteCounter(0);
        let _ = write!(c, "{t}");
        len = len
            .saturating_add(v.len())
            .saturating_add(c.0)
            .saturating_add(3);
    }
    len
}

struct CheckedWriter<F, E> {
    buf: String,
    admit: F,
    err: Option<E>,
}

impl<F, E> Write for CheckedWriter<F, E>
where
    F: FnMut(usize) -> std::result::Result<(), E>,
{
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        if self.err.is_some() {
            return Err(std::fmt::Error);
        }
        let needed = self.buf.len().saturating_add(s.len());
        if needed > self.buf.capacity() {
            let next_cap = self.buf.capacity().saturating_mul(2).max(needed);
            if let Err(e) = (self.admit)(next_cap) {
                self.err = Some(e);
                return Err(std::fmt::Error);
            }
            self.buf.reserve(needed - self.buf.len());
        }
        self.buf.push_str(s);
        Ok(())
    }
}

/// Build the key of one request, admitting its serialized capacity before allocating the
/// `String` and before any buffer growth while formatting terms.
pub fn try_key<E>(
    scope: &str,
    url: &str,
    pattern: &str,
    values: &[(&str, &Term)],
    mut admit: impl FnMut(usize) -> std::result::Result<(), E>,
) -> std::result::Result<String, E> {
    let exact = key_len(scope, url, pattern, values);
    admit(exact)?;
    let buf = String::with_capacity(exact);
    if buf.capacity() > exact {
        admit(buf.capacity())?;
    }
    let mut w = CheckedWriter {
        buf,
        admit,
        err: None,
    };
    let _ = write!(w, "{scope}\u{0}{url}\u{0}{pattern}\u{0}");
    if let Some(e) = w.err {
        return Err(e);
    }
    if !values.is_empty() {
        let mut vs: Vec<_> = values.to_vec();
        vs.sort_unstable_by(|a, b| a.0.cmp(b.0));
        for (v, t) in vs {
            if write!(w, "?{v}={t};").is_err()
                && let Some(e) = w.err
            {
                return Err(e);
            }
        }
    }
    debug_assert_eq!(w.buf.len(), exact);
    Ok(w.buf)
}

/// The key of one request: the caller's scope, the endpoint, the SERVICE pattern as
/// written and the values substituted into it, by variable name.
pub fn key(scope: &str, url: &str, pattern: &str, values: &[(&str, &Term)]) -> String {
    try_key::<std::convert::Infallible>(scope, url, pattern, values, |_| Ok(()))
        .unwrap_or_else(|e| match e {})
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::{Literal, NamedNode};

    fn parts(scope: &str) -> Parts {
        Parts::of(scope, "http://e/sparql", "?s ?p ?o .")
    }

    fn rows(n: usize) -> Arc<Rows> {
        Arc::new(Rows {
            vars: vec!["x".into()],
            rows: (0..n)
                .map(|i| vec![Some(Term::Literal(Literal::from(i as i64)))])
                .collect(),
        })
    }

    #[test]
    fn bounded_and_keyed() {
        let c = ServiceCache::new(64 << 10);
        let a = NamedNode::new("http://e/a").unwrap().into();
        let k = key("alice", "http://e/sparql", "?s ?p ?o .", &[("s", &a)]);
        assert!(c.get(&k, Slice::ALL).is_none());
        c.put(k.clone(), parts("alice"), Slice::ALL, rows(3));
        assert_eq!(c.get(&k, Slice::ALL).unwrap().rows.len(), 3);
        assert_eq!((c.hits(), c.misses()), (1, 1));
        // another caller's key differs
        let k2 = key("bob", "http://e/sparql", "?s ?p ?o .", &[("s", &a)]);
        assert!(c.get(&k2, Slice::ALL).is_none());
        // an entry over an eighth of the budget is not kept
        let big = key("alice", "http://e/sparql", "big", &[]);
        c.put(big.clone(), parts("alice"), Slice::ALL, rows(2_000));
        assert!(c.get(&big, Slice::ALL).is_none());
        c.remove(&k);
        assert!(c.get(&k, Slice::ALL).is_none());
        assert!(!ServiceCache::new(0).enabled());
    }

    /// The largest entry `put` accepts is kept, whatever number of shards the host asks
    /// for. The default asks for four per CPU, so 128 on a host with 32 CPUs.
    #[test]
    fn entries_up_to_an_eighth_fit_their_shard_on_any_host() {
        let budget = 64u64 << 10;
        let key = "k0".to_string();
        let mut n = 1;
        while rows(n + 1).bytes() + key.len() as u64 + 96 <= budget / 8 {
            n += 1;
        }
        for shards in [1, 4, 16, 64, 128, 256, 1024] {
            let store = Arc::new(ServiceStore::with_shards(budget, shards));
            let c = ServiceCache::view(store, 0, false);
            c.put(key.clone(), Parts::default(), Slice::ALL, rows(n));
            assert!(
                c.get(&key, Slice::ALL).is_some(),
                "{shards} shards dropped the entry"
            );
        }
    }

    #[test]
    fn bounded_shared_storage_keeps_namespaces_isolated() {
        for shards in [1, 4, 16, 64, 128, 256, 1024] {
            let store = Arc::new(ServiceStore::with_shards(64 << 10, shards));
            let one = ServiceCache::attach(&store, 11);
            let two = ServiceCache::attach(&store, 22);
            let first = rows(3);
            let second = rows(4);
            one.put(
                "same key".into(),
                Parts::default(),
                Slice::ALL,
                first.clone(),
            );
            assert!(two.get("same key", Slice::ALL).is_none());
            two.put(
                "same key".into(),
                Parts::default(),
                Slice::ALL,
                second.clone(),
            );
            assert!(Arc::ptr_eq(
                &one.get("same key", Slice::ALL).unwrap(),
                &first
            ));
            assert!(Arc::ptr_eq(
                &two.get("same key", Slice::ALL).unwrap(),
                &second
            ));
            let usage = store.usage();
            assert_eq!(usage[&11].entries, 1);
            assert_eq!(usage[&22].entries, 1);
            assert_eq!(one.bytes() + two.bytes(), store.bytes());
            one.remove("same key");
            assert!(one.get("same key", Slice::ALL).is_none());
            assert!(Arc::ptr_eq(
                &two.get("same key", Slice::ALL).unwrap(),
                &second
            ));
            one.put("same key".into(), Parts::default(), Slice::ALL, first);
            one.clear().unwrap();
            assert_eq!(one.entries(), 0);
            assert_eq!(one.bytes(), 0);
            assert!(Arc::ptr_eq(
                &two.get("same key", Slice::ALL).unwrap(),
                &second
            ));
            assert_eq!(two.entries(), store.entries());
            assert_eq!(two.bytes(), store.bytes());
            assert!(store.bytes() <= store.capacity());
        }
    }

    #[test]
    fn key_ignores_value_order() {
        let a: Term = NamedNode::new("http://e/a").unwrap().into();
        let b: Term = Literal::from(1).into();
        assert_eq!(
            key("", "u", "p", &[("a", &a), ("b", &b)]),
            key("", "u", "p", &[("b", &b), ("a", &a)])
        );
    }

    /// The bytes a row keeps count the box of every triple term, at every level of
    /// nesting, beside the terms' text.
    #[test]
    fn retained_bytes_count_every_boxed_triple() {
        let n = |s: &str| NamedNode::new(s).unwrap();
        let depth = 8;
        let mut t: Term = n("urn:o").into();
        for _ in 0..depth {
            t = Term::Triple(Box::new(oxrdf::Triple::new(n("urn:s"), n("urn:p"), t)));
        }
        let row = vec![Some(t)];
        let text = depth * ("urn:s".len() + "urn:p".len()) + "urn:o".len();
        let at_least = std::mem::size_of::<Option<Term>>()
            + std::mem::size_of::<Vec<Option<Term>>>()
            + depth * std::mem::size_of::<oxrdf::Triple>()
            + text;
        assert!(
            Rows::retained_row_bytes(&row) >= at_least as u64,
            "{} < {at_least}",
            Rows::retained_row_bytes(&row)
        );
    }

    fn numbers(r: &Rows) -> Vec<i64> {
        r.rows
            .iter()
            .map(|r| match &r[0] {
                Some(Term::Literal(l)) => l.value().parse().unwrap(),
                _ => -1,
            })
            .collect()
    }

    /// An entry answers its own request, and every request with a limit whose solutions
    /// are all among its rows. A response shorter than its limit, or one without a
    /// limit, is not taken as the end of the result, since an endpoint may cap its
    /// responses without saying so.
    #[test]
    fn slices_are_served_from_the_rows_an_entry_holds() {
        let c = ServiceCache::new(1 << 20);
        let k = key("s", "http://e/sparql", "p", &[]);
        let at = |o, l| Slice {
            offset: o,
            limit: l,
        };
        // the first 10 of a longer result
        c.put(
            k.clone(),
            Parts::of("s", "http://e/sparql", "p"),
            at(0, Some(10)),
            rows(10),
        );
        assert_eq!(
            numbers(&c.get(&k, at(0, Some(10))).unwrap()),
            (0..10).collect::<Vec<_>>()
        );
        assert_eq!(numbers(&c.get(&k, at(2, Some(3))).unwrap()), vec![2, 3, 4]);
        assert_eq!(numbers(&c.get(&k, at(0, Some(1))).unwrap()), vec![0]);
        assert!(c.get(&k, at(5, Some(6))).is_none(), "past the rows held");
        assert!(c.get(&k, Slice::ALL).is_none(), "not the whole result");
        assert!(c.get(&k, at(3, None)).is_none());
        // a response of fewer rows than its limit answers its own request and the slices
        // inside its rows, and nothing past them
        let k2 = key("s", "http://e/sparql", "q", &[]);
        c.put(
            k2.clone(),
            Parts::of("s", "http://e/sparql", "q"),
            at(0, Some(10)),
            rows(4),
        );
        assert_eq!(
            numbers(&c.get(&k2, at(0, Some(10))).unwrap()),
            vec![0, 1, 2, 3]
        );
        assert_eq!(numbers(&c.get(&k2, at(1, Some(2))).unwrap()), vec![1, 2]);
        assert!(c.get(&k2, Slice::ALL).is_none());
        assert!(c.get(&k2, at(2, Some(100))).is_none());
        assert!(c.get(&k2, at(9, Some(1))).is_none());
        // so does a response without a limit
        let k4 = key("s", "http://e/sparql", "t", &[]);
        c.put(
            k4.clone(),
            Parts::of("s", "http://e/sparql", "t"),
            Slice::ALL,
            rows(4),
        );
        assert_eq!(c.get(&k4, Slice::ALL).unwrap().rows.len(), 4);
        assert_eq!(numbers(&c.get(&k4, at(3, Some(1))).unwrap()), vec![3]);
        assert!(c.get(&k4, at(4, Some(1))).is_none());
        assert!(c.get(&k4, at(2, None)).is_none());
        // an entry from an offset answers nothing before it
        let k3 = key("s", "http://e/sparql", "r", &[]);
        c.put(
            k3.clone(),
            Parts::of("s", "http://e/sparql", "r"),
            at(5, Some(2)),
            rows(2),
        );
        assert!(c.get(&k3, at(4, Some(1))).is_none());
        assert_eq!(numbers(&c.get(&k3, at(6, Some(1))).unwrap()), vec![1]);
        // a smaller response does not replace an entry that holds it
        c.put(
            k.clone(),
            Parts::of("s", "http://e/sparql", "p"),
            at(0, Some(3)),
            rows(3),
        );
        assert_eq!(c.get(&k, at(0, Some(10))).unwrap().rows.len(), 10);
        // nor one it does not hold but with more rows
        c.put(
            k.clone(),
            Parts::of("s", "http://e/sparql", "p"),
            at(50, Some(2)),
            rows(2),
        );
        assert_eq!(c.get(&k, at(0, Some(10))).unwrap().rows.len(), 10);
        assert!(c.get(&k, at(50, Some(2))).is_none());
        // a larger one does
        c.put(
            k.clone(),
            Parts::of("s", "http://e/sparql", "p"),
            at(0, Some(20)),
            rows(20),
        );
        assert_eq!(c.get(&k, at(0, Some(20))).unwrap().rows.len(), 20);
    }

    /// An entry answers for its time to live and no longer, and a zero time to live
    /// keeps it until it is evicted.
    #[test]
    fn entries_expire_after_their_time_to_live() {
        let c = ServiceCache::new(1 << 20);
        let k = key("s", "http://e/sparql", "p", &[]);
        let p = Parts::of("s", "http://e/sparql", "p");
        let old = SystemTime::now() - Duration::from_secs(120);
        c.keep(Arc::new(Entry::new(
            Arc::from(k.as_str()),
            p,
            Slice::ALL,
            rows(2),
            old,
        )));
        c.set_ttl(Duration::from_secs(0));
        assert!(c.get(&k, Slice::ALL).is_some());
        c.set_ttl(Duration::from_secs(600));
        assert!(c.get(&k, Slice::ALL).is_some());
        c.set_ttl(Duration::from_secs(60));
        assert!(c.get(&k, Slice::ALL).is_none());
        assert_eq!(c.entries(), 0, "the expired entry is removed");
    }

    /// Listing and removal compare the scope as a whole, so a scope that is a prefix of
    /// another, or holds the key's separator, never reaches the other's entries.
    #[test]
    fn listing_and_removal_stay_in_the_scope() {
        let c = ServiceCache::new(1 << 20);
        let url = "http://user:secret@e/sparql";
        for scope in ["a", "a\u{0}http://user:secret@e/sparql", "ab"] {
            let k = key(scope, url, "p", &[]);
            c.put(k, Parts::of(scope, url, "p"), Slice::ALL, rows(1));
        }
        let mine = c.list("a");
        assert_eq!(mine.len(), 1);
        assert_eq!(
            mine[0].endpoint, "http://e/sparql",
            "credentials are not listed"
        );
        assert_eq!(mine[0].pattern, "p");
        assert_eq!(
            c.remove_in_scope("a", Some(mine[0].id + 1_000_000))
                .unwrap(),
            0
        );
        assert_eq!(c.remove_in_scope("a", None).unwrap(), 1);
        assert_eq!(c.entries(), 2);
        assert_eq!(c.list("ab").len(), 1);
        let other = c.list("ab")[0].id;
        assert_eq!(
            c.remove_in_scope("a", Some(other)).unwrap(),
            0,
            "another scope's id"
        );
        assert_eq!(c.remove_in_scope("ab", Some(other)).unwrap(), 1);
        assert_eq!(c.entries(), 1);
    }

    #[test]
    fn credentials_are_left_out_of_listed_endpoints() {
        assert_eq!(listed_endpoint("https://u:p@h:1/x?a@b"), "https://h:1/x");
        assert_eq!(listed_endpoint("https://h/x?key=secret#t"), "https://h/x");
        assert_eq!(listed_endpoint("https://u@h#f"), "https://h/");
        assert_eq!(listed_endpoint("https://h/x@y"), "https://h/x@y");
        assert_eq!(listed_endpoint("urn:x?y"), UNLISTED_ENDPOINT);
    }

    /// Fictional credentials in every form of an HTTP URL that the outbound client
    /// accepts are left out of what a listing shows and the disk tier writes.
    #[test]
    fn credentials_never_reach_listings_in_any_accepted_form() {
        for url in [
            "http://u5er:s3cret@h/x",
            "http:/u5er:s3cret@h/x",
            "http:u5er:s3cret@h/x",
            "http:///u5er:s3cret@h/x",
            "HTTP://u5er:s3cret@h/x",
            "http:\\\\u5er:s3cret@h/x",
            "https:\\u5er:s3cret@h:8443/x",
            " http://u5er:s3cret@h/x",
            "http://u5er:s3cret@h",
            "https://h/x?token=s3cret",
            "https://h/x#s3cret",
            "http://u5er:s3cret@[::1/x",
            "u5er:s3cret@h/x",
        ] {
            let shown = listed_endpoint(url);
            assert!(
                !shown.contains("s3cret") && !shown.contains("u5er"),
                "{url} is listed as {shown}"
            );
        }
        assert_eq!(listed_endpoint("https://h/x@y"), "https://h/x@y");
    }

    fn set_between_tiers(f: Option<Box<dyn Fn()>>) {
        BETWEEN_TIERS.with(|h| *h.borrow_mut() = f);
    }

    /// A lookup that runs while a removal is between its tiers does not keep the entry
    /// in memory after the removal returns (Neon round-1 P1-2).
    #[test]
    fn a_removal_is_not_undone_by_a_lookup_between_its_tiers() {
        let dir = tempfile::tempdir().unwrap();
        let c = Arc::new(ServiceCache::new(1 << 20));
        c.set_disk(Some(Arc::new(
            super::super::svcdisk::DiskTier::open(dir.path().join("svc"), 1 << 20).unwrap(),
        )));
        let url = "http://e/sparql";
        for by_scope in [true, false] {
            let k = key("a", url, "p", &[]);
            c.put(k.clone(), Parts::of("a", url, "p"), Slice::ALL, rows(3));
            assert!(c.get(&k, Slice::ALL).is_some());
            let (reader, rk) = (c.clone(), k.clone());
            set_between_tiers(Some(Box::new(move || {
                let _ = reader.get(&rk, Slice::ALL);
            })));
            if by_scope {
                assert_eq!(c.remove_in_scope("a", None).unwrap(), 1);
            } else {
                c.remove(&k);
            }
            set_between_tiers(None);
            assert!(
                c.get(&k, Slice::ALL).is_none(),
                "by scope {by_scope}: the removed entry came back"
            );
            assert!(c.list("a").is_empty(), "by scope {by_scope}");
        }
        // a lookup that read the file just before a removal keeps nothing either
        let k = key("a", url, "q", &[]);
        c.put(k.clone(), Parts::of("a", url, "q"), Slice::ALL, rows(3));
        let cold = ServiceCache::new(1 << 20);
        cold.set_disk(c.disk());
        let calls = std::cell::Cell::new(0);
        let r = cold.get_admitted(&k, Slice::ALL, &|_| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                assert_eq!(cold.remove_in_scope("a", None).unwrap(), 1);
            }
            Ok(())
        });
        assert!(r.is_ok());
        assert!(cold.get(&k, Slice::ALL).is_none(), "kept after its removal");
    }

    /// The refresh that `cache+clear` makes, a removal of the request's entry and a
    /// write of the new response, leaves a file of a newer version alone (Sol round-1
    /// #8), and a removal by scope does too.
    #[test]
    fn a_refresh_keeps_a_newer_file() {
        use super::super::svcdisk::DiskTier;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc");
        let url = "http://e/sparql";
        let k = key("a", url, "p", &[]);
        let c = ServiceCache::new(1 << 20);
        c.set_disk(Some(Arc::new(
            DiskTier::open(path.clone(), 1 << 20).unwrap(),
        )));
        c.put(k.clone(), Parts::of("a", url, "p"), Slice::ALL, rows(2));
        let file = std::fs::read_dir(&path)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut newer = std::fs::read(&file).unwrap();
        newer[8..12].copy_from_slice(&(super::super::svcdisk::VERSION + 1).to_le_bytes());
        std::fs::write(&file, &newer).unwrap();
        let c = ServiceCache::new(1 << 20);
        c.set_disk(Some(Arc::new(
            DiskTier::open(path.clone(), 1 << 20).unwrap(),
        )));
        assert!(c.get(&k, Slice::ALL).is_none());
        c.remove(&k);
        c.put(k.clone(), Parts::of("a", url, "p"), Slice::ALL, rows(3));
        assert_eq!(
            c.remove_in_scope("a", None).unwrap(),
            1,
            "the entry in memory"
        );
        assert_eq!(
            std::fs::read(&file).unwrap(),
            newer,
            "the newer file is kept"
        );
    }

    fn set_after_disk_put(f: Option<Box<dyn Fn()>>) {
        AFTER_DISK_PUT.with(|h| *h.borrow_mut() = f);
    }

    /// A scoped removal that runs between a writer's disk publication and its memory
    /// insert prevents the writer from resurrecting the removed entry (Sol round-2 P2-1).
    #[test]
    fn a_removal_during_put_does_not_resurrect_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let url = "http://e/sparql";
        for by_scope in [true, false] {
            let c = Arc::new(ServiceCache::new(1 << 20));
            let disk = Arc::new(
                super::super::svcdisk::DiskTier::open(
                    dir.path().join(format!("svc-{by_scope}")),
                    1 << 20,
                )
                .unwrap(),
            );
            c.set_disk(Some(disk));
            let k = key("a", url, "race", &[]);
            let remover = c.clone();
            let rk = k.clone();
            set_after_disk_put(Some(Box::new(move || {
                if by_scope {
                    let _ = remover.remove_in_scope("a", None);
                } else {
                    remover.remove(&rk);
                }
            })));
            c.put(k.clone(), Parts::of("a", url, "race"), Slice::ALL, rows(3));
            set_after_disk_put(None);
            assert!(
                c.get(&k, Slice::ALL).is_none(),
                "by scope {by_scope}: the removed entry was resurrected by put"
            );
        }
    }
}
