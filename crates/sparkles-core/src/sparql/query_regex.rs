//! Query-owned regular expressions with explicitly bounded, charged search caches. Both
//! accounting modes compile `REGEX`, `REPLACE` and key-filter patterns here, once per
//! query. A query charges each program it uses as if it had compiled it, and its search
//! caches go with the query.
//!
//! Compiled programs, without their search caches, are also kept in one process-wide
//! table of at most [`PROGRAMS_MAX`] programs and [`PROGRAMS_BYTES`] bytes, so that a
//! pattern that queries repeat compiles once. Compilation takes about as long as a
//! filter over 10,000 keys, and the table holds at most 16 MiB outside every query's
//! budget for that time. When a query ends, the workers' clones of a kept program, with
//! their warm search caches, wait in the table for the next query that searches the same
//! program on the same threads, up to [`PARKED_BYTES`] of their charged bounds. A cold
//! cache cost a 1 ms filter over 100,000 IRIs about 4% on 12 threads.
//!
//! A program has one search cache that any thread can use behind a lock, and a clone of
//! the program for each Rayon worker that searches it, with the worker's own cache. A
//! worker's clone searches without a lock, which keeps a parallel filter as fast as one
//! regular expression per thread. Each clone's cache is charged when it is made, and a
//! worker whose cache does not fit shares the locked one, as do threads outside Rayon.

use super::ctx::{Ctx, RetainedCharge};
use crate::error::Result;
use arc_swap::ArcSwap;
use parking_lot::Mutex;
use regex_automata::{
    Input,
    meta::{Cache, Regex},
    util::iter::Searcher,
};
use rustc_hash::FxHashMap;
use std::sync::{Arc, OnceLock};

thread_local! {
    /// A number for this thread, unique in the process, which tells a worker's clone of
    /// a program from the thread that made it without the reference count of
    /// `std::thread::current()`.
    static THREAD: usize = {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    };
}

/// The programs the process-wide table keeps. Reaching either bound starts the table over.
const PROGRAMS_MAX: usize = 64;
/// The program bytes the process-wide table keeps.
const PROGRAMS_BYTES: usize = 16 << 20;
/// The charged cache bounds of the worker clones that wait in the table between queries.
const PARKED_BYTES: usize = 16 << 20;

/// A compiled program with the bounds it was built under. A query whose own bound is at
/// least `nfa` would have built the same program.
#[derive(Clone)]
struct Program {
    program: Regex,
    nfa: usize,
    hybrid: usize,
}

/// A kept program and the workers' clones of it that wait for the next query, by the
/// thread that made each clone, which is the only thread its pool serves, with the
/// charged bound of its cache.
struct Kept {
    program: Program,
    parked: FxHashMap<usize, (Regex, usize)>,
}

#[derive(Default)]
struct Programs {
    by_text: FxHashMap<Box<str>, Kept>,
    bytes: usize,
    parked_bytes: usize,
}

impl Programs {
    /// The kept entry of `text` when its program is `program` itself rather than another
    /// compilation of the same text.
    fn same(&mut self, text: &str, program: &Regex) -> Option<&mut Kept> {
        self.by_text
            .get_mut(text)
            .filter(|kept| std::ptr::eq(kept.program.program.group_info(), program.group_info()))
    }
}

fn programs() -> &'static Mutex<Programs> {
    static PROGRAMS: OnceLock<Mutex<Programs>> = OnceLock::new();
    PROGRAMS.get_or_init(Mutex::default)
}

/// The kept program of `text` when a query bounded by `maximum` would have built it. The
/// clone shares the compiled program and makes its own search caches.
fn kept_program(text: &str, maximum: usize) -> Option<Program> {
    let programs = programs().lock();
    let kept = &programs.by_text.get(text)?.program;
    (kept.nfa <= maximum).then(|| kept.clone())
}

/// The parked clone of `program` that `thread` made for an earlier query, with its warm
/// search cache.
fn take_parked(text: &str, program: &Regex, thread: usize) -> Option<Regex> {
    let mut programs = programs().lock();
    let (clone, bytes) = programs.same(text, program)?.parked.remove(&thread)?;
    programs.parked_bytes -= bytes;
    Some(clone)
}

