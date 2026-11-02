//! Query-owned regular expressions with explicitly bounded, charged search caches. Both
//! accounting modes compile `REGEX`, `REPLACE` and key-filter patterns here, once per
//! query, and nothing in query evaluation keeps a program past its query.
//!
//! A program has one search cache that any thread can use behind a lock, and a clone of
//! the program for each Rayon worker that searches it, with the worker's own cache. A
//! worker's clone searches without a lock, which keeps a parallel filter as fast as one
//! regular expression per thread. Each clone's cache is charged when it is made, and a
//! worker whose cache does not fit shares the locked one.

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

/// The patterns a query keeps at once. Reaching it starts the cache over, and the
/// programs go once the last search that holds one ends.
const MAX_PATTERNS: usize = 64;

type Entries = FxHashMap<Box<str>, FxHashMap<Box<str>, Arc<Entry>>>;

/// A query's compiled patterns by pattern and flags. A lookup reads the current map
/// without a lock, and an insert replaces the whole map, which holds at most
/// [`MAX_PATTERNS`] entries.
pub(super) struct RegexCache {
    entries: ArcSwap<Entries>,
    /// serializes compilations, so that each pattern compiles once
    insert: Mutex<()>,
}

impl Default for RegexCache {
    fn default() -> Self {
        Self {
            entries: ArcSwap::from_pointee(Entries::default()),
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
        self.entries.store(Arc::default());
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
        {
            let entries = self.entries.load();
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
        if let Some(entry) = self.entries.load().get(pattern).and_then(|e| e.get(flags)) {
            return Ok(entry.state.clone());
        }
        ctx.check()?;
        let _insert = self.insert.lock();
        let current = self.entries.load_full();
        if let Some(entry) = current.get(pattern).and_then(|e| e.get(flags)) {
            return Ok(entry.state.clone());
        }
        let charge = ctx.retained_charge((pattern.len() + flags.len()) as u64 * 2 + 1024)?;
        let state = RegexState::compile(Some(ctx), pattern, flags)?.map(Arc::new);
        let mut next = if current.values().map(FxHashMap::len).sum::<usize>() >= MAX_PATTERNS {
            Entries::default()
        } else {
            (*current).clone()
        };
        next.entry(pattern.into()).or_default().insert(
            flags.into(),
            Arc::new(Entry {
                state: state.clone(),
                _charge: charge,
            }),
        );
        self.entries.store(Arc::new(next));
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
    /// one clone of the program per Rayon worker of the pool that compiled it, and slot 0
    /// for other threads, each made on first use
    workers: Box<[OnceLock<Worker>]>,
    _charge: Option<RetainedCharge>,
    _shared_charge: Option<RetainedCharge>,
}

/// A worker's clone of a program, whose own pool keeps the worker's search cache, or
/// `None` when the cache did not fit and the worker shares the locked one.
struct Worker {
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
        let (program, hybrid, mut charge) = loop {
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
                Ok(program) => break (program, hybrid, charge),
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
        let workers = (0..=rayon::current_num_threads())
            .map(|_| OnceLock::new())
            .collect();
        Ok(Some(Self {
            program,
            matches_empty,
            shared: Mutex::new(cache),
            cache_bytes,
            workers,
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

    /// This thread's clone of the program, made on its first search, or `None` when the
    /// thread shares the locked cache.
    #[inline]
    fn worker(&self) -> Option<&Regex> {
        let slot = rayon::current_thread_index().map_or(0, |i| i + 1);
        self.workers
            .get(slot)?
            .get_or_init(|| self.new_worker())
            .program
            .as_ref()
    }

    #[cold]
    fn new_worker(&self) -> Worker {
        // A program compiled without an owner is uncharged, and so are its clones.
        let Some(charge) = &self._charge else {
            return Worker {
                program: Some(self.program.clone()),
                _charge: None,
            };
        };
        let refused = Worker {
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
                program: Some(self.program.clone()),
                _charge: charge,
            },
            _ => refused,
        }
    }

    #[inline]
    pub(super) fn is_match(&self, text: &str) -> bool {
        let input = Input::new(text).earliest(true);
        match self.worker() {
            Some(program) => program.search(&input).is_some(),
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
            // Each replacement byte can at most reference one full input.
            // Reserve before interpolation and before growing the output.
            let need = output
                .len()
                .saturating_add(found.start() - end)
                .saturating_add(
                    replacement
                        .len()
                        .saturating_mul(text.len().saturating_add(1)),
                );
            let bytes = (need as u64).saturating_mul(2);
            charge.add(bytes.saturating_sub(reserved))?;
            reserved = bytes;
            output.reserve_exact(need.saturating_sub(output.len()));
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
                });
            });
        }
        assert!(RegexState::compile(None, "[", "").unwrap().is_none());
        assert!(RegexState::compile(None, "a", "!").unwrap().is_none());
    }
}
