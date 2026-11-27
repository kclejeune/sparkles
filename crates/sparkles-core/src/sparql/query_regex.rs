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

#[derive(Clone, Copy)]
struct FlagState {
    /// Whether `.` matches `\r` and `\n`.
    dot_all: bool,
    /// Whether Rust's inline `(?x)` mode is active (whitespace and `#...\n` comments are
    /// ignored both outside and inside character classes).
    rust_x: bool,
    /// Whether XPath's external `x` flag is active (XML whitespace is removed outside
    /// character classes, while spaces inside classes and `#` stay literal).
    xpath_x: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClassStart {
    None,
    /// Immediately after `[` (so `^` negates and `]` is a literal member).
    AfterOpen,
    /// Immediately after `[^` (so `]` is a literal member).
    AfterNegation,
}

#[inline]
fn is_xml_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// Matches `regex_syntax::ast::parse::is_capture_char`.
#[inline]
fn is_capture_char(c: char, first: bool) -> bool {
    if first {
        c == '_' || c.is_alphabetic()
    } else {
        c == '_' || c == '.' || c == '[' || c == ']' || c.is_alphanumeric()
    }
}

/// Advance `probe` past whitespace/comments (under inline Rust `(?x)`) or XML whitespace
/// (under external XPath `x` when `rust_x` is off), returning the number of chars skipped.
fn skip_group_trivia(
    probe: &mut std::iter::Peekable<std::str::Chars<'_>>,
    state: FlagState,
) -> usize {
    let mut skipped = 0usize;
    if state.rust_x {
        loop {
            match probe.peek().copied() {
                Some(ch) if ch.is_whitespace() => {
                    probe.next();
                    skipped += 1;
                }
                Some('#') => {
                    probe.next();
                    skipped += 1;
                    for cc in probe.by_ref() {
                        skipped += 1;
                        if cc == '\n' {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
    } else if state.xpath_x {
        while probe.peek().is_some_and(|&ch| is_xml_ws(ch)) {
            probe.next();
            skipped += 1;
        }
    }
    skipped
}

/// Copy `count` characters from `chars` into `out`, stripping XML whitespace when
/// `strip_ws` is active.
fn emit_consumed(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    count: usize,
    strip_ws: bool,
    out: &mut String,
) {
    for _ in 0..count {
        let ch = chars.next().expect("probed character exists");
        if !(strip_ws && is_xml_ws(ch)) {
            out.push(ch);
        }
    }
}

/// Handle an opening `(` outside a character class, mirroring `regex_syntax`'s
/// `parse_group`, `parse_capture_name`, and `parse_flags` rules while also honoring
/// external XPath `x` whitespace stripping.
fn handle_open_paren(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    state: &mut FlagState,
    stack: &mut Vec<FlagState>,
    out: &mut String,
) {
    out.push('(');
    let strip_ws = state.xpath_x && !state.rust_x;
    let mut probe = chars.clone();
    let mut consumed = skip_group_trivia(&mut probe, *state);
    if probe.peek() != Some(&'?') {
        stack.push(*state);
        return;
    }
    probe.next();
    consumed += 1;
    if strip_ws {
        while probe.peek().is_some_and(|&ch| is_xml_ws(ch)) {
            probe.next();
            consumed += 1;
        }
    }

    // Named capture group: `(?P<name>...)` or `(?<name>...)`. Copy the header and name
    // verbatim (stripping XPath `x` whitespace if active) so `.`, `[`, and `]` inside the
    // name are never mistaken for regex syntax.
    let mut named_probe = probe.clone();
    let mut named_extra = 0usize;
    let is_named = if named_probe.peek() == Some(&'<') {
        named_probe.next();
        named_extra += 1;
        true
    } else if named_probe.peek() == Some(&'P') {
        named_probe.next();
        named_extra += 1;
        if strip_ws {
            while named_probe.peek().is_some_and(|&ch| is_xml_ws(ch)) {
                named_probe.next();
                named_extra += 1;
            }
        }
        if named_probe.peek() == Some(&'<') {
            named_probe.next();
            named_extra += 1;
            true
        } else {
            false
        }
    } else {
        false
    };
    if is_named {
        let mut first = true;
        let mut valid_name = false;
        for nc in named_probe {
            named_extra += 1;
            if strip_ws && is_xml_ws(nc) {
                continue;
            }
            if nc == '>' {
                valid_name = !first;
                break;
            }
            if !is_capture_char(nc, first) {
                break;
            }
            first = false;
        }
        if valid_name {
            emit_consumed(chars, consumed + named_extra, strip_ws, out);
            stack.push(*state);
            return;
        }
    }

    // Inline flag directive `(?flags)` or scoped flag group `(?flags:...)`.
    let mut next_state = *state;
    let mut neg = false;
    let mut last_was_neg = false;
    let mut saw_flag = false;
    let mut seen_flags = 0u8;
    let mut valid = true;
    let mut terminator = None;
    for fc in probe {
        consumed += 1;
        if strip_ws && is_xml_ws(fc) {
            continue;
        }
        match fc {
            'i' | 'm' | 'R' | 'U' | 'u' | 's' | 'x' => {
                let bit = match fc {
                    'i' => 1 << 0,
                    'm' => 1 << 1,
                    'R' => 1 << 2,
                    'U' => 1 << 3,
                    'u' => 1 << 4,
                    's' => 1 << 5,
                    'x' => 1 << 6,
                    _ => unreachable!(),
                };
                if seen_flags & bit != 0 {
                    valid = false;
                    break;
                }
                seen_flags |= bit;
                saw_flag = true;
                last_was_neg = false;
                if fc == 's' {
                    next_state.dot_all = !neg;
                } else if fc == 'x' {
                    if neg {
                        next_state.rust_x = false;
                        next_state.xpath_x = false;
                    } else {
                        next_state.rust_x = true;
                    }
                }
            }
            '-' if !neg => {
                neg = true;
                last_was_neg = true;
            }
            ':' if !last_was_neg => {
                terminator = Some(':');
                break;
            }
            ')' if saw_flag && !last_was_neg => {
                terminator = Some(')');
                break;
            }
            _ => {
                valid = false;
                break;
            }
        }
    }
    if valid {
        match terminator {
            Some(')') => {
                emit_consumed(chars, consumed, strip_ws, out);
                *state = next_state;
                return;
            }
            Some(':') => {
                emit_consumed(chars, consumed, strip_ws, out);
                stack.push(*state);
                *state = next_state;
                return;
            }
            _ => {}
        }
    }
    stack.push(*state);
}

/// Translate `pattern` into Rust `regex` syntax under the effective flag state:
/// - Under XPath `x` (`xpath_x && !rust_x`), strip XML whitespace (`#x9`, `#xA`, `#xD`,
///   `#x20`) outside character classes before interpreting escapes while preserving
///   whitespace inside classes and keeping `#` literal.
/// - Under inline Rust `(?x)` (`rust_x`), copy `#...\n` comments verbatim without
///   altering class/group state, skip whitespace and comments after `(` when probing
///   group headers, and keep `ClassStart` across whitespace/comments inside classes.
/// - Preserve named capture headers (`(?P<name>...)` and `(?<name>...)`) verbatim so
///   `.` and brackets in capture names are not treated as regex body tokens.
/// - Track inline Rust flag directives `(?flags)` and scoped flag groups `(?flags:...)`
///   across parenthesized groups so effective `s` and `x` are respected at each `.`.
/// - Rewrite unescaped `.` outside character classes to `[^\n\r]` whenever effective `s`
///   is disabled, matching XPath F&O 3.1 §5.6.1.
fn rewrite_xpath_pattern(pattern: &str, initial: FlagState, out: &mut String) {
    let mut chars = pattern.chars().peekable();
    let mut state = initial;
    let mut stack: Vec<FlagState> = Vec::new();
    let mut class_depth: usize = 0;
    let mut class_start = ClassStart::None;

    while let Some(c) = chars.next() {
        if state.rust_x && c == '#' {
            out.push('#');
            for cc in chars.by_ref() {
                out.push(cc);
                if cc == '\n' {
                    break;
                }
            }
            continue;
        }
        if state.rust_x && c.is_whitespace() {
            out.push(c);
            continue;
        }
        if class_depth == 0 && state.xpath_x && !state.rust_x && is_xml_ws(c) {
            continue;
        }
        match c {
            '\\' => {
                out.push('\\');
                if class_depth == 0 && state.xpath_x && !state.rust_x {
                    while chars.peek().is_some_and(|&ch| is_xml_ws(ch)) {
                        chars.next();
                    }
                }
                if let Some(next) = chars.next() {
                    out.push(next);
                }
                class_start = ClassStart::None;
            }
            '[' => {
                class_depth += 1;
                out.push('[');
                class_start = ClassStart::AfterOpen;
            }
            '^' if class_start == ClassStart::AfterOpen => {
                out.push('^');
                class_start = ClassStart::AfterNegation;
            }
            ']' if class_depth > 0 => {
                out.push(']');
                if class_start != ClassStart::None {
                    class_start = ClassStart::None;
                } else {
                    class_depth -= 1;
                }
            }
            '(' if class_depth == 0 => {
                handle_open_paren(&mut chars, &mut state, &mut stack, out);
            }
            ')' if class_depth == 0 => {
                out.push(')');
                if let Some(prev) = stack.pop() {
                    state = prev;
                }
            }
            '.' if class_depth == 0 => {
                if state.dot_all {
                    out.push('.');
                } else {
                    out.push_str("[^\\n\\r]");
                }
            }
            _ => {
                out.push(c);
                class_start = ClassStart::None;
            }
        }
    }
}

/// Translate an XPath pattern and SPARQL `REGEX` / `REPLACE` flags into a Rust `regex`
/// pattern string. Returns `None` if `flags` contains an unsupported flag.
pub(super) fn translate_xpath_pattern(pattern: &str, flags: &str) -> Option<String> {
    let mut insensitive = false;
    let mut dot_all = false;
    let mut multi_line = false;
    let mut ignore_ws = false;
    let mut literal = false;
    for f in flags.chars() {
        match f {
            'i' => insensitive = true,
            's' => dot_all = true,
            'm' => multi_line = true,
            'x' => ignore_ws = true,
            'q' => literal = true,
            _ => return None,
        }
    }
    let mut text = String::with_capacity(pattern.len() + 16);
    if insensitive {
        text.push_str("(?i)");
    }
    if literal {
        text.push_str(&regex::escape(pattern));
        return Some(text);
    }
    if dot_all {
        text.push_str("(?s)");
    }
    if multi_line {
        text.push_str("(?m)");
    }
    if !ignore_ws && !pattern.contains('.') {
        text.push_str(pattern);
    } else {
        rewrite_xpath_pattern(
            pattern,
            FlagState {
                dot_all,
                rust_x: false,
                xpath_x: ignore_ws,
            },
            &mut text,
        );
    }
    Some(text)
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
        let Some(text) = translate_xpath_pattern(pattern, flags) else {
            return Ok(None);
        };
        let retained = |bytes: u64| match ctx {
            Some(ctx) => ctx.retained_charge(bytes),
            None => Ok(None),
        };
        let check = || ctx.map_or(Ok(()), Ctx::check);
        let mut nfa = maximum.min(
            text.len()
                .max(pattern.len())
                .saturating_mul(128)
                .saturating_add(8192),
        );
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
        self.replace_inner(ctx, text, replacement, false)
    }

    /// `text` with every match of the program replaced by the literal string
    /// `replacement` (for XPath `REPLACE` under the `q` flag), charging the actual output
    /// bytes without reserving capture expansions for `$`.
    pub(super) fn replace_literal(
        &self,
        ctx: &Ctx,
        text: &str,
        replacement: &str,
    ) -> Result<String> {
        self.replace_inner(ctx, text, replacement, true)
    }

    fn replace_inner(
        &self,
        ctx: &Ctx,
        text: &str,
        replacement: &str,
        literal: bool,
    ) -> Result<String> {
        let worker = self.worker();
        let mut shared = worker.is_none().then(|| self.shared.lock());
        let mut captures = (!literal).then(|| self.program.create_captures());
        let mut searcher = Searcher::new(Input::new(text));
        let charge = ctx.charge(1024)?;
        // `$` starts every group reference, and `$$` counts twice, which only overstates.
        // Literal (`q`) replacements write `replacement` verbatim with no capture expansion.
        let references = if literal {
            0
        } else {
            replacement.bytes().filter(|&b| b == b'$').count()
        };
        let mut reserved = 0u64;
        let mut output = String::new();
        let mut end = 0;
        while let Some(found) = searcher.advance(|input| {
            let found = if let Some(captures) = captures.as_mut() {
                match (worker, shared.as_deref_mut()) {
                    (Some(program), _) => program.search_captures(input, captures),
                    (None, Some(cache)) => {
                        self.program.search_captures_with(cache, input, captures)
                    }
                    (None, None) => {
                        unreachable!("a search without a worker holds the shared cache")
                    }
                }
                captures.get_match()
            } else {
                match (worker, shared.as_deref_mut()) {
                    (Some(program), _) => program.search(input),
                    (None, Some(cache)) => self.program.search_with(cache, input),
                    (None, None) => {
                        unreachable!("a search without a worker holds the shared cache")
                    }
                }
            };
            Ok(found)
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
            if let Some(captures) = captures.as_ref() {
                captures.interpolate_string_into(text, replacement, &mut output);
            } else {
                output.push_str(replacement);
            }
            end = found.end();
        }
        let need = output.len().saturating_add(text.len() - end);
        charge.add((need as u64 * 2).saturating_sub(reserved))?;
        output.reserve_exact(need - output.len());
        output.push_str(&text[end..]);
        // The expression's returned value remains live after this helper.
        ctx.reserve_decode_checked(output.capacity())?;
        ctx.check()?;
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

    #[test]
    fn xpath_dot_excludes_carriage_return_without_s_flag() {
        let re = RegexState::compile(None, "^a.b$", "").unwrap().unwrap();
        assert!(re.is_match("axb"));
        assert!(re.is_match("a.b"));
        assert!(!re.is_match("a\nb"));
        assert!(!re.is_match("a\rb"));

        let dot_all = RegexState::compile(None, "^a.b$", "s").unwrap().unwrap();
        assert!(dot_all.is_match("a\nb"));
        assert!(dot_all.is_match("a\rb"));

        // Escaped dot, escaped backslash before dot, and character classes with `.` or `]`.
        let esc_dot = RegexState::compile(None, "^a\\.b$", "").unwrap().unwrap();
        assert!(esc_dot.is_match("a.b"));
        assert!(!esc_dot.is_match("axb"));
        assert!(!esc_dot.is_match("a\rb"));

        let bs_dot = RegexState::compile(None, "^a\\\\.b$", "").unwrap().unwrap();
        assert!(bs_dot.is_match("a\\xb"));
        assert!(!bs_dot.is_match("a\\\rb"));

        let cls_dot = RegexState::compile(None, "^a[.]b$", "").unwrap().unwrap();
        assert!(cls_dot.is_match("a.b"));
        assert!(!cls_dot.is_match("axb"));

        let neg_cls = RegexState::compile(None, "^a[^.]b$", "").unwrap().unwrap();
        assert!(neg_cls.is_match("a\rb"));
        assert!(neg_cls.is_match("a\nb"));
        assert!(!neg_cls.is_match("a.b"));

        let leading_rbracket = RegexState::compile(None, "^a[].]b$", "").unwrap().unwrap();
        assert!(leading_rbracket.is_match("a]b"));
        assert!(leading_rbracket.is_match("a.b"));
        assert!(!leading_rbracket.is_match("axb"));

        let neg_leading_rbracket = RegexState::compile(None, "^a[^].]b$", "").unwrap().unwrap();
        assert!(neg_leading_rbracket.is_match("axb"));
        assert!(neg_leading_rbracket.is_match("a\rb"));
        assert!(!neg_leading_rbracket.is_match("a]b"));
        assert!(!neg_leading_rbracket.is_match("a.b"));

        let posix_then_dot = RegexState::compile(None, "^[[:alpha:].].$", "")
            .unwrap()
            .unwrap();
        assert!(posix_then_dot.is_match(".x"));
        assert!(posix_then_dot.is_match("ax"));
        assert!(!posix_then_dot.is_match(".\r"));
        assert!(!posix_then_dot.is_match("a\r"));

        let escaped_rbracket = RegexState::compile(None, "^[\\]].$", "").unwrap().unwrap();
        assert!(escaped_rbracket.is_match("]x"));
        assert!(!escaped_rbracket.is_match("]\r"));

        // `q` flag ignores `s`, `m`, `x` while preserving `i`.
        let qx = RegexState::compile(None, "a . b", "qx").unwrap().unwrap();
        assert!(qx.is_match("a . b"));
        assert!(!qx.is_match("axb"));
        let qi = RegexState::compile(None, "A.B", "qi").unwrap().unwrap();
        assert!(qi.is_match("a.b"));
        assert!(!qi.is_match("axb"));
    }

    #[test]
    fn xpath_x_flag_preserves_class_spaces_and_handles_inline_rust_flags() {
        // XPath F&O 3.1 `x` flag strips XML whitespace outside character classes while
        // preserving literal whitespace inside character classes and literal `#`.
        let cls_space_and_dot = RegexState::compile(None, "^[ ]a.b$", "x").unwrap().unwrap();
        assert!(cls_space_and_dot.is_match(" axb"));
        assert!(!cls_space_and_dot.is_match(" a\rb"));
        assert!(!cls_space_and_dot.is_match(" a\nb"));
        assert!(!cls_space_and_dot.is_match("axb"));

        let spaced_outside = RegexState::compile(None, "^ [ ] a . b $", "x")
            .unwrap()
            .unwrap();
        assert!(spaced_outside.is_match(" axb"));
        assert!(!spaced_outside.is_match(" a\rb"));

        let quant_space = RegexState::compile(None, "^a{ 2 }$", "x").unwrap().unwrap();
        assert!(quant_space.is_match("aa"));
        assert!(!quant_space.is_match("a"));

        let literal_hash = RegexState::compile(None, "^a # b$", "x").unwrap().unwrap();
        assert!(literal_hash.is_match("a#b"));
        assert!(!literal_hash.is_match("a"));

        // Under XPath `x`, `[ ]` is a space class closed by the first `]`, never matching "ax".
        let bracket_dot = RegexState::compile(None, "^[ ].].$", "x").unwrap().unwrap();
        assert!(!bracket_dot.is_match("ax"));
        assert!(bracket_dot.is_match(" x]y"));
        assert!(!bracket_dot.is_match(" \r]y"));
        assert!(!bracket_dot.is_match(" x]\r"));

        // Inline Rust `(?x)` honors `#...\n` comments and ignores spaces after `[`/`[^`.
        let rust_x_comment_open = RegexState::compile(None, "(?x)# [note\n^a.b$", "")
            .unwrap()
            .unwrap();
        assert!(rust_x_comment_open.is_match("axb"));
        assert!(!rust_x_comment_open.is_match("a\rb"));

        let rust_x_comment_in_class = RegexState::compile(None, "^(?x)[a # comment ]\n .]$", "")
            .unwrap()
            .unwrap();
        assert!(rust_x_comment_in_class.is_match("a"));
        assert!(rust_x_comment_in_class.is_match("."));
        assert!(!rust_x_comment_in_class.is_match("x"));
        assert!(!rust_x_comment_in_class.is_match("\r"));

        let rust_x_space_before_rbracket = RegexState::compile(None, "^(?x)[ ].].$", "")
            .unwrap()
            .unwrap();
        assert!(rust_x_space_before_rbracket.is_match("]x"));
        assert!(rust_x_space_before_rbracket.is_match(".x"));
        assert!(!rust_x_space_before_rbracket.is_match("ax"));
        assert!(!rust_x_space_before_rbracket.is_match("]\r"));

        let rust_x_neg_space_before_rbracket = RegexState::compile(None, "^(?x)[^ ].].$", "")
            .unwrap()
            .unwrap();
        assert!(rust_x_neg_space_before_rbracket.is_match("ax"));
        assert!(!rust_x_neg_space_before_rbracket.is_match("]x"));
        assert!(!rust_x_neg_space_before_rbracket.is_match(".x"));
        assert!(!rust_x_neg_space_before_rbracket.is_match("a\r"));

        // Scoped and inline `s` / `-s` flags control effective `.` semantics.
        let scoped_enable = RegexState::compile(None, "^(?s:.)$", "").unwrap().unwrap();
        assert!(scoped_enable.is_match("\r"));
        assert!(scoped_enable.is_match("\n"));
        assert!(scoped_enable.is_match("x"));

        let scoped_disable = RegexState::compile(None, "^(?-s:.)$", "s")
            .unwrap()
            .unwrap();
        assert!(scoped_disable.is_match("x"));
        assert!(!scoped_disable.is_match("\r"));
        assert!(!scoped_disable.is_match("\n"));

        let scoped_then_outer = RegexState::compile(None, "^(?s:.).$", "").unwrap().unwrap();
        assert!(scoped_then_outer.is_match("\rx"));
        assert!(!scoped_then_outer.is_match("\r\r"));

        let inline_toggle = RegexState::compile(None, "^(?s).(?-s).$", "")
            .unwrap()
            .unwrap();
        assert!(inline_toggle.is_match("\rx"));
        assert!(!inline_toggle.is_match("\r\r"));

        let group_scoped_toggle = RegexState::compile(None, "^(a(?s).)b.$", "")
            .unwrap()
            .unwrap();
        assert!(group_scoped_toggle.is_match("a\rbx"));
        assert!(!group_scoped_toggle.is_match("a\rb\r"));

        // XPath `x` strips XML whitespace outside character classes BEFORE pairing `\`
        // with the next character (W3C F&O 3.1 example: `hello\ sworld` -> `hello\sworld`),
        // while keeping `\ ` inside classes and inline Rust `(?x)` `\ ` literal.
        let xpath_esc_ws = RegexState::compile(None, "^hello\\ sworld$", "x")
            .unwrap()
            .unwrap();
        assert!(xpath_esc_ws.is_match("hello world"));
        assert!(!xpath_esc_ws.is_match("hello sworld"));

        let xpath_esc_dot = RegexState::compile(None, "^a\\ \t\n.b$", "x")
            .unwrap()
            .unwrap();
        assert!(xpath_esc_dot.is_match("a.b"));
        assert!(!xpath_esc_dot.is_match("axb"));

        let xpath_esc_open_bracket = RegexState::compile(None, "^a\\ [ ]b$", "x")
            .unwrap()
            .unwrap();
        assert!(xpath_esc_open_bracket.is_match("a[]b"));
        assert!(!xpath_esc_open_bracket.is_match("a b"));

        let xpath_class_esc_space = RegexState::compile(None, "^[\\ ]a.b$", "x")
            .unwrap()
            .unwrap();
        assert!(xpath_class_esc_space.is_match(" axb"));
        assert!(!xpath_class_esc_space.is_match(" a\rb"));
        assert!(!xpath_class_esc_space.is_match("axb"));

        // Trailing backslash followed only by stripped XML whitespace is an invalid escape.
        assert!(RegexState::compile(None, "a\\ ", "x").unwrap().is_none());

        // Inline Rust `(?x)` keeps `\ ` as an escaped literal space.
        let rust_x_esc_space = RegexState::compile(None, "(?x)^hello\\ sworld$", "")
            .unwrap()
            .unwrap();
        assert!(rust_x_esc_space.is_match("hello sworld"));
        assert!(!rust_x_esc_space.is_match("hello world"));

        // Inline Rust `(?x)` skips whitespace and `#...\n` comments directly after `(`
        // before both scoped and standalone flag directives.
        for pat in [
            "(?x)^( ?s:.)$",
            "(?x)^(# enable s\n ?s:.)$",
            "(?x)^( ?s).$",
            "(?x)^(# enable s\n ?s).$",
        ] {
            let re = RegexState::compile(None, pat, "").unwrap().unwrap();
            assert!(re.is_match("\n"), "pattern {pat} should match LF");
            assert!(re.is_match("\r"), "pattern {pat} should match CR");
            assert!(re.is_match("x"), "pattern {pat} should match x");
        }
        for pat in [
            "(?x)^( ?-s:.)$",
            "(?x)^(# disable s\n ?-s:.)$",
            "(?x)^( ?-s).$",
            "(?x)^(# disable s\n ?-s).$",
        ] {
            let re = RegexState::compile(None, pat, "s").unwrap().unwrap();
            assert!(re.is_match("x"), "pattern {pat} should match x");
            assert!(!re.is_match("\r"), "pattern {pat} must reject CR");
            assert!(!re.is_match("\n"), "pattern {pat} must reject LF");
        }

        // Named capture headers (`(?P<...>)` and `(?<...>`) may contain `.`, `[`, and `]`
        // without corrupting the name or putting the scanner into class mode.
        for pat in [
            "^(?-s:(?P<a.b>.))$",
            "^(?-s:(?<a.b>.))$",
            "^(?-s:(?P<a[b].c>.))$",
            "^(?-s:(?<a[b>.))$",
            "(?x)^(?-s:( # named capture\n ?P<a[b].c> . ))$",
        ] {
            let re = RegexState::compile(None, pat, "s").unwrap().unwrap();
            assert!(re.is_match("x"), "pattern {pat} should match x");
            assert!(!re.is_match("\r"), "pattern {pat} must reject CR");
            assert!(!re.is_match("\n"), "pattern {pat} must reject LF");
        }
    }

    /// Replacing a large match with a dollar-heavy literal (`q`) replacement charges only
    /// the actual output size rather than `count('$') * match_len * 2`.
    #[test]
    fn literal_q_replacement_does_not_inflate_budget_for_dollars() {
        let store = crate::store::Store::in_memory(Default::default());
        let make_ctx = |limit: u64| {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = limit;
            let ctx = Arc::new(ctx);
            ctx.attach_owner();
            ctx
        };

        let hay = "a".repeat(4096);
        let dollars = "$".repeat(4096);
        // 8 MiB easily covers the compiled literal pattern (~1 MiB) and 4 KiB output,
        // whereas doubling `$` and reserving `references * found.len() * 2` demands > 64 MiB.
        let budget = 8 << 20;

        let ctx_lit = make_ctx(budget);
        let re_lit = RegexState::compile(Some(&ctx_lit), &hay, "q")
            .unwrap()
            .unwrap();
        let out = re_lit.replace_literal(&ctx_lit, &hay, &dollars).unwrap();
        assert_eq!(out, dollars);

        // Confirm the old doubled-`$` capture-interpolation path would exceed this budget.
        let ctx_interp = make_ctx(budget);
        let re_interp = RegexState::compile(Some(&ctx_interp), &hay, "q")
            .unwrap()
            .unwrap();
        let doubled = dollars.replace('$', "$$");
        assert!(
            re_interp.replace(&ctx_interp, &hay, &doubled).is_err(),
            "interpolating doubled dollars should exceed the 8 MiB budget"
        );
    }
}