/// Park a worker's clone of a kept program for the next query, unless the table no
/// longer keeps that program, already holds a clone for `thread` or has no room.
fn park(text: &str, clone: Regex, thread: usize, bytes: usize) {
    let mut programs = programs().lock();
    if programs.parked_bytes + bytes > PARKED_BYTES {
        return;
    }
    let Some(kept) = programs.same(text, &clone) else {
        return;
    };
    if kept.parked.contains_key(&thread) {
        return;
    }
    kept.parked.insert(thread, (clone, bytes));
    programs.parked_bytes += bytes;
}

fn keep_program(text: &str, program: &Program) {
    let bytes = program.program.memory_usage() + text.len();
    if bytes > PROGRAMS_BYTES / 4 {
        return;
    }
    let mut programs = programs().lock();
    if programs.by_text.contains_key(text) {
        return;
    }
    if programs.by_text.len() >= PROGRAMS_MAX || programs.bytes + bytes > PROGRAMS_BYTES {
        *programs = Programs::default();
    }
    programs.bytes += bytes;
    programs.by_text.insert(
        text.into(),
        Kept {
            program: program.clone(),
            parked: FxHashMap::default(),
        },
    );
}

/// The patterns a query keeps at once. Reaching it starts the cache over, and the
/// programs go once the last search that holds one ends.
const MAX_PATTERNS: usize = 64;

/// Patterns and flags are shared with every copy of the map, so a copy holds only its
/// tables, at most [`MAX_PATTERNS`] entries of pointers. Each entry's charge covers its
/// strings.
type Entries = FxHashMap<Arc<str>, FxHashMap<Arc<str>, Arc<Entry>>>;

/// A query's compiled patterns by pattern and flags. A lookup reads the current map
/// without a lock, and an insert replaces the whole map, which holds at most
/// [`MAX_PATTERNS`] entries.
pub(super) struct RegexCache {
    /// Made on the first lookup. Most queries compile no pattern, and they neither
    /// allocate a map nor, when the context drops, pay the debts of every thread that
    /// ever read one.
    entries: std::sync::OnceLock<ArcSwap<Entries>>,
    /// serializes compilations, so that each pattern compiles once
    insert: Mutex<()>,
}

impl Default for RegexCache {
    fn default() -> Self {
        Self {
            entries: std::sync::OnceLock::new(),
            insert: Mutex::new(()),
        }
    }
}

struct Entry {
    /// `None` for a pattern or flags that do not compile
    state: Option<Arc<RegexState>>,
    _charge: Option<RetainedCharge>,
}

impl RegexCache {
    pub(super) fn clear(&self) {
        let _insert = self.insert.lock();
        // Every query clears its cache when it is shared and when its work ends, and
        // most queries never compile a pattern. Replacing the map pays the debts of every
        // thread that ever read one, about a thousand instructions, so an empty map stays.
        // Inserts take the same lock, so none can land between the check and the store.
        let Some(entries) = self.entries.get() else {
            return;
        };
        if entries.load().is_empty() {
            return;
        }
        entries.store(Arc::default());
    }

    fn entries(&self) -> &ArcSwap<Entries> {
        self.entries
            .get_or_init(|| ArcSwap::from_pointee(Entries::default()))
    }

    /// Run `f` on the program of `pattern` and `flags`, compiling and charging it on
    /// first use. `Ok(None)` for a pattern or flags that do not compile.
    #[inline]
    pub(super) fn with<R>(
        &self,
        ctx: &Ctx,
        pattern: &str,
        flags: &str,
        f: impl FnOnce(&RegexState) -> R,
    ) -> Result<Option<R>> {
        if let Some(entries) = self.entries.get() {
            let entries = entries.load();
            if let Some(entry) = entries.get(pattern).and_then(|e| e.get(flags)) {
                return Ok(entry.state.as_deref().map(f));
            }
        }
        Ok(self.get(ctx, pattern, flags)?.as_deref().map(f))
    }

