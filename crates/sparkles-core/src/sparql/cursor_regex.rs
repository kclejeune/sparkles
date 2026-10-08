//! Query-owned regex programs and explicit, bounded search caches. Cursor
//! expressions never populate the process's eager thread-local regex cache.

use super::ctx::{Ctx, RetainedCharge};
use crate::error::Result;
use parking_lot::Mutex;
use regex_automata::{
    Input,
    meta::{Cache, Regex},
    util::iter::Searcher,
};
use rustc_hash::FxHashMap;
use std::sync::Arc;

#[derive(Default)]
pub(super) struct RegexCache {
    entries: Mutex<FxHashMap<String, FxHashMap<String, Entry>>>,
}

struct Entry {
    state: Option<Arc<RegexState>>,
    _charge: Option<RetainedCharge>,
}

impl RegexCache {
    pub(super) fn clear(&self) {
        self.entries.lock().clear();
    }

    pub(super) fn get(
        &self,
        ctx: &Ctx,
        pattern: &str,
        flags: &str,
    ) -> Result<Option<Arc<RegexState>>> {
        ctx.check()?;
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.get(pattern).and_then(|e| e.get(flags)) {
            return Ok(entry.state.clone());
        }
        // Bound cardinality as well as bytes; clearing releases charges after
        // the last active search releases its program.
        if entries.values().map(FxHashMap::len).sum::<usize>() >= 64 {
            entries.clear();
        }
        let charge = ctx.retained_charge((pattern.len() + flags.len()) as u64 * 2 + 1024)?;
        let state = RegexState::compile(ctx, pattern, flags)?.map(Arc::new);
        entries.entry(pattern.into()).or_default().insert(
            flags.into(),
            Entry {
                state: state.clone(),
                _charge: charge,
            },
        );
        Ok(state)
    }
}

pub(super) struct RegexState {
    program: Regex,
    cache: Arc<SearchCache>,
    cache_bytes: u64,
    _charge: Option<RetainedCharge>,
}

struct SearchCache {
    inner: Mutex<Cache>,
    _charge: Option<RetainedCharge>,
}

/// A key-filter worker can hold its own search cache. If the optional fork does
/// not fit, it shares the admitted cache, preserving semantics and the budget.
pub(super) struct RegexHandle {
    state: Arc<RegexState>,
    cache: Arc<SearchCache>,
}

impl Clone for RegexHandle {
    fn clone(&self) -> Self {
        let cache = self
            .state
            ._charge
            .as_ref()
            .and_then(RetainedCharge::owner)
            .and_then(|ctx| {
                // Worker-local caches are optional. Preserve most of the query
                // allowance for input/output and reconstruction scratch rather
                // than greedily spending it immediately before a worker pull.
                (ctx.mem_limit > 1 << 20
                    && ctx
                        .memory_remaining()
                        .saturating_sub(self.state.cache_bytes)
                        > ctx.mem_limit / 4 * 3)
                    .then(|| self.state.fork(&ctx).ok())
                    .flatten()
            })
            .unwrap_or_else(|| self.cache.clone());
        Self {
            state: self.state.clone(),
            cache,
        }
    }
}

impl RegexHandle {
    pub(super) fn is_match(&self, text: &str) -> bool {
        self.state
            .program
            .search_with(
                &mut self.cache.inner.lock(),
                &Input::new(text).earliest(true),
            )
            .is_some()
    }
}

impl RegexState {
    fn compile(ctx: &Ctx, pattern: &str, flags: &str) -> Result<Option<Self>> {
        let base = (pattern.len() + flags.len()) as u64 * 256 + 4096;
        let available = ctx.memory_remaining();
        let maximum = ((available.saturating_sub(base) / 48).min(4 << 20)) as usize;
        if maximum < 1024 {
            return Err(ctx.memory_exceeded(ctx.mem_limit.saturating_add(1)));
        }
        // Compilation may hold AST/HIR, forward/reverse NFAs and construction
        // buffers simultaneously. Reserve a conservative envelope up front.
        let text_charge = ctx.charge(base)?;
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
        let mut nfa = maximum.min(pattern.len().saturating_mul(128).saturating_add(8192));
        let (program, hybrid, mut charge) = loop {
            ctx.check()?;
            let hybrid = (nfa * 2).min(2 << 20);
            let charge = ctx.retained_charge(nfa as u64 * 32 + hybrid as u64 * 4)?;
            let built = Regex::builder()
                .configure(
                    Regex::config()
                        .nfa_size_limit(Some(nfa))
                        .hybrid_cache_capacity(hybrid)
                        // These alternative engines retain additional state; the hybrid
                        // DFA and PikeVM suffice and have explicitly budgeted caches.
                        .dfa(false)
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
                Err(e) if e.size_limit().is_some() && maximum < 4 << 20 => {
                    return Err(ctx.memory_exceeded(ctx.mem_limit.saturating_add(1)));
                }
                Err(_) => return Ok(None),
            }
        };
        // PikeVM capture slots grow with both states and capture groups. Include
        // that product before creating the explicit cache, not just NFA bytes.
        let retained = (program.memory_usage() as u64).saturating_mul(
            (program.captures_len() as u64)
                .saturating_mul(8)
                .saturating_add(16),
        ) + hybrid as u64 * 2
            + program.captures_len() as u64 * 512
            + 4096;
        if let Some(charge) = &mut charge {
            charge.resize(program.memory_usage() as u64 + 4096)?;
        }
        let cache_charge = ctx.retained_charge(retained)?;
        let cache = program.create_cache();
        drop(text_charge);
        ctx.check()?;
        Ok(Some(Self {
            program,
            cache: Arc::new(SearchCache {
                inner: Mutex::new(cache),
                _charge: cache_charge,
            }),
            cache_bytes: retained,
            _charge: charge,
        }))
    }

    fn fork(&self, ctx: &Ctx) -> Result<Arc<SearchCache>> {
        let charge = ctx.retained_charge(self.cache_bytes)?;
        Ok(Arc::new(SearchCache {
            inner: Mutex::new(self.program.create_cache()),
            _charge: charge,
        }))
    }

    pub(super) fn handle(self: &Arc<Self>) -> RegexHandle {
        RegexHandle {
            state: self.clone(),
            cache: self.cache.clone(),
        }
    }

    pub(super) fn is_match(&self, ctx: &Ctx, text: &str) -> Result<bool> {
        ctx.check()?;
        let found = self
            .program
            .search_with(
                &mut self.cache.inner.lock(),
                &Input::new(text).earliest(true),
            )
            .is_some();
        ctx.check()?;
        Ok(found)
    }

    pub(super) fn replace(&self, ctx: &Ctx, text: &str, replacement: &str) -> Result<String> {
        let mut cache = self.cache.inner.lock();
        let mut captures = self.program.create_captures();
        let mut searcher = Searcher::new(Input::new(text));
        let charge = ctx.charge(1024)?;
        let mut reserved = 0u64;
        let mut output = String::new();
        let mut end = 0;
        while let Some(found) = searcher.advance(|input| {
            self.program
                .search_captures_with(&mut cache, input, &mut captures);
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
        if !ctx.cursor_decode(output.capacity()) {
            ctx.check()?;
        }
        Ok(output)
    }
}