    /// The program of `pattern` and `flags`, compiled and charged on first use.
    pub(super) fn get(
        &self,
        ctx: &Ctx,
        pattern: &str,
        flags: &str,
    ) -> Result<Option<Arc<RegexState>>> {
        let entries = self.entries();
        if let Some(entry) = entries.load().get(pattern).and_then(|e| e.get(flags)) {
            return Ok(entry.state.clone());
        }
        ctx.check()?;
        let _insert = self.insert.lock();
        let current = entries.load_full();
        if let Some(entry) = current.get(pattern).and_then(|e| e.get(flags)) {
            return Ok(entry.state.clone());
        }
        // A full cache starts over before the new program is admitted, so that the old
        // programs' charges go once no search holds them.
        let mut next = if current.values().map(FxHashMap::len).sum::<usize>() >= MAX_PATTERNS {
            drop(current);
            entries.store(Arc::default());
            Entries::default()
        } else {
            (*current).clone()
        };
        let charge = ctx.retained_charge((pattern.len() + flags.len()) as u64 * 2 + 1024)?;
        let state = RegexState::compile(Some(ctx), pattern, flags)?.map(Arc::new);
        let pattern: Arc<str> = next
            .get_key_value(pattern)
            .map_or_else(|| pattern.into(), |(k, _)| k.clone());
        next.entry(pattern).or_default().insert(
            flags.into(),
            Arc::new(Entry {
                state: state.clone(),
                _charge: charge,
            }),
        );
        entries.store(Arc::new(next));
        Ok(state)
    }
}

/// A compiled pattern with its search caches.
pub(super) struct RegexState {
    program: Regex,
    /// whether the pattern matches the empty string, which `REPLACE` refuses
    matches_empty: bool,
    /// the cache any thread can search with, behind a lock
    shared: Mutex<Cache>,
    /// the charged bound of one search cache
    cache_bytes: u64,
    /// one clone of the program per Rayon worker of the pool that compiled it, each made
    /// on first use
    workers: Box<[OnceLock<Worker>]>,
    /// the text of the program in the process-wide table, whose workers' clones are
    /// parked there when the query ends
    text: Box<str>,
    _charge: Option<RetainedCharge>,
    _shared_charge: Option<RetainedCharge>,
}

impl Drop for RegexState {
    fn drop(&mut self) {
        let bytes = usize::try_from(self.cache_bytes).unwrap_or(usize::MAX);
        for slot in self.workers.iter_mut() {
            if let Some(Worker {
                thread,
                program: Some(program),
                ..
            }) = slot.take()
            {
                park(&self.text, program, thread, bytes);
            }
        }
    }
}

/// A worker's clone of a program, whose own pool keeps the worker's search cache, or
/// `None` when the cache did not fit and the worker shares the locked one.
struct Worker {
    /// the thread that made the clone, the only one that searches with it, so that its
    /// pool only ever holds the one cache of its owner thread
    thread: usize,
    program: Option<Regex>,
    _charge: Option<RetainedCharge>,
}

/// A key filter's reference to a program.
#[derive(Clone)]
pub(super) struct RegexHandle(Arc<RegexState>);

impl RegexHandle {
    #[inline]
    pub(super) fn is_match(&self, text: &str) -> bool {
        self.0.is_match(text)
    }

    /// The program this thread searches with, found once for many searches.
    #[inline]
    pub(super) fn matcher(&self) -> Matcher<'_> {
        self.0.matcher()
    }
}

/// A program as one thread searches it: the thread's worker clone, or the locked cache.
/// Finding the clone takes the thread's Rayon index, its number and the clone's slot,
/// which cost a filter over 100,000 IRIs about 6% when it was done for each key. A matcher
/// stays on the thread that made it, so that no clone's pool makes a cache for a second
/// thread.
pub(super) struct Matcher<'a> {
    state: &'a RegexState,
    own: Option<&'a Regex>,
    _thread: std::marker::PhantomData<*const ()>,
}

impl Matcher<'_> {
    #[inline]
    pub(super) fn is_match(&self, text: &str) -> bool {
        self.state.search(self.own, text)
    }
}

impl RegexState {
    /// Compile `pattern` with the SPARQL `flags`. `Ok(None)` when they do not compile.
    /// A context bounds the program by what its budget leaves and charges it. Without
    /// one the program is bounded as a context with room would bound it, uncharged.
    pub(super) fn compile(ctx: Option<&Ctx>, pattern: &str, flags: &str) -> Result<Option<Self>> {
        let base = (pattern.len() + flags.len()) as u64 * 256 + 4096;
        let available = ctx.map_or(u64::MAX, Ctx::memory_remaining);
        let maximum = ((available.saturating_sub(base) / 48).min(4 << 20)) as usize;
        let exceeded = |ctx: Option<&Ctx>| {
            let ctx = ctx.expect("only a budget limits a program below its maximum");
            Err(ctx.memory_exceeded(ctx.mem_limit.saturating_add(1)))
        };
        if maximum < 1024 {
            return exceeded(ctx);
        }
        // Compilation may hold AST/HIR, forward/reverse NFAs and construction
        // buffers simultaneously. Reserve a conservative envelope up front.
        let text_charge = ctx.map(|ctx| ctx.charge(base)).transpose()?;
        let mut text = String::new();
        let mut literal = false;
        for f in flags.chars() {
            match f {
                'i' => text.push_str("(?i)"),
                's' => text.push_str("(?s)"),
                'm' => text.push_str("(?m)"),
                'x' => text.push_str("(?x)"),
                'q' => literal = true,
                _ => return Ok(None),
            }
        }
        if literal {
            text.push_str(&regex::escape(pattern));
        } else {
            text.push_str(pattern);
        }
        let retained = |bytes: u64| match ctx {
            Some(ctx) => ctx.retained_charge(bytes),
            None => Ok(None),
        };
        let check = || ctx.map_or(Ok(()), Ctx::check);
        let mut nfa = maximum.min(pattern.len().saturating_mul(128).saturating_add(8192));
        let kept = kept_program(&text, maximum);
        let (program, hybrid, mut charge) = loop {
            if let Some(kept) = &kept {
                let charge = retained(kept.program.memory_usage() as u64 + 4096)?;
                break (kept.program.clone(), kept.hybrid, charge);
            }
            check()?;
            let hybrid = (nfa * 2).min(2 << 20);
            let charge = retained(nfa as u64 * 32 + hybrid as u64 * 4)?;
            let built = Regex::builder()
                .configure(
                    Regex::config()
                        .nfa_size_limit(Some(nfa))
                        .hybrid_cache_capacity(hybrid)
                        // These alternative engines retain additional state; the hybrid
                        // DFA and PikeVM suffice and have explicitly budgeted caches.
                        .dfa(false)
                        // A worker's clone keeps only its owner thread's cache, which is
                        // charged when the clone is made.
                        .pool_capacity(0)
                        .onepass(false)
                        .backtrack(false),
                )
                .build(&text);
            match built {
                Ok(program) => {
                    keep_program(
                        &text,
                        &Program {
                            program: program.clone(),
                            nfa,
                            hybrid,
                        },
                    );
                    break (program, hybrid, charge);
                }
                Err(e) if e.size_limit().is_some() && nfa < maximum => {
                    drop(charge);
                    nfa = nfa.saturating_mul(2).min(maximum);
                }
                Err(e) if e.size_limit().is_some() && maximum < 4 << 20 => return exceeded(ctx),
                Err(_) => return Ok(None),
            }
        };
        // PikeVM capture slots grow with both states and capture groups. Include
        // that product before creating the explicit cache, not just NFA bytes.
        let cache_bytes = (program.memory_usage() as u64).saturating_mul(
            (program.captures_len() as u64)
                .saturating_mul(8)
                .saturating_add(16),
        ) + hybrid as u64 * 2
            + program.captures_len() as u64 * 512
            + 4096;
        if let Some(charge) = &mut charge {
            charge.resize(program.memory_usage() as u64 + 4096)?;
        }
        let shared_charge = retained(cache_bytes)?;
        let mut cache = program.create_cache();
        let matches_empty = program
            .search_with(&mut cache, &Input::new("").earliest(true))
            .is_some();
        drop(text_charge);
        check()?;
        let workers = (0..rayon::current_num_threads())
            .map(|_| OnceLock::new())
            .collect();
        Ok(Some(Self {
            program,
            matches_empty,
            shared: Mutex::new(cache),
            cache_bytes,
            workers,
            text: text.into_boxed_str(),
            _charge: charge,
            _shared_charge: shared_charge,
        }))
    }

    pub(super) fn handle(self: &Arc<Self>) -> RegexHandle {
        RegexHandle(self.clone())
    }

    pub(super) fn matches_empty(&self) -> bool {
        self.matches_empty
    }

    /// This Rayon worker's clone of the program, made on its first search, or `None` when
    /// the thread shares the locked cache. A thread outside Rayon, such as the one that
    /// pulls a cursor's batches and may change from batch to batch, and a worker of
    /// another pool whose index collides with a clone's maker, share the locked cache, so
    /// no clone's pool makes a cache for a second thread.
    #[inline]
    fn worker(&self) -> Option<&Regex> {
        let slot = self.workers.get(rayon::current_thread_index()?)?;
        let thread = THREAD.with(|t| *t);
        let worker = slot.get_or_init(|| self.new_worker(thread));
        if worker.thread != thread {
            return None;
        }
        worker.program.as_ref()
    }

    #[cold]
    fn new_worker(&self, thread: usize) -> Worker {
        // A clone that this thread made for an earlier query keeps its warm cache.
        let clone = || {
            take_parked(&self.text, &self.program, thread).unwrap_or_else(|| self.program.clone())
        };
        // A program compiled without an owner is uncharged, and so are its clones.
        let Some(charge) = &self._charge else {
            return Worker {
                thread,
                program: Some(clone()),
                _charge: None,
            };
        };
        let refused = Worker {
            thread,
            program: None,
            _charge: None,
        };
        let Some(ctx) = charge.owner() else {
            return refused;
        };
        // Worker caches are optional. Keep most of the query's allowance for its
        // inputs, outputs and scratch rather than spending it on faster searches.
        let room = ctx.mem_limit > 1 << 20
            && ctx.memory_remaining().saturating_sub(self.cache_bytes) > ctx.mem_limit / 4 * 3;
        match room.then(|| ctx.retained_charge(self.cache_bytes)) {
            Some(Ok(charge)) => Worker {
                thread,
                program: Some(clone()),
                _charge: charge,
            },
            _ => refused,
        }
    }

    #[inline]
    pub(super) fn is_match(&self, text: &str) -> bool {
        self.search(self.worker(), text)
    }

    /// [`RegexState::is_match`] with this thread's worker clone found once, for a run of
    /// searches on this thread.
    #[inline]
    pub(super) fn matcher(&self) -> Matcher<'_> {
        Matcher {
            state: self,
            own: self.worker(),
            _thread: std::marker::PhantomData,
        }
    }

    /// Whether `text` matches, searched with `own`, this thread's worker clone, or with
    /// the locked cache.
    #[inline]
    fn search(&self, own: Option<&Regex>, text: &str) -> bool {
        let input = Input::new(text).earliest(true);
        match own {
            Some(program) => program.is_match(input),
            None => self
                .program
                .search_with(&mut self.shared.lock(), &input)
                .is_some(),
        }
    }

    /// `text` with every match of the program replaced by `replacement`, in which `$n`
    /// names a group, with the output charged as it grows.
    pub(super) fn replace(&self, ctx: &Ctx, text: &str, replacement: &str) -> Result<String> {
        let worker = self.worker();
        let mut shared = worker.is_none().then(|| self.shared.lock());
        let mut captures = self.program.create_captures();
        let mut searcher = Searcher::new(Input::new(text));
        let charge = ctx.charge(1024)?;
        // `$` starts every group reference, and `$$` counts twice, which only overstates
        let references = replacement.bytes().filter(|&b| b == b'$').count();
        let mut reserved = 0u64;
        let mut output = String::new();
        let mut end = 0;
        while let Some(found) = searcher.advance(|input| {
            match (worker, shared.as_deref_mut()) {
                (Some(program), _) => program.search_captures(input, &mut captures),
                (None, Some(cache)) => {
                    self.program
                        .search_captures_with(cache, input, &mut captures)
                }
                (None, None) => unreachable!("a search without a worker holds the shared cache"),
            }
            Ok(captures.get_match())
        }) {
            ctx.check()?;
            // A group lies inside its match, so each reference in the replacement adds
            // at most the match's length to the replacement's own bytes. Reserve that
            // bound before interpolation and before growing the output.
            let need = output
                .len()
                .saturating_add(found.start() - end)
                .saturating_add(replacement.len())
                .saturating_add(references.saturating_mul(found.len()));
            let bytes = (need as u64).saturating_mul(2);
            charge.add(bytes.saturating_sub(reserved))?;
            reserved = reserved.max(bytes);
            // Amortized growth keeps the capacity within twice the bound, which the
            // charge covers.
            output.reserve(need.saturating_sub(output.len()));
            output.push_str(&text[end..found.start()]);
            captures.interpolate_string_into(text, replacement, &mut output);
            end = found.end();
        }
        let need = output.len().saturating_add(text.len() - end);
        charge.add((need as u64 * 2).saturating_sub(reserved))?;
        output.reserve_exact(need - output.len());
        output.push_str(&text[end..]);
        // The expression's returned value remains live after this helper.
        if !ctx.reserve_decode(output.capacity()) {
            ctx.check()?;
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The query-owned program answers as `regex::Regex` does for the same flags, on the
    /// calling thread and on Rayon workers with their own caches.
    #[test]
    fn query_owned_programs_match_the_regex_crate() {
        let cases: [(&str, &str, &[&str]); 7] = [
            ("adá", "i", &["Adá", "ada", "ADÁ", "αβ", ""]),
            ("^a.b$", "s", &["a\nb", "axb", "ab"]),
            ("^b", "m", &["a\nb", "ab"]),
            ("a b", "x", &["ab", "a b"]),
            (".*", "q", &["a.*b", "ab"]),
            (
                "person/1[0-9]{3}$",
                "",
                &["http://e/person/1234", "http://e/person/12345", "x"],
            ),
            ("\\w+", "", &["α", " ", ""]),
        ];
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(3)
            .build()
            .unwrap();
        for (pattern, flags, texts) in cases {
            let mut text = String::new();
            for f in flags.chars().filter(|&f| f != 'q') {
                text.push_str(&format!("(?{f})"));
            }
            if flags.contains('q') {
                text.push_str(&regex::escape(pattern));
            } else {
                text.push_str(pattern);
            }
            let oracle = regex::Regex::new(&text).unwrap();
            let state =
                pool.install(|| RegexState::compile(None, pattern, flags).unwrap().unwrap());
            assert_eq!(
                state.matches_empty(),
                oracle.is_match(""),
                "{pattern} {flags}"
            );
            for t in texts {
                assert_eq!(
                    state.is_match(t),
                    oracle.is_match(t),
                    "{pattern} {flags} {t:?}"
                );
            }
            pool.install(|| {
                use rayon::prelude::*;
                (0..256).into_par_iter().for_each(|i| {
                    let t = texts[i % texts.len()];
                    assert_eq!(
                        state.is_match(t),
                        oracle.is_match(t),
                        "{pattern} {flags} {t:?}"
                    );
                    // a matcher found once searches as the state does, with the
                    // worker's own clone
                    let matcher = state.matcher();
                    assert!(matcher.own.is_some(), "{pattern} {flags}");
                    for t in texts {
                        assert_eq!(
                            matcher.is_match(t),
                            oracle.is_match(t),
                            "{pattern} {flags} {t:?}"
                        );
                    }
                });
            });
            // outside the pool a matcher searches with the locked cache
            let matcher = state.matcher();
            assert!(matcher.own.is_none());
            for t in texts {
                assert_eq!(
                    matcher.is_match(t),
                    oracle.is_match(t),
                    "{pattern} {flags} {t:?}"
                );
            }
        }
        assert!(RegexState::compile(None, "[", "").unwrap().is_none());
        assert!(RegexState::compile(None, "a", "!").unwrap().is_none());
    }

    /// A pattern that queries repeat compiles once. A later compilation shares the kept
    /// program, and a bound below the one it was built under compiles again.
    #[test]
    fn a_repeated_pattern_shares_its_kept_program() {
        let pattern = "^kept-[0-9]{2}x$";
        let first = RegexState::compile(None, pattern, "").unwrap().unwrap();
        let kept = kept_program(pattern, usize::MAX).expect("the program is kept");
        assert!(kept_program(pattern, kept.nfa - 1).is_none());
        let second = RegexState::compile(None, pattern, "").unwrap().unwrap();
        assert!(second.is_match("kept-12x") && !second.is_match("kept-123x"));
        assert!(first.is_match("kept-99x"));
        // clones of one compiled program share its group information
        assert!(std::ptr::eq(
            first.program.group_info(),
            second.program.group_info()
        ));
        let other = Regex::new(pattern).unwrap();
        assert!(!std::ptr::eq(
            first.program.group_info(),
            other.group_info()
        ));
    }

    /// A finished query parks its workers' clones of a kept program, and the next query
    /// that searches the program on those threads takes them back with their caches. A
    /// program that the table does not keep parks nothing.
    #[test]
    fn workers_take_back_the_clones_an_earlier_query_parked() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let pattern = "^parked-[0-9]+$";
        let parked = |pattern: &str| -> Vec<usize> {
            let programs = programs().lock();
            let mut threads: Vec<usize> = programs
                .by_text
                .get(pattern)
                .map(|kept| kept.parked.keys().copied().collect())
                .unwrap_or_default();
            threads.sort_unstable();
            threads
        };
        let first = pool.install(|| RegexState::compile(None, pattern, "").unwrap().unwrap());
        let mut makers: Vec<usize> = pool.broadcast(|_| {
            assert!(first.is_match("parked-1") && !first.is_match("parked-x"));
            THREAD.with(|t| *t)
        });
        makers.sort_unstable();
        assert!(parked(pattern).is_empty());
        drop(first);
        assert_eq!(parked(pattern), makers);
        let second = pool.install(|| RegexState::compile(None, pattern, "").unwrap().unwrap());
        let found: Vec<bool> =
            pool.broadcast(|_| second.is_match("parked-22") && !second.is_match("parked-"));
        assert!(found.into_iter().all(|ok| ok));
        assert!(
            parked(pattern).is_empty(),
            "the workers took their clones back"
        );
        // another compilation of the same text does not take clones of the kept program
        drop(second);
        assert_eq!(parked(pattern), makers);
        let other = Regex::new(pattern).unwrap();
        assert!(take_parked(pattern, &other, makers[0]).is_none());
        assert_eq!(parked(pattern), makers);
    }

    /// A query with an owner charges each parked clone it takes back as it charges a new
    /// clone, and a query without room for the cache leaves the clone parked.
    #[test]
    fn a_charged_query_charges_the_parked_clones_it_takes() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let pattern = "^charged-parked-[0-9]+$";
        let parked = || -> usize {
            programs()
                .lock()
                .by_text
                .get(pattern)
                .map_or(0, |kept| kept.parked.len())
        };
        let store = crate::store::Store::in_memory(Default::default());
        let query = |limit: u64| {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = limit;
            let ctx = Arc::new(ctx);
            ctx.attach_owner();
            ctx
        };
        let first = query(256 << 20);
        let state = pool.install(|| {
            RegexState::compile(Some(&first), pattern, "")
                .unwrap()
                .unwrap()
        });
        assert!(
            pool.broadcast(|_| state.is_match("charged-parked-1"))
                .into_iter()
                .all(|m| m)
        );
        assert!(
            state
                .workers
                .iter()
                .all(|w| w.get().unwrap().program.is_some())
        );
        let cache_bytes = state.cache_bytes;
        drop(state);
        assert_eq!(parked(), 2);
        // a query whose budget leaves no room for worker caches shares the locked one
        let tight = query(1 << 20);
        let state = pool.install(|| {
            RegexState::compile(Some(&tight), pattern, "")
                .unwrap()
                .unwrap()
        });
        assert!(
            pool.broadcast(|_| state.is_match("charged-parked-2"))
                .into_iter()
                .all(|m| m)
        );
        assert!(
            state
                .workers
                .iter()
                .all(|w| w.get().unwrap().program.is_none())
        );
        drop(state);
        assert_eq!(parked(), 2, "a refused worker leaves the clone parked");
        let second = query(256 << 20);
        let state = pool.install(|| {
            RegexState::compile(Some(&second), pattern, "")
                .unwrap()
                .unwrap()
        });
        let before = second.memory_remaining();
        assert!(
            pool.broadcast(|_| state.is_match("charged-parked-3"))
                .into_iter()
                .all(|m| m)
        );
        assert_eq!(parked(), 0, "both workers took their clones back");
        assert!(
            before - second.memory_remaining() >= 2 * cache_bytes,
            "each clone taken back is charged its cache bound"
        );
        drop(state);
        assert_eq!(parked(), 2);
    }

    /// A worker's clone serves only the thread that made it. A worker of another pool
    /// whose index is the same, and a thread outside Rayon, search with the locked cache.
    #[test]
    fn a_worker_clone_serves_only_the_thread_that_made_it() {
        let a = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let b = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let state = a.install(|| RegexState::compile(None, "^x[0-9]+$", "").unwrap().unwrap());
        let makers: Vec<usize> = a.broadcast(|_| {
            assert!(state.is_match("x12"));
            THREAD.with(|t| *t)
        });
        let slots = |state: &RegexState| -> Vec<Option<usize>> {
            state
                .workers
                .iter()
                .map(|w| w.get().map(|w| w.thread))
                .collect()
        };
        let mut made: Vec<usize> = slots(&state).into_iter().map(Option::unwrap).collect();
        made.sort_unstable();
        let mut expected = makers.clone();
        expected.sort_unstable();
        assert_eq!(made, expected);
        let others: Vec<bool> = b.broadcast(|_| {
            assert!(state.worker().is_none());
            state.is_match("x7") && !state.is_match("y7")
        });
        assert!(others.into_iter().all(|ok| ok));
        assert!(state.worker().is_none());
        assert!(state.is_match("x1"));
        let mut after: Vec<usize> = slots(&state).into_iter().map(Option::unwrap).collect();
        after.sort_unstable();
        assert_eq!(after, expected);
    }

    /// A cache makes no map until a lookup, clearing one that holds no pattern keeps its
    /// map, which every query does twice, and clearing one that holds a pattern gives the
    /// map and its charge back.
    #[test]
    fn clearing_an_empty_cache_keeps_its_map() {
        let store = crate::store::Store::in_memory(Default::default());
        let ctx = Arc::new(Ctx::new(store.snapshot()));
        ctx.attach_owner();
        let cache = RegexCache::default();
        cache.clear();
        assert!(cache.entries.get().is_none());
        let empty = cache.entries().load_full();
        cache.clear();
        assert!(Arc::ptr_eq(&empty, &cache.entries().load_full()));
        let before = ctx.live_bytes();
        assert!(cache.get(&ctx, "^kept-[a-z]+$", "").unwrap().is_some());
        assert!(ctx.live_bytes() > before);
        cache.clear();
        assert!(cache.entries().load().is_empty());
        assert!(!Arc::ptr_eq(&empty, &cache.entries().load_full()));
        assert_eq!(ctx.live_bytes(), before);
    }
}
