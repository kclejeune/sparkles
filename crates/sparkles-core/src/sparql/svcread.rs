//! The memory a streamed SERVICE response holds while sparesults parses it (item 829).
//!
//! sparesults reads a JSON response as it arrives only when the `vars` of its `head`
//! come before the `bindings` of its `results`. Otherwise it keeps every binding until
//! it has read the head, and only then yields the first solution. Jena and Fuseki write
//! the head first. The start of a JSON response is therefore read here first, up to its
//! top-level `results` or `boolean` member, to tell the two orders apart before
//! sparesults sees a byte. A strict cursor refuses a response whose head comes late, and
//! a permissive one reads it under a charge for the whole buffered answer. A decoder
//! that streams either order is roadmap item 1125.
//!
//! The charge comes from byte counts alone, reserved in [`Metered::read`] before the
//! parser has the bytes, so it is in place before the parser builds anything from them.
//! What the parser holds after the first solution is bounded by the most bytes read
//! between two solutions. The JSON parser's buffer grows only while a token is
//! incomplete, by doubling, and the XML parser's event buffer holds one event, so both
//! stay within a small multiple of that window and a fixed read-ahead.
use crate::sparql::ctx::{Ctx, OwnedCharge, RetainedCharge};
use crate::sparql::query_memory::QueryMemoryLease;
use json_event_parser::{JsonEvent, LowLevelJsonParser};
use oxrdf::{Term, Variable};
use std::io::{self, Read};
use std::mem::size_of;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Bytes charged for each byte of the most read between two solutions. The parser's
/// buffer holds that window and what is left of the previous one, at up to twice its
/// length, and a token that is unescaped and the terms of the solution copy it again.
const WINDOW_FACTOR: u64 = 8;

/// The parsers' fixed read-ahead: the JSON parser's buffer grows by 4 KiB, and the XML
/// parser reads through an 8 KiB buffer.
pub(super) const SLACK: u64 = 4 * 4096 + 8192;

/// Bytes read per bounded body read while probing a JSON head or validating a late-head
/// JSON response.
const READ_STEP: usize = 4096;

/// Credit held for the JSON staging buffer in either cursor or eager execution.
pub(super) enum StagingCharge {
    Cursor(OwnedCharge),
    EagerRetained(RetainedCharge),
    EagerLease { lease: QueryMemoryLease, limit: u64 },
}

impl StagingCharge {
    pub(super) fn from_cursor(charge: OwnedCharge) -> Self {
        Self::Cursor(charge)
    }

    pub(super) fn into_cursor(self) -> OwnedCharge {
        match self {
            Self::Cursor(charge) => charge,
            Self::EagerRetained(_) | Self::EagerLease { .. } => {
                unreachable!("cursor staging charge expected")
            }
        }
    }

    fn is_cursor(&self) -> bool {
        matches!(self, Self::Cursor(_))
    }

    fn bytes(&self) -> u64 {
        match self {
            Self::Cursor(c) => c.bytes(),
            Self::EagerRetained(c) => c.bytes(),
            Self::EagerLease { lease, .. } => lease.bytes,
        }
    }

    fn resize(&mut self, bytes: u64) -> crate::error::Result<()> {
        match self {
            Self::Cursor(c) => c.resize(bytes),
            Self::EagerRetained(c) => c.resize(bytes),
            Self::EagerLease { lease, limit } => lease.resize(*limit, bytes),
        }
    }
}

/// Start a zero-byte staging charge for an eager SERVICE response against `ctx`.
pub(super) fn eager_charge(ctx: &Ctx) -> crate::error::Result<StagingCharge> {
    if let Some(rc) = ctx.retained_charge(0)? {
        Ok(StagingCharge::EagerRetained(rc))
    } else {
        Ok(StagingCharge::EagerLease {
            lease: ctx.output_lease(0)?,
            limit: ctx.mem_limit,
        })
    }
}

/// A replay cursor that holds its [`StagingCharge`] for both its raw buffer and any
/// `parser_reserve` (head structures or late-head buffered bindings held by sparesults).
/// Once the raw buffer drains to EOF on a non-empty read, the buffer is freed and the
/// charge shrinks to `parser_reserve`, which stays live until the cursor drops.
pub(super) struct ChargedCursor {
    inner: io::Cursor<Vec<u8>>,
    charge: StagingCharge,
    parser_reserve: u64,
}

impl ChargedCursor {
    #[cfg(test)]
    pub(super) fn new(bytes: Vec<u8>, charge: StagingCharge) -> io::Result<Self> {
        Self::with_parser_reserve(bytes, charge, 0)
    }

    pub(super) fn with_parser_reserve(
        bytes: Vec<u8>,
        mut charge: StagingCharge,
        parser_reserve: u64,
    ) -> io::Result<Self> {
        let target = (bytes.capacity() as u64).saturating_add(parser_reserve);
        if target != charge.bytes() {
            charge.resize(target).map_err(io::Error::other)?;
        }
        Ok(Self {
            inner: io::Cursor::new(bytes),
            charge,
            parser_reserve,
        })
    }
}

impl Read for ChargedCursor {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if !buf.is_empty() && n == 0 && self.inner.get_ref().capacity() > 0 {
            let v = self.inner.get_mut();
            v.clear();
            v.shrink_to_fit();
            self.inner.set_position(0);
            let _ = self.charge.resize(self.parser_reserve);
        }
        Ok(n)
    }
}

fn vec_growth_peak(cur_cap: usize, next_len: usize) -> u64 {
    if cur_cap >= next_len {
        cur_cap as u64
    } else {
        let next_cap = next_len.checked_next_power_of_two().unwrap_or(next_len);
        (cur_cap as u64).saturating_add(next_cap as u64)
    }
}

fn skip_json_ws(mut slice: &[u8]) -> &[u8] {
    while let Some((&b, rest)) = slice.split_first() {
        if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
            slice = rest;
        } else {
            break;
        }
    }
    slice
}

fn parse_hex4(bytes: &[u8]) -> Option<u32> {
    let mut val = 0u32;
    for &b in bytes {
        let digit = match b {
            b'0'..=b'9' => (b - b'0') as u32,
            b'a'..=b'f' => (b - b'a' + 10) as u32,
            b'A'..=b'F' => (b - b'A' + 10) as u32,
            _ => return None,
        };
        val = (val << 4) | digit;
    }
    Some(val)
}

/// Size in bytes of one entry in `LowLevelJsonParser::state_stack` (`JsonState` is an
/// 8-variant unit enum in `json-event-parser 0.2.3`, so `Vec<JsonState>` starts at
/// capacity 8 and doubles in powers of two).
const PARSER_STATE_BYTES: usize = 1;
const PARSER_STATE_MIN_CAP: usize = 8;

/// Diagnostic `String` lengths allocated by `json-event-parser 0.2.3`'s
/// `LowLevelJsonParser` when reporting syntax errors (including recovery paths that hold
/// the diagnostic concurrently with `buffered_event`).
const OBJECT_COLON_DIAGNOSTIC_BYTES: u64 = "Object keys must be strings".len() as u64;
const ARRAY_COMMA_DIAGNOSTIC_BYTES: u64 =
    "Array values must be followed by a comma to add a new value or a squared bracket to end the array"
        .len() as u64;
const OBJECT_COMMA_DIAGNOSTIC_BYTES: u64 =
    "Object values must be followed by a comma to add a new value or a curly bracket to end the object"
        .len() as u64;
const TRAILING_COMMA_DIAGNOSTIC_BYTES: u64 = "Trailing commas are not allowed".len() as u64;
const ROOT_DONE_DIAGNOSTIC_BYTES: u64 = "The JSON already contains one root element".len() as u64;
const VALUE_SYNTAX_DIAGNOSTIC_BYTES: u64 =
    "Unexpected closing square bracket, no array to close".len() as u64;
/// Peak heap bytes for `format!`-built diagnostics in `JsonLexer` and `check_stack_size`
/// (messages are at most ~82 bytes, so `String` growth peaks at 64 + 128 = 192 bytes).
const LEXER_DIAGNOSTIC_PEAK_BYTES: u64 = 192;
/// `LowLevelJsonParser`'s default `MAX_STATE_STACK_SIZE`.
const PARSER_MAX_STATE_STACK_SIZE: usize = 65_536;

/// Inline bitset capacity (1,024 nested containers) so valid Sparkles nesting depths keep
/// container ancestry stack-only with zero heap overhead.
const INLINE_CONTAINER_WORDS: usize = 16;
const INLINE_CONTAINER_BITS: usize = INLINE_CONTAINER_WORDS * 64;
const OVERFLOW_BITS_MIN_CAP: usize = 4;

fn stack_bytes_for_cap(cap: usize) -> u64 {
    (cap as u64).saturating_mul(PARSER_STATE_BYTES as u64)
}

fn next_stack_cap(depth: usize) -> usize {
    depth.max(PARSER_STATE_MIN_CAP).next_power_of_two()
}

/// Mirrors `LowLevelJsonParser`'s exact top-of-stack grammar state across `parse_next`
/// calls (until the first syntax error or `Eof`, after which `svcread` stops parsing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParserState {
    RootValue,
    RootDone,
    ObjectKey,
    ObjectKeyOrEnd,
    ObjectColon,
    ObjectValue,
    ObjectCommaOrEnd,
    ArrayValue,
    ArrayValueOrEnd,
    ArrayCommaOrEnd,
}

/// Exact shadow of `LowLevelJsonParser`'s grammar state, stack depth/capacity, and
/// `JsonLexer::is_start` flag so `parser_step_peak` charges recovery clones and
/// diagnostics only on actual error paths and charges zero copy bytes on valid borrowed
/// tokens.
struct ParserGrammar {
    top: ParserState,
    depth: usize,
    stack_cap: usize,
    lexer_at_start: bool,
    /// Bit `i` is `1` when open container level `i` is an object (`ObjectCommaOrEnd`
    /// when resumed) and `0` when it is an array (`ArrayCommaOrEnd` when resumed).
    inline_bits: [u64; INLINE_CONTAINER_WORDS],
    overflow_bits: Vec<u64>,
}

impl ParserGrammar {
    fn new() -> Self {
        Self {
            top: ParserState::RootValue,
            depth: 0,
            stack_cap: 0,
            lexer_at_start: true,
            inline_bits: [0; INLINE_CONTAINER_WORDS],
            overflow_bits: Vec::new(),
        }
    }

    fn depth(&self) -> usize {
        self.depth
    }

    fn live_stack_bytes(&self) -> u64 {
        stack_bytes_for_cap(self.stack_cap).saturating_add(
            (self.overflow_bits.capacity() as u64).saturating_mul(size_of::<u64>() as u64),
        )
    }

    fn push_stack_peak_bytes(&self) -> u64 {
        let next_depth = self.depth.saturating_add(1);
        let parser_stack = if next_depth > self.stack_cap {
            vec_growth_peak(self.stack_cap, next_depth.max(PARSER_STATE_MIN_CAP))
                .saturating_mul(PARSER_STATE_BYTES as u64)
        } else {
            stack_bytes_for_cap(self.stack_cap)
        };
        let overflow_stack = if next_depth > INLINE_CONTAINER_BITS {
            let need_words = (next_depth - INLINE_CONTAINER_BITS).div_ceil(64);
            let cur_cap = self.overflow_bits.capacity();
            if need_words > cur_cap {
                vec_growth_peak(cur_cap, need_words.max(OVERFLOW_BITS_MIN_CAP))
                    .saturating_mul(size_of::<u64>() as u64)
            } else {
                (cur_cap as u64).saturating_mul(size_of::<u64>() as u64)
            }
        } else {
            (self.overflow_bits.capacity() as u64).saturating_mul(size_of::<u64>() as u64)
        };
        parser_stack.saturating_add(overflow_stack)
    }

    fn set_container_bit(&mut self, level: usize, is_object: bool) {
        if level < INLINE_CONTAINER_BITS {
            let word = &mut self.inline_bits[level / 64];
            let mask = 1u64 << (level % 64);
            if is_object {
                *word |= mask;
            } else {
                *word &= !mask;
            }
        } else {
            let bit = level - INLINE_CONTAINER_BITS;
            let word_idx = bit / 64;
            if word_idx >= self.overflow_bits.len() {
                self.overflow_bits.resize(word_idx + 1, 0);
            }
            let mask = 1u64 << (bit % 64);
            if is_object {
                self.overflow_bits[word_idx] |= mask;
            } else {
                self.overflow_bits[word_idx] &= !mask;
            }
        }
    }

    fn is_object_at(&self, level: usize) -> bool {
        if level < INLINE_CONTAINER_BITS {
            (self.inline_bits[level / 64] & (1u64 << (level % 64))) != 0
        } else {
            let bit = level - INLINE_CONTAINER_BITS;
            let word_idx = bit / 64;
            self.overflow_bits
                .get(word_idx)
                .is_some_and(|w| (*w & (1u64 << (bit % 64))) != 0)
        }
    }

    /// Record whether `JsonLexer::is_start` cleared when `parse_next` inspected
    /// `input_len` bytes with `ending`.
    fn note_lexer_call(&mut self, input_len: usize, ending: bool) {
        if self.lexer_at_start && (input_len >= 3 || ending) {
            self.lexer_at_start = false;
        }
    }

    /// Advance across a valid separator consumed by `parse_next` before returning `None`.
    fn on_incomplete(&mut self, consumed: &[u8]) {
        match self.top {
            ParserState::ObjectColon if consumed.contains(&b':') => {
                self.top = ParserState::ObjectValue;
            }
            ParserState::ObjectCommaOrEnd if consumed.contains(&b',') => {
                self.top = ParserState::ObjectKey;
            }
            ParserState::ArrayCommaOrEnd if consumed.contains(&b',') => {
                self.top = ParserState::ArrayValue;
            }
            _ => {}
        }
    }

    /// Advance after `parse_next` emitted a valid `JsonEvent`.
    fn on_event(&mut self, event: &JsonEvent<'_>) {
        match event {
            JsonEvent::StartObject => {
                self.set_container_bit(self.depth, true);
                self.depth += 1;
                if self.depth > self.stack_cap {
                    self.stack_cap = next_stack_cap(self.depth);
                }
                self.top = ParserState::ObjectKeyOrEnd;
            }
            JsonEvent::StartArray => {
                self.set_container_bit(self.depth, false);
                self.depth += 1;
                if self.depth > self.stack_cap {
                    self.stack_cap = next_stack_cap(self.depth);
                }
                self.top = ParserState::ArrayValueOrEnd;
            }
            JsonEvent::EndObject | JsonEvent::EndArray => {
                self.depth = self.depth.saturating_sub(1);
                self.top = if self.depth == 0 {
                    ParserState::RootDone
                } else if self.is_object_at(self.depth - 1) {
                    ParserState::ObjectCommaOrEnd
                } else {
                    ParserState::ArrayCommaOrEnd
                };
            }
            JsonEvent::ObjectKey(_) => {
                self.top = ParserState::ObjectColon;
            }
            JsonEvent::String(_)
            | JsonEvent::Number(_)
            | JsonEvent::Boolean(_)
            | JsonEvent::Null => {
                self.top = if self.depth == 0 {
                    ParserState::RootDone
                } else if self.is_object_at(self.depth - 1) {
                    ParserState::ObjectCommaOrEnd
                } else {
                    ParserState::ArrayCommaOrEnd
                };
            }
            JsonEvent::Eof => {}
        }
    }
}

enum TokenScratch {
    /// Complete or incomplete token whose lexer scratch (or recovery clone) is `bytes`.
    /// When `reaches_parser` is true, `apply_new_token` will process the token and any
    /// syntax-error diagnostic can coexist with `bytes`.
    Ok { bytes: u64, reaches_parser: bool },
    /// Lexer fails before `apply_new_token`; `bytes` already covers the lexer diagnostic.
    LexerErr(u64),
}

fn eof_diagnostic_bytes(state: ParserState) -> u64 {
    match state {
        ParserState::RootDone => 0,
        ParserState::ObjectColon | ParserState::ObjectKey | ParserState::ObjectKeyOrEnd => {
            OBJECT_COLON_DIAGNOSTIC_BYTES
        }
        ParserState::ObjectCommaOrEnd => OBJECT_COMMA_DIAGNOSTIC_BYTES,
        ParserState::ArrayCommaOrEnd => ARRAY_COMMA_DIAGNOSTIC_BYTES,
        ParserState::RootValue
        | ParserState::ObjectValue
        | ParserState::ArrayValueOrEnd
        | ParserState::ArrayValue => VALUE_SYNTAX_DIAGNOSTIC_BYTES,
    }
}

/// Conservative peak heap bytes `LowLevelJsonParser::parse_next` may hold or allocate
/// while processing the next token in `slice`:
/// - its live `state_stack` (and any overflow bitset) plus reallocation peak when the
///   next token opens `[` or `{`;
/// - an owned decoded `String` (plus `from_utf8_lossy` chunk) when a JSON string has
///   escapes or non-ASCII bytes;
/// - an owned `String` clone in `LowLevelJsonParser::buffered_event` plus the coexisting
///   diagnostic `String` when error recovery on a missing `:` (`ObjectColon`) or `,`
///   (`ArrayCommaOrEnd`) buffers a borrowed plain-ASCII string or number event;
/// - bounded parser or lexer syntax-error diagnostics on malformed input.
fn parser_step_peak(mut slice: &[u8], ending: bool, grammar: &ParserGrammar) -> u64 {
    let live_stack = grammar.live_stack_bytes();
    if grammar.lexer_at_start {
        if slice.len() < 3 && !ending {
            return live_stack;
        }
        if slice.starts_with(&[0xEF, 0xBB, 0xBF]) {
            slice = &slice[3..];
        }
    }
    slice = skip_json_ws(slice);
    // `LowLevelJsonParser::parse_next` loops past a valid `:` or `,` separator and
    // decodes the following value or key in the same call. Advance the effective state
    // only when the separator is valid for the tracked grammar state.
    let mut state = grammar.top;
    match (state, slice.first().copied()) {
        (ParserState::ObjectColon, Some(b':')) => {
            state = ParserState::ObjectValue;
            slice = skip_json_ws(&slice[1..]);
        }
        (ParserState::ObjectCommaOrEnd, Some(b',')) => {
            state = ParserState::ObjectKey;
            slice = skip_json_ws(&slice[1..]);
        }
        (ParserState::ArrayCommaOrEnd, Some(b',')) => {
            state = ParserState::ArrayValue;
            slice = skip_json_ws(&slice[1..]);
        }
        _ => {}
    }

    let Some(&first) = slice.first() else {
        if !ending || state == ParserState::RootDone {
            return live_stack;
        }
        return live_stack.saturating_add(eof_diagnostic_bytes(state));
    };

    // Only `ObjectColon` (without `:`) and `ArrayCommaOrEnd` (without `,` or `]`)
    // invoke error recovery that clones a borrowed string or number into
    // `buffered_event` while holding the diagnostic `String`.
    let recovery_diag = match (state, first) {
        (ParserState::ObjectColon, _) => Some(OBJECT_COLON_DIAGNOSTIC_BYTES),
        (ParserState::ArrayCommaOrEnd, b']') => None,
        (ParserState::ArrayCommaOrEnd, _) => Some(ARRAY_COMMA_DIAGNOSTIC_BYTES),
        _ => None,
    };
    let may_clone_borrowed = recovery_diag.is_some();

    let fixed_diag = recovery_diag.or(match (state, first) {
        (ParserState::RootDone, _) => Some(ROOT_DONE_DIAGNOSTIC_BYTES),
        (ParserState::ObjectCommaOrEnd, b'}') => None,
        (ParserState::ObjectCommaOrEnd, _) => Some(OBJECT_COMMA_DIAGNOSTIC_BYTES),
        (ParserState::ObjectKeyOrEnd, b'}' | b'"') => None,
        (ParserState::ObjectKeyOrEnd, _) => Some(OBJECT_COLON_DIAGNOSTIC_BYTES),
        (ParserState::ObjectKey, b'"') => None,
        (ParserState::ObjectKey, b'}') => Some(TRAILING_COMMA_DIAGNOSTIC_BYTES),
        (ParserState::ObjectKey, _) => Some(OBJECT_COLON_DIAGNOSTIC_BYTES),
        (ParserState::ArrayValue, b']') => Some(TRAILING_COMMA_DIAGNOSTIC_BYTES),
        (ParserState::ArrayValueOrEnd, b']') => None,
        (
            ParserState::RootValue
            | ParserState::ObjectValue
            | ParserState::ArrayValueOrEnd
            | ParserState::ArrayValue,
            b'[' | b'{' | b'"' | b'-' | b'0'..=b'9' | b't' | b'f' | b'n',
        ) => None,
        (
            ParserState::RootValue
            | ParserState::ObjectValue
            | ParserState::ArrayValueOrEnd
            | ParserState::ArrayValue,
            _,
        ) => Some(VALUE_SYNTAX_DIAGNOSTIC_BYTES),
        (ParserState::ObjectColon | ParserState::ArrayCommaOrEnd, _) => None,
    });

    match first {
        b'[' | b'{' => {
            let pushes_container = matches!(
                state,
                ParserState::RootValue
                    | ParserState::ObjectValue
                    | ParserState::ArrayValueOrEnd
                    | ParserState::ArrayValue
                    | ParserState::ObjectColon
                    | ParserState::ArrayCommaOrEnd
            );
            let stack_peak = if pushes_container {
                grammar.push_stack_peak_bytes()
            } else {
                live_stack
            };
            let diag = if pushes_container && grammar.depth() > PARSER_MAX_STATE_STACK_SIZE {
                LEXER_DIAGNOSTIC_PEAK_BYTES
            } else {
                fixed_diag.unwrap_or(0)
            };
            stack_peak.saturating_add(diag)
        }
        b'"' => match string_token_scratch(slice, ending, may_clone_borrowed) {
            TokenScratch::Ok {
                bytes,
                reaches_parser,
            } => {
                let diag = if reaches_parser {
                    fixed_diag.unwrap_or(0)
                } else {
                    0
                };
                live_stack.saturating_add(bytes).saturating_add(diag)
            }
            TokenScratch::LexerErr(bytes) => live_stack.saturating_add(bytes),
        },
        b'-' | b'0'..=b'9' => match number_token_scratch(slice, ending, may_clone_borrowed) {
            TokenScratch::Ok {
                bytes,
                reaches_parser,
            } => {
                let diag = if reaches_parser {
                    fixed_diag.unwrap_or(0)
                } else {
                    0
                };
                live_stack.saturating_add(bytes).saturating_add(diag)
            }
            TokenScratch::LexerErr(bytes) => live_stack.saturating_add(bytes),
        },
        b't' | b'f' | b'n' => match constant_token_scratch(slice, ending) {
            TokenScratch::Ok {
                bytes,
                reaches_parser,
            } => {
                let diag = if reaches_parser {
                    fixed_diag.unwrap_or(0)
                } else {
                    0
                };
                live_stack.saturating_add(bytes).saturating_add(diag)
            }
            TokenScratch::LexerErr(bytes) => live_stack.saturating_add(bytes),
        },
        b'}' | b']' | b',' | b':' => live_stack.saturating_add(fixed_diag.unwrap_or(0)),
        _ => live_stack.saturating_add(LEXER_DIAGNOSTIC_PEAK_BYTES),
    }
}

fn number_token_scratch(slice: &[u8], ending: bool, may_clone_borrowed: bool) -> TokenScratch {
    let Some((&first, rest)) = slice.split_first() else {
        return TokenScratch::Ok {
            bytes: 0,
            reaches_parser: false,
        };
    };
    let mut phase = match first {
        b'-' => NumberPhase::AfterSign,
        b'0' => NumberPhase::AfterZero,
        b'1'..=b'9' => NumberPhase::IntDigits,
        _ => return TokenScratch::LexerErr(LEXER_DIAGNOSTIC_PEAK_BYTES),
    };
    let mut len = 1usize;
    let mut terminated = false;
    for &b in rest {
        if phase.advance(b) {
            terminated = true;
            break;
        }
        len += 1;
    }
    let incomplete_phase = matches!(
        phase,
        NumberPhase::AfterSign
            | NumberPhase::FracStart
            | NumberPhase::ExpStart
            | NumberPhase::ExpSign
    );
    if terminated {
        if incomplete_phase {
            return TokenScratch::LexerErr(LEXER_DIAGNOSTIC_PEAK_BYTES);
        }
        return TokenScratch::Ok {
            bytes: if may_clone_borrowed { len as u64 } else { 0 },
            reaches_parser: true,
        };
    }
    if !ending {
        return TokenScratch::Ok {
            bytes: 0,
            reaches_parser: false,
        };
    }
    if incomplete_phase {
        return TokenScratch::LexerErr(VALUE_SYNTAX_DIAGNOSTIC_BYTES);
    }
    TokenScratch::Ok {
        bytes: if may_clone_borrowed { len as u64 } else { 0 },
        reaches_parser: true,
    }
}

fn constant_token_scratch(slice: &[u8], ending: bool) -> TokenScratch {
    let expected: &[u8] = match slice.first().copied() {
        Some(b't') => b"true",
        Some(b'f') => b"false",
        Some(b'n') => b"null",
        _ => return TokenScratch::LexerErr(LEXER_DIAGNOSTIC_PEAK_BYTES),
    };
    if slice.len() < expected.len() {
        return if ending {
            TokenScratch::LexerErr(VALUE_SYNTAX_DIAGNOSTIC_BYTES)
        } else {
            TokenScratch::Ok {
                bytes: 0,
                reaches_parser: false,
            }
        };
    }
    if slice.starts_with(expected) {
        return TokenScratch::Ok {
            bytes: 0,
            reaches_parser: true,
        };
    }
    let alpha = slice.iter().take_while(|b| b.is_ascii_alphabetic()).count();
    if alpha == slice.len() && !ending {
        return TokenScratch::Ok {
            bytes: 0,
            reaches_parser: false,
        };
    }
    TokenScratch::LexerErr(LEXER_DIAGNOSTIC_PEAK_BYTES)
}

fn string_token_scratch(slice: &[u8], ending: bool, may_clone_borrowed: bool) -> TokenScratch {
    let mut has_escape = false;
    let mut has_non_ascii = false;
    let mut has_lexer_error = false;
    let mut closed = false;
    let mut decoded = 0usize;
    let mut i = 1usize;
    while i < slice.len() {
        let b = slice[i];
        if b == b'\\' {
            has_escape = true;
            i += 1;
            if i >= slice.len() {
                break;
            }
            match slice[i] {
                b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {
                    decoded = decoded.saturating_add(1);
                    i += 1;
                }
                b'u' => {
                    i += 1;
                    if i + 4 > slice.len() {
                        break;
                    }
                    let cp = parse_hex4(&slice[i..i + 4]);
                    i += 4;
                    if let Some(cp) = cp
                        && char::from_u32(cp).is_none()
                    {
                        if !(0xD800..=0xDBFF).contains(&cp) {
                            has_lexer_error = true;
                        }
                        // Surrogate code unit: `read_string` consumes 6 more bytes and
                        // pushes at most one 4-byte UTF-8 character.
                        if i + 6 > slice.len() {
                            break;
                        }
                        let tail = &slice[i..i + 6];
                        if !tail.starts_with(b"\\u")
                            || !parse_hex4(&tail[2..])
                                .is_some_and(|lo| (0xDC00..=0xDFFF).contains(&lo))
                        {
                            has_lexer_error = true;
                        }
                        i += 6;
                        decoded = decoded.saturating_add(4);
                    } else {
                        if cp.is_none() {
                            has_lexer_error = true;
                        }
                        // Single BMP scalar or invalid hex -> `U+FFFD` (at most 3 bytes).
                        decoded = decoded.saturating_add(3);
                    }
                }
                _ => {
                    // Unrecognized 2-byte escape pushes `U+FFFD` (3 UTF-8 bytes) and
                    // records a lexer syntax error.
                    has_lexer_error = true;
                    decoded = decoded.saturating_add(3);
                    i += 1;
                }
            }
        } else if b == b'"' {
            closed = true;
            break;
        } else if b < 0x20 {
            has_lexer_error = true;
            decoded = decoded.saturating_add(1);
            i += 1;
        } else if !b.is_ascii() {
            has_non_ascii = true;
            decoded = decoded.saturating_add(3);
            i += 1;
        } else {
            decoded = decoded.saturating_add(1);
            i += 1;
        }
    }
    let decoded = decoded as u64;
    if !has_escape && !has_non_ascii {
        if has_lexer_error {
            return TokenScratch::LexerErr(LEXER_DIAGNOSTIC_PEAK_BYTES);
        }
        if !closed {
            return if ending {
                TokenScratch::LexerErr(VALUE_SYNTAX_DIAGNOSTIC_BYTES)
            } else {
                TokenScratch::Ok {
                    bytes: 0,
                    reaches_parser: false,
                }
            };
        }
        return TokenScratch::Ok {
            bytes: if may_clone_borrowed { decoded } else { 0 },
            reaches_parser: true,
        };
    }
    // `String::new(); string.push_str(prefix)` allocates `prefix.len()` without rounding
    // to a power of two; a subsequent push can double `old_cap` to `2 * old_cap`, giving
    // an old + new reallocation peak bounded by `3 * decoded + 16`. When `has_escape`
    // and `has_non_ascii` both hold, an owned `from_utf8_lossy` chunk buffer can coexist
    // with `string` during `push_str`, bounded by `4 * decoded + 24`.
    let mut scratch = if has_escape && has_non_ascii {
        decoded.saturating_mul(4).saturating_add(24)
    } else {
        decoded.saturating_mul(3).saturating_add(16)
    };
    if has_non_ascii && std::str::from_utf8(&slice[1..i]).is_err() {
        has_lexer_error = true;
    }
    if has_lexer_error {
        scratch = scratch.saturating_add(LEXER_DIAGNOSTIC_PEAK_BYTES);
        return TokenScratch::LexerErr(scratch);
    }
    if !closed {
        return TokenScratch::Ok {
            bytes: if ending {
                scratch.max(VALUE_SYNTAX_DIAGNOSTIC_BYTES)
            } else {
                scratch
            },
            reaches_parser: false,
        };
    }
    TokenScratch::Ok {
        bytes: scratch,
        reaches_parser: true,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StringPhase {
    Normal,
    Escaped,
    /// Reading the 4 hex bytes after `\u`: `done` bytes read (`0..4`) and accumulated
    /// value while every byte so far is ASCII hex.
    UnicodeHex {
        done: u8,
        val: Option<u32>,
    },
    /// Skipping `rem` raw bytes (1..=6) unconditionally, matching
    /// `JsonLexer::read_string`'s 6-byte surrogate tail consumption after any
    /// `\uD800..=\uDFFF`.
    Skip(u8),
}

impl StringPhase {
    /// Advance by one byte inside a JSON string. Returns `true` when this byte closes
    /// the string at an unescaped `"`.
    fn advance(&mut self, b: u8) -> bool {
        match *self {
            Self::Normal => match b {
                b'\\' => *self = Self::Escaped,
                b'"' => return true,
                _ => {}
            },
            Self::Escaped => {
                *self = if b == b'u' {
                    Self::UnicodeHex {
                        done: 0,
                        val: Some(0),
                    }
                } else {
                    Self::Normal
                };
            }
            Self::UnicodeHex { done, val } => {
                let next_val = val.and_then(|v| {
                    let digit = match b {
                        b'0'..=b'9' => (b - b'0') as u32,
                        b'a'..=b'f' => (b - b'a' + 10) as u32,
                        b'A'..=b'F' => (b - b'A' + 10) as u32,
                        _ => return None,
                    };
                    Some((v << 4) | digit)
                });
                let next_done = done + 1;
                if next_done < 4 {
                    *self = Self::UnicodeHex {
                        done: next_done,
                        val: next_val,
                    };
                } else if next_val.is_some_and(|cp| char::from_u32(cp).is_none()) {
                    *self = Self::Skip(6);
                } else {
                    *self = Self::Normal;
                }
            }
            Self::Skip(rem) => {
                *self = if rem > 1 {
                    Self::Skip(rem - 1)
                } else {
                    Self::Normal
                };
            }
        }
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NumberPhase {
    AfterSign,
    AfterZero,
    IntDigits,
    FracStart,
    FracDigits,
    ExpStart,
    ExpSign,
    ExpDigits,
}

impl NumberPhase {
    /// Advance by one byte of `JsonLexer::read_number`. Returns `true` once this byte
    /// terminates the number (either validly as a delimiter or as a syntax error).
    fn advance(&mut self, b: u8) -> bool {
        match (*self, b) {
            (Self::AfterSign, b'0') => *self = Self::AfterZero,
            (Self::AfterSign, b'1'..=b'9') => *self = Self::IntDigits,
            (Self::AfterZero, b'.') => *self = Self::FracStart,
            (Self::AfterZero, b'e' | b'E') => *self = Self::ExpStart,
            (Self::IntDigits, b'0'..=b'9') => {}
            (Self::IntDigits, b'.') => *self = Self::FracStart,
            (Self::IntDigits, b'e' | b'E') => *self = Self::ExpStart,
            (Self::FracStart, b'0'..=b'9') => *self = Self::FracDigits,
            (Self::FracDigits, b'0'..=b'9') => {}
            (Self::FracDigits, b'e' | b'E') => *self = Self::ExpStart,
            (Self::ExpStart, b'+' | b'-') => *self = Self::ExpSign,
            (Self::ExpStart, b'0'..=b'9') => *self = Self::ExpDigits,
            (Self::ExpSign, b'0'..=b'9') => *self = Self::ExpDigits,
            (Self::ExpDigits, b'0'..=b'9') => {}
            _ => return true,
        }
        false
    }
}

/// State of an incomplete JSON token in `json_start` after `parse_next` returned `None`.
/// Advancing it on each newly read chunk avoids quadratic rescanning of long strings,
/// numbers, or alphabetic runs while retrying `parse_next` immediately when the token
/// closes.
enum IncompleteToken {
    String(StringPhase),
    Number(NumberPhase),
    AlphaRun,
}

impl IncompleteToken {
    fn from_slice(mut slice: &[u8], allow_bom: bool) -> Option<Self> {
        if allow_bom && slice.starts_with(&[0xEF, 0xBB, 0xBF]) {
            slice = &slice[3..];
        }
        slice = skip_json_ws(slice);
        let (&first, rest) = slice.split_first()?;
        match first {
            b'"' => {
                let mut token = Self::String(StringPhase::Normal);
                if token.advance_slice(rest) {
                    None
                } else {
                    Some(token)
                }
            }
            b'-' | b'0'..=b'9' => {
                let phase = match first {
                    b'-' => NumberPhase::AfterSign,
                    b'0' => NumberPhase::AfterZero,
                    _ => NumberPhase::IntDigits,
                };
                let mut token = Self::Number(phase);
                if token.advance_slice(rest) {
                    None
                } else {
                    Some(token)
                }
            }
            b't' | b'f' | b'n' => {
                let expected_len = if first == b'f' { 5 } else { 4 };
                if slice.len() >= expected_len && slice.iter().all(|b| b.is_ascii_alphabetic()) {
                    Some(Self::AlphaRun)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Advance over newly read bytes. Returns `true` as soon as the incomplete token
    /// closes or hits a terminating/invalid byte so `parse_next` can retry immediately.
    fn advance_slice(&mut self, bytes: &[u8]) -> bool {
        match self {
            Self::String(phase) => {
                for &b in bytes {
                    if phase.advance(b) {
                        return true;
                    }
                }
                false
            }
            Self::Number(phase) => {
                for &b in bytes {
                    if phase.advance(b) {
                        return true;
                    }
                }
                false
            }
            Self::AlphaRun => bytes.iter().any(|b| !b.is_ascii_alphabetic()),
        }
    }
}

/// How much a JSON head keeps for each byte of it. The shortest variable, `"a",`, is
/// four bytes. sparesults keeps it in a vector that may double, in a map from its name
/// to its column, at about twice the size of the entries for the map's spare room, and
/// in the list of variables it hands out. The head's bytes are also held twice, by the
/// copy read here and by the parser.
pub(super) fn head_factor() -> u64 {
    let entry = size_of::<String>() + 8 + size_of::<usize>() + 1;
    let retained = 2 * (size_of::<Variable>() + 8) + 2 * entry + size_of::<Variable>() + 8;
    (retained as u64).div_ceil(4) + 2
}

/// How much a buffered JSON answer keeps for each byte of it, the larger of two worst
/// cases. An empty binding, `{},`, is three bytes and one pair of a vector of names and
/// a vector of terms in a vector that may double. The shortest bound value,
/// `"a":{"type":"uri","value":""},`, is thirty bytes and one name and one term, each in
/// a vector that may double.
pub(super) fn buffered_factor() -> u64 {
    let empty = 2 * size_of::<(Vec<String>, Vec<Term>)>();
    let value = 2 * (size_of::<String>() + 8 + size_of::<Term>());
    (empty as u64).div_ceil(3).max((value as u64).div_ceil(30)) + 2
}

/// What the start of a JSON response says about its order.
pub(super) struct Start {
    /// the bytes read so far, which the parser reads first
    pub(super) bytes: Vec<u8>,
    /// the length of the part of `bytes` the order was decided from
    pub(super) decided: usize,
    /// Whether the results come before the head's variables, which sparesults then
    /// buffers.
    pub(super) late: bool,
    parser: LowLevelJsonParser,
    ending: bool,
    grammar: ParserGrammar,
    in_head: bool,
}

/// Read the start of a JSON response up to its top-level `results` or `boolean` member,
/// pre-admitting buffer growth on `charge`, compacting consumed JSON whitespace so a long
/// whitespace prefix stays bounded, and polling `ctx.check()` between reads. A response
/// that is not valid JSON or ends first is left for sparesults to refuse, as one whose
/// head comes first.
pub(super) fn json_start(
    body: &mut dyn Read,
    charge: &mut StagingCharge,
    ctx: &Ctx,
) -> io::Result<Start> {
    json_start_inner(body, charge, ctx).map(|(start, _)| start)
}

fn json_start_inner(
    body: &mut dyn Read,
    charge: &mut StagingCharge,
    ctx: &Ctx,
) -> io::Result<(Start, usize)> {
    let mut parser = LowLevelJsonParser::new();
    let mut grammar = ParserGrammar::new();
    let mut bytes = Vec::new();
    let (mut at, mut ending) = (0usize, false);
    let (mut in_head, mut vars, mut seen_token) = (false, false, false);
    let mut incomplete_retries = 0usize;
    loop {
        let prev_at = at;
        if at < bytes.len() || ending {
            let step_peak = parser_step_peak(&bytes[at..], ending, &grammar);
            if step_peak > 0 {
                let parse_peak = (bytes.capacity() as u64).saturating_add(step_peak);
                if parse_peak > charge.bytes() {
                    charge.resize(parse_peak).map_err(io::Error::other)?;
                }
            }
        }
        let result = parser.parse_next(&bytes[at..], ending);
        grammar.note_lexer_call(bytes.len() - prev_at, ending);
        at += result.consumed_bytes;
        if result.event.is_some() {
            seen_token = true;
        } else {
            grammar.on_incomplete(&bytes[prev_at..at]);
        }
        // `Some(late)` once the order is known
        let decided = match result.event {
            None if ending => Some(false),
            None => None,
            Some(Err(_)) | Some(Ok(JsonEvent::Eof)) => Some(false),
            Some(Ok(event)) => {
                let prev_depth = grammar.depth();
                grammar.on_event(&event);
                match event {
                    JsonEvent::StartObject | JsonEvent::StartArray => continue,
                    JsonEvent::EndObject | JsonEvent::EndArray => {
                        if prev_depth == 2 && in_head && event == JsonEvent::EndObject {
                            in_head = false;
                        }
                        if grammar.depth() == 0 {
                            Some(false)
                        } else {
                            continue;
                        }
                    }
                    JsonEvent::ObjectKey(key) if prev_depth == 1 => match key.as_ref() {
                        "results" => Some(!vars),
                        "boolean" => Some(false),
                        other => {
                            in_head = other == "head";
                            continue;
                        }
                    },
                    JsonEvent::ObjectKey(key) if prev_depth == 2 && in_head && key == "vars" => {
                        vars = true;
                        continue;
                    }
                    _ => continue,
                }
            }
        };
        if let Some(late) = decided {
            return Ok((
                Start {
                    bytes,
                    decided: at,
                    late,
                    parser,
                    ending,
                    grammar,
                    in_head,
                },
                incomplete_retries,
            ));
        }
        // Compact consumed pure whitespace so a long whitespace prefix before the root
        // object or between tokens does not grow the lookahead buffer. Before the first
        // token, retain one leading space so replaying `bytes` into a fresh lexer never
        // promotes a later misplaced UTF-8 BOM to byte zero.
        if at > prev_at {
            if !seen_token {
                let tail = bytes.len() - at;
                bytes.copy_within(at.., 1);
                bytes[0] = b' ';
                bytes.truncate(1 + tail);
                at = 1;
            } else if bytes[prev_at..at]
                .iter()
                .all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
                && at - prev_at > 1
            {
                let drop_from = prev_at + 1;
                let dropped = at - drop_from;
                bytes.copy_within(at.., drop_from);
                bytes.truncate(bytes.len() - dropped);
                at = drop_from;
            }
        }
        if at < bytes.len() {
            incomplete_retries += 1;
        }
        let mut incomplete = IncompleteToken::from_slice(&bytes[at..], grammar.lexer_at_start);
        loop {
            ctx.check().map_err(io::Error::other)?;
            let start = bytes.len();
            let next_len = start
                .checked_add(READ_STEP)
                .ok_or_else(|| io::Error::new(io::ErrorKind::OutOfMemory, "JSON head too large"))?;
            let peak = vec_growth_peak(bytes.capacity(), next_len)
                .saturating_add(grammar.live_stack_bytes());
            let target = if charge.is_cursor() {
                (next_len as u64)
                    .saturating_mul(head_factor())
                    .max(peak)
                    .saturating_add(SLACK)
            } else {
                peak
            };
            if target > charge.bytes() {
                charge.resize(target).map_err(io::Error::other)?;
            }
            bytes.resize(next_len, 0);
            let n = body.read(&mut bytes[start..])?;
            bytes.truncate(start + n);
            ending = n == 0;
            if ending {
                break;
            }
            let Some(token) = incomplete.as_mut() else {
                break;
            };
            if token.advance_slice(&bytes[start..]) {
                break;
            }
        }
    }
}

/// Drain and validate a late-head JSON response to EOF before handing it to sparesults.
///
/// sparesults 0.3.5 switches to an iterator over its buffered bindings as soon as the
/// late `"vars"` array closes and stops reading the underlying stream, so a response
/// truncated after `"vars"` or followed by trailing garbage would otherwise look like a
/// clean EOF. Reading and validating to `JsonEvent::Eof` here catches both, polls
/// `ctx.check()` before every body read, grows the unread window geometrically for large
/// incomplete tokens so validation stays linear, and pre-admits memory on `charge` before
/// every growth.
pub(super) fn read_late_json(
    start: Start,
    body: &mut dyn Read,
    charge: &mut StagingCharge,
    ctx: &Ctx,
) -> io::Result<Vec<u8>> {
    read_late_json_inner(start, body, charge, ctx).map(|(bytes, _)| bytes)
}

fn read_late_json_inner(
    start: Start,
    body: &mut dyn Read,
    charge: &mut StagingCharge,
    ctx: &Ctx,
) -> io::Result<(Vec<u8>, usize)> {
    let Start {
        mut bytes,
        decided: mut at,
        mut parser,
        mut ending,
        mut grammar,
        mut in_head,
        ..
    } = start;
    let mut expect_links_array = false;
    let mut in_links = false;
    let mut links_depth = 0usize;
    let mut incomplete_retries = 0usize;
    loop {
        let prev_at = at;
        if at < bytes.len() || ending {
            let step_peak = parser_step_peak(&bytes[at..], ending, &grammar);
            if step_peak > 0 {
                let parse_peak = (bytes.capacity() as u64).saturating_add(step_peak);
                if parse_peak > charge.bytes() {
                    charge.resize(parse_peak).map_err(io::Error::other)?;
                }
            }
        }
        let result = parser.parse_next(&bytes[at..], ending);
        grammar.note_lexer_call(bytes.len() - prev_at, ending);
        at += result.consumed_bytes;
        match result.event {
            None if ending => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected end of JSON response",
                ));
            }
            None => {
                grammar.on_incomplete(&bytes[prev_at..at]);
            }
            Some(Err(e)) => {
                return Err(io::Error::new(io::ErrorKind::InvalidData, e));
            }
            Some(Ok(JsonEvent::Eof)) => {
                if grammar.depth() != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unexpected end of JSON response",
                    ));
                }
                drop(parser);
                drop(grammar);
                let target = (bytes.capacity() as u64)
                    .saturating_add((bytes.len() as u64).saturating_mul(buffered_factor()))
                    .saturating_add(SLACK);
                if target != charge.bytes() {
                    charge.resize(target).map_err(io::Error::other)?;
                }
                return Ok((bytes, incomplete_retries));
            }
            Some(Ok(event)) => {
                let prev_depth = grammar.depth();
                grammar.on_event(&event);
                if expect_links_array {
                    expect_links_array = false;
                    if event != JsonEvent::StartArray {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "SPARQL JSON results links must be an array",
                        ));
                    }
                    in_links = true;
                    links_depth = grammar.depth();
                    continue;
                }
                if in_links && prev_depth == links_depth {
                    match event {
                        JsonEvent::String(_) => continue,
                        JsonEvent::EndArray => {
                            in_links = false;
                            continue;
                        }
                        _ => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "Links in the links array must be strings",
                            ));
                        }
                    }
                }
                match event {
                    JsonEvent::EndObject if prev_depth == 2 && in_head => {
                        in_head = false;
                    }
                    JsonEvent::ObjectKey(key) if prev_depth == 1 => {
                        in_head = key.as_ref() == "head";
                    }
                    JsonEvent::ObjectKey(key) if prev_depth == 2 && in_head && key == "links" => {
                        expect_links_array = true;
                    }
                    _ => {}
                }
                continue;
            }
        }
        // Geometrically grow the bytes supplied before retrying the same incomplete token
        // so scanning a multi-megabyte literal stays O(N) instead of O(N^2), while still
        // polling `ctx.check()` and pre-admitting memory before every bounded body read.
        let unparsed = bytes.len() - at;
        let target_unparsed = if unparsed == 0 {
            0
        } else {
            incomplete_retries += 1;
            unparsed.saturating_mul(2).max(READ_STEP)
        };
        loop {
            ctx.check().map_err(io::Error::other)?;
            let cur = bytes.len();
            let cap = bytes.capacity();
            let step = if cap > cur && cap - cur >= READ_STEP / 2 {
                (cap - cur).min(READ_STEP)
            } else {
                READ_STEP
            };
            let next_len = cur.checked_add(step).ok_or_else(|| {
                io::Error::new(io::ErrorKind::OutOfMemory, "JSON response too large")
            })?;
            // Pre-admit raw staging growth plus live parser stack before `body.read`;
            // size the buffered replay reserve against bytes actually received so a tiny
            // complete response is not rejected before the read can observe EOF.
            let vec_peak =
                vec_growth_peak(cap, next_len).saturating_add(grammar.live_stack_bytes());
            if vec_peak > charge.bytes() {
                charge.resize(vec_peak).map_err(io::Error::other)?;
            }
            bytes.resize(next_len, 0);
            let n = body.read(&mut bytes[cur..])?;
            bytes.truncate(cur + n);
            ending = n == 0;
            if n > 0 {
                let replay_target = (bytes.capacity() as u64)
                    .saturating_add((bytes.len() as u64).saturating_mul(buffered_factor()))
                    .saturating_add(SLACK)
                    .max((bytes.capacity() as u64).saturating_add(grammar.live_stack_bytes()));
                if replay_target > charge.bytes() {
                    charge.resize(replay_target).map_err(io::Error::other)?;
                }
            }
            if ending || target_unparsed == 0 || bytes.len() - at >= target_unparsed {
                break;
            }
        }
    }
}

/// What a streamed answer shares with the reader under its parser: the bytes read since
/// the parser last yielded a solution, and whether it has yielded one.
#[derive(Default)]
pub(super) struct Window {
    since: AtomicU64,
    yielded: AtomicBool,
}

impl Window {
    /// The parser has yielded a solution.
    pub(super) fn solution(&self) {
        self.since.store(0, Ordering::Relaxed);
        self.yielded.store(true, Ordering::Relaxed);
    }
}

/// A reader that charges what the parser above it may hold, before it hands it bytes,
/// and polls query controls between reads.
pub(super) struct Metered<R> {
    inner: R,
    charge: OwnedCharge,
    /// the charge for the head, and for a buffered answer
    base: u64,
    /// Whether the parser buffers the answer before its first solution, so that each of
    /// those bytes is charged [`buffered_factor`] times.
    late: bool,
    /// Whether `charge` already covers the entire in-memory buffer and parsed solutions
    /// (for a pre-validated late-head response).
    precharged: bool,
    window: Arc<Window>,
    /// the most bytes read between two solutions
    high: u64,
}

impl<R: Read> Metered<R> {
    pub(super) fn new(
        inner: R,
        charge: OwnedCharge,
        base: u64,
        late: bool,
        window: Arc<Window>,
    ) -> Self {
        Self {
            inner,
            charge,
            base,
            late,
            precharged: false,
            window,
            high: 0,
        }
    }

    /// Wrap an already validated and charged in-memory late-head JSON buffer, keeping
    /// its [`OwnedCharge`] live until the parser drops without double-charging window
    /// bytes.
    pub(super) fn precharged(inner: R, charge: OwnedCharge, window: Arc<Window>) -> Self {
        let base = charge.bytes();
        Self {
            inner,
            charge,
            base,
            late: false,
            precharged: true,
            window,
            high: 0,
        }
    }
}

impl<R: Read> Read for Metered<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.charge.check().map_err(io::Error::other)?;
        let n = self.inner.read(buf)?;
        if self.precharged {
            return Ok(n);
        }
        let since = self.window.since.fetch_add(n as u64, Ordering::Relaxed) + n as u64;
        if self.late && !self.window.yielded.load(Ordering::Relaxed) {
            self.base = self
                .base
                .saturating_add((n as u64).saturating_mul(buffered_factor()));
        } else {
            self.high = self.high.max(since);
        }
        let target = self
            .base
            .saturating_add(self.high.saturating_mul(WINDOW_FACTOR))
            .saturating_add(SLACK);
        if target > self.charge.bytes() {
            self.charge.resize(target).map_err(io::Error::other)?;
        }
        Ok(n)
    }
}

/// The charge a streamed answer starts with, for the query of `ctx`.
pub(super) fn charge(ctx: &Arc<Ctx>) -> crate::error::Result<OwnedCharge> {
    OwnedCharge::new(ctx, SLACK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::store::{Store, StoreOptions};
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn the_factors_cover_their_worst_cases() {
        // an empty binding of a buffered answer keeps at least a pair of vectors
        assert!(buffered_factor() * 3 >= 2 * size_of::<(Vec<String>, Vec<Term>)>() as u64);
        assert!(head_factor() * 4 >= 2 * size_of::<Variable>() as u64);
    }

    struct CancelAfterFirst {
        chunks: VecDeque<Vec<u8>>,
        cancel: Arc<AtomicBool>,
        reads: Arc<AtomicUsize>,
        cancel_after: usize,
    }

    impl Read for CancelAfterFirst {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n_reads = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
            let Some(chunk) = self.chunks.pop_front() else {
                return Ok(0);
            };
            let n = chunk.len().min(buf.len());
            buf[..n].copy_from_slice(&chunk[..n]);
            if n < chunk.len() {
                self.chunks.push_front(chunk[n..].to_vec());
            }
            if n_reads == self.cancel_after {
                self.cancel.store(true, Ordering::SeqCst);
            }
            Ok(n)
        }
    }

    #[test]
    fn read_late_json_rejects_truncation_and_trailing_garbage() {
        let store = Store::in_memory(StoreOptions::default());
        let mut raw = Ctx::new(store.snapshot());
        raw.mem_limit = 16 << 20;
        raw.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(raw);
        ctx.attach_owner();

        let bad_cases = [
            br#"{"results":{"bindings":[{"s":{"type":"uri","value":"urn:s"}}]},"head":{"vars":["s"]"#.as_slice(),
            br#"{"results":{"bindings":[{"s":{"type":"uri","value":"urn:s"}}]},"head":{"vars":["s"]}} trailing"#.as_slice(),
            br#"{"results":{"bindings":[]},"head":{"vars":["s"],"links":123}}"#.as_slice(),
        ];
        for body in bad_cases {
            let mut cur = io::Cursor::new(body);
            let mut ch = StagingCharge::from_cursor(charge(&ctx).unwrap());
            let start = json_start(&mut cur, &mut ch, &ctx).unwrap();
            assert!(start.late);
            let res = read_late_json(start, &mut cur, &mut ch, &ctx);
            assert!(
                res.is_err(),
                "expected error for {:?}",
                String::from_utf8_lossy(body)
            );
        }

        let valid = br#"{"results":{"bindings":[{"s":{"type":"uri","value":"urn:s"}}]},"head":{"vars":["s"],"links":["http://example.org/"]}}"#;
        let mut cur = io::Cursor::new(valid.as_slice());
        let mut ch = StagingCharge::from_cursor(charge(&ctx).unwrap());
        let start = json_start(&mut cur, &mut ch, &ctx).unwrap();
        assert!(start.late);
        let bytes = read_late_json(start, &mut cur, &mut ch, &ctx).unwrap();
        assert_eq!(bytes, valid);
    }

    #[test]
    fn read_late_json_polls_cancellation_between_body_reads() {
        let store = Store::in_memory(StoreOptions::default());
        let mut raw = Ctx::new(store.snapshot());
        raw.mem_limit = 16 << 20;
        raw.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(raw);
        ctx.attach_owner();

        let reads = Arc::new(AtomicUsize::new(0));
        let mut chunks = VecDeque::new();
        chunks.push_back(
            br#"{"results":{"bindings":[{"s":{"type":"uri","value":"urn:s0"}}"#.to_vec(),
        );
        for i in 1..10usize {
            chunks
                .push_back(format!(r#",{{"s":{{"type":"uri","value":"urn:s{i}"}}}}"#).into_bytes());
        }
        chunks.push_back(br#"]},"head":{"vars":["s"]}}"#.to_vec());
        let mut body = CancelAfterFirst {
            chunks,
            cancel: ctx.cancel.clone(),
            reads: reads.clone(),
            cancel_after: 1,
        };
        let mut ch = StagingCharge::from_cursor(charge(&ctx).unwrap());
        let start = json_start(&mut body, &mut ch, &ctx).unwrap();
        assert!(start.late);
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        let err = read_late_json(start, &mut body, &mut ch, &ctx).unwrap_err();
        assert!(matches!(crate::codec::io_error(err), Error::Cancelled));
        assert_eq!(
            reads.load(Ordering::SeqCst),
            1,
            "read_late_json read past cancellation"
        );
    }

    /// A long whitespace prefix before a small head-first JSON response is compacted as
    /// it streams in, so eager and cursor probing both stay within one 4 KiB buffer.
    #[test]
    fn json_start_compacts_long_whitespace_prefix_in_bounded_memory() {
        let store = Store::in_memory(StoreOptions::default());
        let mut raw = Ctx::new(store.snapshot());
        raw.mem_limit = 64 << 10;
        let ctx = Arc::new(raw);
        ctx.attach_owner();

        let mut payload = vec![b' '; 256 << 10];
        let body_json = br#"{"head":{"vars":["s"]},"results":{"bindings":[{"s":{"type":"uri","value":"urn:s"}}]}}"#;
        payload.extend_from_slice(body_json);

        let mut cur = io::Cursor::new(payload.as_slice());
        let mut staging = eager_charge(&ctx).unwrap();
        let start = json_start(&mut cur, &mut staging, &ctx).unwrap();
        assert!(!start.late);
        assert!(
            start.bytes.capacity() <= 2 * READ_STEP,
            "whitespace prefix grew lookahead buffer to {} bytes",
            start.bytes.capacity()
        );
        let head = ChargedCursor::new(start.bytes, staging).unwrap();
        let mut combined = Vec::new();
        head.chain(cur).read_to_end(&mut combined).unwrap();
        assert_eq!(combined[0], b' ');
        assert_eq!(&combined[1..], body_json);
    }

    /// Eager head probing pre-admits owned unescaped-string scratch inside
    /// `LowLevelJsonParser::parse_next` even when the raw staging vector and its growth
    /// peak fit inside the query memory budget.
    #[test]
    fn eager_json_probe_pre_admits_escaped_string_scratch() {
        let store = Store::in_memory(StoreOptions::default());
        let mut raw = Ctx::new(store.snapshot());
        // 27 KiB escaped string fits in a 32 KiB Vec<u8> (largest Vec growth peak = 48 KiB),
        // but coexists with the parser's owned decoded String (~32-48 KiB peak).
        raw.mem_limit = 56 << 10;
        let ctx = Arc::new(raw);
        ctx.attach_owner();

        let escaped = format!(r#"\"{}"#, "a".repeat(27 << 10));
        let payload = format!(
            r#"{{"head":{{"links":["{escaped}"],"vars":["s"]}},"results":{{"bindings":[]}}}}"#
        );
        assert!(payload.len() < (32 << 10));

        let mut cur = io::Cursor::new(payload.as_bytes());
        let mut staging = eager_charge(&ctx).unwrap();
        let err = json_start(&mut cur, &mut staging, &ctx).err().unwrap();
        assert!(
            matches!(
                crate::codec::io_error(err),
                Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
            ),
            "escaped head string scratch must refuse on memory budget"
        );
    }

    /// Eager late-head validation charges `sparesults`'s buffered bindings through replay,
    /// refusing 10,000 empty bindings (~30 KiB wire, ~1 MiB buffered) under a 256 KiB
    /// budget (above a single 4 KiB read's allowance) even though the raw vector fits.
    #[test]
    fn eager_late_json_reserves_buffered_bindings_through_replay() {
        let store = Store::in_memory(StoreOptions::default());
        let mut raw = Ctx::new(store.snapshot());
        raw.mem_limit = 256 << 10;
        let ctx = Arc::new(raw);
        ctx.attach_owner();

        let bindings = vec!["{}"; 10_000].join(",");
        let payload =
            format!(r#"{{"results":{{"bindings":[{bindings}]}},"head":{{"vars":["s"]}}}}"#);
        assert!(payload.len() < (32 << 10));

        let mut cur = io::Cursor::new(payload.as_bytes());
        let mut staging = eager_charge(&ctx).unwrap();
        let start = json_start(&mut cur, &mut staging, &ctx).unwrap();
        assert!(start.late);
        let err = read_late_json(start, &mut cur, &mut staging, &ctx).unwrap_err();
        assert!(
            matches!(
                crate::codec::io_error(err),
                Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
            ),
            "late-head buffered bindings must refuse on eager memory budget"
        );
    }

    /// `parser_step_peak` pre-admits strings immediately following `,` or `:`,
    /// non-power-of-two initial string growth when an escape follows a long ASCII prefix,
    /// and 2 -> 3 byte `U+FFFD` expansion on invalid ASCII escapes.
    #[test]
    fn eager_json_probe_pre_admits_separator_prefix_and_invalid_escape_scratch() {
        let store = Store::in_memory(StoreOptions::default());

        // 1. Comma-preceded second link in the first 4 KiB chunk (~3 KiB escaped string
        // plus 4 KiB staging vector exceeds a 6 KiB budget).
        {
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 6 << 10;
            let ctx = Arc::new(raw);
            ctx.attach_owner();

            let escaped = format!(r#"\"{}"#, "a".repeat(3_000));
            let payload = format!(
                r#"{{"head":{{"links":["ok","{escaped}"],"vars":["s"]}},"results":{{"bindings":[]}}}}"#
            );
            assert!(payload.len() < READ_STEP);
            let mut cur = io::Cursor::new(payload.as_bytes());
            let mut staging = eager_charge(&ctx).unwrap();
            let err = json_start(&mut cur, &mut staging, &ctx).err().unwrap();
            assert!(
                matches!(
                    crate::codec::io_error(err),
                    Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
                ),
                "comma-preceded escaped string in first chunk must refuse on memory budget"
            );
        }

        // 2. Colon-preceded invalid UTF-8 object value in a single-chunk late-head body.
        {
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 16 << 10;
            let ctx = Arc::new(raw);
            ctx.attach_owner();

            let mut payload =
                br#"{"results":{"bindings":[{"s":{"type":"literal","value":""#.to_vec();
            payload.extend(std::iter::repeat_n(0xFFu8, 3_500));
            payload.extend_from_slice(br#""}}]},"head":{"vars":["s"]}}"#);
            assert!(payload.len() < READ_STEP);
            let mut cur = io::Cursor::new(payload.as_slice());
            let mut staging = eager_charge(&ctx).unwrap();
            let start = json_start(&mut cur, &mut staging, &ctx).unwrap();
            assert!(start.late);
            let err = read_late_json(start, &mut cur, &mut staging, &ctx).unwrap_err();
            assert!(
                matches!(
                    crate::codec::io_error(err),
                    Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
                ),
                "colon-preceded invalid UTF-8 string must refuse on memory budget"
            );
        }

        // 3. 18,000-byte ASCII prefix before an escaped quote allocates 18,000 B then
        // doubles to 36,000 B (54,000 B growth peak + 32,768 B staging = 86,768 B > 84 KiB).
        {
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 84 << 10;
            let ctx = Arc::new(raw);
            ctx.attach_owner();

            let escaped = format!(r#"{}\""#, "a".repeat(18_000));
            let payload = format!(
                r#"{{"head":{{"links":["{escaped}"],"vars":["s"]}},"results":{{"bindings":[]}}}}"#
            );
            assert!(payload.len() < (32 << 10));
            let mut cur = io::Cursor::new(payload.as_bytes());
            let mut staging = eager_charge(&ctx).unwrap();
            let err = json_start(&mut cur, &mut staging, &ctx).err().unwrap();
            assert!(
                matches!(
                    crate::codec::io_error(err),
                    Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
                ),
                "escape after long literal prefix must cover non-power-of-two doubling peak"
            );
        }

        // 4. 22,000 bytes of invalid `\a` escapes expand 2 -> 3 bytes (33,000 B decoded,
        // 96 KiB String growth peak + 32 KiB staging = 128 KiB > 120 KiB).
        {
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 120 << 10;
            let ctx = Arc::new(raw);
            ctx.attach_owner();

            let invalid_escapes = r#"\a"#.repeat(11_000);
            let payload = format!(
                r#"{{"head":{{"links":["{invalid_escapes}"],"vars":["s"]}},"results":{{"bindings":[]}}}}"#
            );
            assert!(payload.len() < (32 << 10));
            let mut cur = io::Cursor::new(payload.as_bytes());
            let mut staging = eager_charge(&ctx).unwrap();
            let err = json_start(&mut cur, &mut staging, &ctx).err().unwrap();
            assert!(
                matches!(
                    crate::codec::io_error(err),
                    Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
                ),
                "invalid ASCII escapes expanding to U+FFFD must refuse on memory budget"
            );
        }
    }

    /// A tiny complete late-head response succeeds under a 128 KiB eager grant instead of
    /// failing before its EOF read, and replay keeps `parser_reserve` live while
    /// `sparesults` owns the buffered solutions iterator.
    #[test]
    fn eager_late_json_accepts_tiny_response_under_128kib_and_holds_replay_credit() {
        let store = Store::in_memory(StoreOptions::default());
        let payload = br#"{"results":{"bindings":[{"s":{"type":"uri","value":"urn:tiny"}}]},"head":{"vars":["s"]}}"#;
        assert!(payload.len() < 100);

        // 1. Succeeds under a 128 KiB eager budget.
        {
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 128 << 10;
            let ctx = Arc::new(raw);
            ctx.attach_owner();

            let mut cur = io::Cursor::new(payload.as_slice());
            let mut staging = eager_charge(&ctx).unwrap();
            let start = json_start(&mut cur, &mut staging, &ctx).unwrap();
            assert!(start.late);
            let bytes = read_late_json(start, &mut cur, &mut staging, &ctx).unwrap();
            assert_eq!(bytes, payload);
        }

        // 2. Under ample grant, `sparesults` switches to its buffered iterator as soon as
        // late `"vars"` closes and stops reading the stream, so both the raw replay buffer
        // and `parser_reserve` remain charged until `sols` drops. (The direct EOF reduction
        // to `parser_reserve` is verified in
        // `charged_cursor_ignores_empty_read_slice_and_keeps_parser_reserve_until_drop`.)
        {
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 4 << 20;
            let ctx = Arc::new(raw);
            ctx.attach_owner();
            let base = ctx.live_bytes();

            let mut cur = io::Cursor::new(payload.as_slice());
            let mut staging = eager_charge(&ctx).unwrap();
            let start = json_start(&mut cur, &mut staging, &ctx).unwrap();
            let bytes = read_late_json(start, &mut cur, &mut staging, &ctx).unwrap();
            let raw_cap = bytes.capacity() as u64;
            let parser_reserve = (bytes.len() as u64)
                .saturating_mul(buffered_factor())
                .saturating_add(SLACK);
            let cursor =
                ChargedCursor::with_parser_reserve(bytes, staging, parser_reserve).unwrap();
            assert_eq!(ctx.live_bytes(), base + raw_cap + parser_reserve);

            let parser =
                sparesults::QueryResultsParser::from_format(sparesults::QueryResultsFormat::Json);
            let sparesults::ReaderQueryResultsParserOutput::Solutions(mut sols) =
                parser.for_reader(cursor).unwrap()
            else {
                panic!("expected solutions");
            };
            // `sparesults` read the tiny late-head JSON in a single read and stopped once
            // `"vars"` closed, so the full combined charge (`raw_cap + parser_reserve`)
            // must remain live until `sols` drops.
            assert_eq!(
                ctx.live_bytes(),
                base + raw_cap + parser_reserve,
                "full combined raw buffer and parser_reserve must remain charged while sparesults owns buffered solutions"
            );
            let row = sols.next().unwrap().unwrap();
            assert_eq!(
                row.get("s"),
                Some(&Term::NamedNode(oxrdf::NamedNode::new("urn:tiny").unwrap()))
            );
            assert!(sols.next().is_none());
            assert_eq!(ctx.live_bytes(), base + raw_cap + parser_reserve);
            drop(sols);
            assert_eq!(ctx.live_bytes(), base);
        }
    }

    /// Leading whitespace compaction preserves a single space before replaying so a
    /// misplaced UTF-8 BOM after spaces is still rejected by `sparesults`, while a valid
    /// UTF-8 BOM at byte zero is accepted both directly and before long whitespace.
    #[test]
    fn json_start_preserves_misplaced_bom_failure_and_accepts_leading_bom() {
        let store = Store::in_memory(StoreOptions::default());
        let mut raw = Ctx::new(store.snapshot());
        raw.mem_limit = 1 << 20;
        let ctx = Arc::new(raw);
        ctx.attach_owner();

        // 1. Misplaced BOM after > 4 KiB of spaces with late head must fail in sparesults.
        let mut misplaced = vec![b' '; 8192];
        misplaced.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        misplaced.extend_from_slice(
            br#"{"results":{"bindings":[{"s":{"type":"uri","value":"urn:bad"}}]},"head":{"vars":["s"]}}"#,
        );
        let mut cur = io::Cursor::new(misplaced.as_slice());
        let mut staging = eager_charge(&ctx).unwrap();
        let start = json_start(&mut cur, &mut staging, &ctx).unwrap();
        assert_eq!(start.bytes[0], b' ');
        let replay = ChargedCursor::new(start.bytes, staging).unwrap().chain(cur);
        let parser =
            sparesults::QueryResultsParser::from_format(sparesults::QueryResultsFormat::Json);
        assert!(
            parser.for_reader(replay).is_err(),
            "misplaced BOM after discarded whitespace must fail parsing"
        );

        // 2. Valid leading BOM followed by > 4 KiB spaces compacts and parses cleanly.
        let mut valid_ws = vec![0xEF, 0xBB, 0xBF];
        valid_ws.extend(std::iter::repeat_n(b' ', 8192));
        valid_ws.extend_from_slice(
            br#"{"head":{"vars":["s"]},"results":{"bindings":[{"s":{"type":"uri","value":"urn:ok"}}]}}"#,
        );
        let mut cur = io::Cursor::new(valid_ws.as_slice());
        let mut staging = eager_charge(&ctx).unwrap();
        let start = json_start(&mut cur, &mut staging, &ctx).unwrap();
        assert!(!start.late);
        assert!(start.bytes.capacity() <= 2 * READ_STEP);
        let replay = ChargedCursor::new(start.bytes, staging).unwrap().chain(cur);
        let parser =
            sparesults::QueryResultsParser::from_format(sparesults::QueryResultsFormat::Json);
        let sparesults::ReaderQueryResultsParserOutput::Solutions(sols) =
            parser.for_reader(replay).unwrap()
        else {
            panic!("expected solutions");
        };
        assert_eq!(sols.count(), 1);

        // 3. Valid leading BOM followed by > 4 KiB spaces and a SECOND misplaced BOM fails.
        let mut double_bom = vec![0xEF, 0xBB, 0xBF];
        double_bom.extend(std::iter::repeat_n(b' ', 8192));
        double_bom.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        double_bom.extend_from_slice(br#"{"head":{"vars":["s"]},"results":{"bindings":[]}}"#);
        let mut cur = io::Cursor::new(double_bom.as_slice());
        let mut staging = eager_charge(&ctx).unwrap();
        let start = json_start(&mut cur, &mut staging, &ctx).unwrap();
        let replay = ChargedCursor::new(start.bytes, staging).unwrap().chain(cur);
        let parser =
            sparesults::QueryResultsParser::from_format(sparesults::QueryResultsFormat::Json);
        assert!(
            parser.for_reader(replay).is_err(),
            "second misplaced BOM after leading BOM + whitespace must fail parsing"
        );

        // 4. Valid leading BOM immediately before `{` parses cleanly.
        let mut direct_bom = vec![0xEF, 0xBB, 0xBF];
        direct_bom.extend_from_slice(
            br#"{"head":{"vars":["s"]},"results":{"bindings":[{"s":{"type":"uri","value":"urn:direct"}}]}}"#,
        );
        let mut cur = io::Cursor::new(direct_bom.as_slice());
        let mut staging = eager_charge(&ctx).unwrap();
        let start = json_start(&mut cur, &mut staging, &ctx).unwrap();
        assert!(!start.late);
        let replay = ChargedCursor::new(start.bytes, staging).unwrap().chain(cur);
        let parser =
            sparesults::QueryResultsParser::from_format(sparesults::QueryResultsFormat::Json);
        let sparesults::ReaderQueryResultsParserOutput::Solutions(sols) =
            parser.for_reader(replay).unwrap()
        else {
            panic!("expected solutions");
        };
        assert_eq!(sols.count(), 1);
    }

    /// Head probing retries immediately once a chunk closes an incomplete > 4 KiB head
    /// token and does not issue another blocking read after `"results"` has arrived.
    #[test]
    fn json_start_does_not_issue_extra_read_after_large_head_token_closes() {
        struct PanicOnThirdRead {
            chunks: VecDeque<Vec<u8>>,
            reads: usize,
        }

        impl Read for PanicOnThirdRead {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.reads += 1;
                let Some(chunk) = self.chunks.pop_front() else {
                    panic!(
                        "json_start issued unexpected blocking read #{} after head completed",
                        self.reads
                    );
                };
                let n = chunk.len().min(buf.len());
                buf[..n].copy_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    self.chunks.push_front(chunk[n..].to_vec());
                }
                Ok(n)
            }
        }

        let store = Store::in_memory(StoreOptions::default());
        let mut raw = Ctx::new(store.snapshot());
        raw.mem_limit = 16 << 20;
        raw.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(raw);
        ctx.attach_owner();

        // Chunk 1 (> 4 KiB) leaves a `head.links` string incomplete; chunk 2 is short,
        // closes the string, finishes `head`, and delivers `"results"` plus the first row.
        let mut chunk1 = br#"{"head":{"links":[""#.to_vec();
        chunk1.extend(std::iter::repeat_n(b'a', 5_000));
        let chunk2 =
            br#"","x"],"vars":["s"]},"results":{"bindings":[{"s":{"type":"uri","value":"urn:1"}}"#
                .to_vec();
        // `json_start` reads in 4096-byte slices, so chunk1 takes 2 reads (4096 + 923)
        // and chunk2 takes 1 read -> 3 reads total, never a 4th blocking read.
        let mut body = PanicOnThirdRead {
            chunks: VecDeque::from([chunk1, chunk2]),
            reads: 0,
        };
        let mut ch = StagingCharge::from_cursor(charge(&ctx).unwrap());
        let start = json_start(&mut body, &mut ch, &ctx).unwrap();
        assert!(!start.late);
        assert_eq!(body.reads, 3);
    }

    /// `ChargedCursor::read` ignores zero-length caller buffers without dropping unread
    /// bytes, and retains `parser_reserve` after EOF until dropped.
    #[test]
    fn charged_cursor_ignores_empty_read_slice_and_keeps_parser_reserve_until_drop() {
        let store = Store::in_memory(StoreOptions::default());
        let mut raw = Ctx::new(store.snapshot());
        raw.mem_limit = 1 << 20;
        let ctx = Arc::new(raw);
        ctx.attach_owner();

        let data = b"hello world".to_vec();
        let cap = data.capacity() as u64;
        let reserve = 4096u64;
        let staging = eager_charge(&ctx).unwrap();
        let base = ctx.live_bytes();
        let mut cursor =
            ChargedCursor::with_parser_reserve(data.clone(), staging, reserve).unwrap();
        assert_eq!(ctx.live_bytes(), base + cap + reserve);

        // An empty slice read must return 0 without freeing the buffer or charge.
        assert_eq!(cursor.read(&mut []).unwrap(), 0);
        assert_eq!(ctx.live_bytes(), base + cap + reserve);

        let mut out = Vec::new();
        cursor.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
        // After draining to EOF, raw buffer capacity is freed while `parser_reserve` stays.
        assert_eq!(ctx.live_bytes(), base + reserve);
        drop(cursor);
        assert_eq!(ctx.live_bytes(), base);
    }

    /// Eager staging credit refuses an oversized non-whitespace head or late-head body
    /// with `BudgetKind::Memory` before allocating past the query budget.
    #[test]
    fn eager_staging_charge_refuses_oversized_head_and_late_body() {
        let store = Store::in_memory(StoreOptions::default());
        let mut raw = Ctx::new(store.snapshot());
        raw.mem_limit = 64 << 10;
        let ctx = Arc::new(raw);
        ctx.attach_owner();

        let big_head = format!(
            r#"{{"head":{{"links":["{}"],"vars":["s"]}},"results":{{"bindings":[]}}}}"#,
            "a".repeat(128 << 10)
        );
        let mut cur = io::Cursor::new(big_head.as_bytes());
        let mut staging = eager_charge(&ctx).unwrap();
        let err = json_start(&mut cur, &mut staging, &ctx).err().unwrap();
        assert!(
            matches!(
                crate::codec::io_error(err),
                Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
            ),
            "oversized non-whitespace head must refuse on memory budget"
        );

        let big_late = format!(
            r#"{{"results":{{"bindings":[{{"s":{{"type":"literal","value":"{}"}}}}]}},"head":{{"vars":["s"]}}}}"#,
            "b".repeat(128 << 10)
        );
        let mut cur = io::Cursor::new(big_late.as_bytes());
        let mut staging = eager_charge(&ctx).unwrap();
        let start = json_start(&mut cur, &mut staging, &ctx).unwrap();
        assert!(start.late);
        let err = read_late_json(start, &mut cur, &mut staging, &ctx).unwrap_err();
        assert!(
            matches!(
                crate::codec::io_error(err),
                Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
            ),
            "oversized late-head body must refuse on memory budget"
        );
    }

    /// Validating a multi-megabyte escaped string token over many 4 KiB reads retries the
    /// parser logarithmically rather than quadratically.
    #[test]
    fn read_late_json_linear_retries_on_large_escaped_token() {
        let store = Store::in_memory(StoreOptions::default());
        let mut raw = Ctx::new(store.snapshot());
        raw.mem_limit = 256 << 20;
        raw.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(raw);
        ctx.attach_owner();

        // ~2.1 MiB escaped literal (> 512 reads of 4 KiB each).
        let piece = r#"a\"b\\c\n"#;
        let escaped = piece.repeat(240_000);
        let payload = format!(
            r#"{{"results":{{"bindings":[{{"s":{{"type":"literal","value":"{escaped}"}}}}]}},"head":{{"vars":["s"]}}}}"#
        );
        assert!(payload.len() > (2 << 20));

        let mut cur = io::Cursor::new(payload.as_bytes());
        let mut ch = StagingCharge::from_cursor(charge(&ctx).unwrap());
        let start = json_start(&mut cur, &mut ch, &ctx).unwrap();
        assert!(start.late);
        let (bytes, incomplete_retries) =
            read_late_json_inner(start, &mut cur, &mut ch, &ctx).unwrap();
        assert_eq!(bytes, payload.as_bytes());
        assert!(
            incomplete_retries <= 20,
            "expected logarithmic retries for 2 MiB token, got {incomplete_retries}"
        );
    }

    /// Head probing avoids quadratic rescanning of long incomplete JSON numbers (integer,
    /// fractional, and exponent digit runs) and malformed alphabetic runs, while still
    /// retrying immediately on the chunk that closes the token.
    #[test]
    fn json_start_avoids_quadratic_rescan_on_long_incomplete_numbers() {
        let store = Store::in_memory(StoreOptions::default());
        let mut raw = Ctx::new(store.snapshot());
        raw.mem_limit = 256 << 20;
        raw.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(raw);
        ctx.attach_owner();

        // 1. 2 MiB integer in a root extension member (> 512 reads of 4 KiB).
        let big_int = "1".repeat(2 << 20);
        let payload =
            format!(r#"{{"ext":{big_int},"head":{{"vars":["s"]}},"results":{{"bindings":[]}}}}"#);
        let mut cur = io::Cursor::new(payload.as_bytes());
        let mut ch = StagingCharge::from_cursor(charge(&ctx).unwrap());
        let (start, retries) = json_start_inner(&mut cur, &mut ch, &ctx).unwrap();
        assert!(!start.late);
        assert!(
            retries <= 2,
            "expected O(1) retries for 2 MiB head number, got {retries}"
        );

        // 2. Long fractional and exponent digit runs in `head` extension members.
        let frac = format!("0.{}", "2".repeat(128 << 10));
        let exp = format!("-1.5e+{}", "3".repeat(128 << 10));
        let payload = format!(
            r#"{{"head":{{"a":{frac},"b":{exp},"vars":["s"]}},"results":{{"bindings":[]}}}}"#
        );
        let mut cur = io::Cursor::new(payload.as_bytes());
        let mut ch = StagingCharge::from_cursor(charge(&ctx).unwrap());
        let (start, retries) = json_start_inner(&mut cur, &mut ch, &ctx).unwrap();
        assert!(!start.late);
        assert!(
            retries <= 4,
            "expected O(1) retries for fractional/exponent numbers, got {retries}"
        );

        // 3. Long malformed alphabetic run starting with `t` avoids quadratic rescan.
        let bad_alpha = "t".repeat(128 << 10);
        let payload = format!(r#"{{"ext":{bad_alpha}}}"#);
        let mut cur = io::Cursor::new(payload.as_bytes());
        let mut ch = StagingCharge::from_cursor(charge(&ctx).unwrap());
        let (_start, retries) = json_start_inner(&mut cur, &mut ch, &ctx).unwrap();
        assert!(
            retries <= 2,
            "expected O(1) retries for malformed alpha run, got {retries}"
        );

        // 4. Chunked > 4 KiB number in `head` retries immediately when its delimiter
        // arrives and does not issue an extra blocking read.
        struct Scripted {
            chunks: VecDeque<Vec<u8>>,
            reads: usize,
        }
        impl Read for Scripted {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.reads += 1;
                let Some(chunk) = self.chunks.pop_front() else {
                    panic!(
                        "json_start issued unexpected blocking read #{} after number closed",
                        self.reads
                    );
                };
                let n = chunk.len().min(buf.len());
                buf[..n].copy_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    self.chunks.push_front(chunk[n..].to_vec());
                }
                Ok(n)
            }
        }
        let mut chunk1 = br#"{"head":{"ext":1"#.to_vec();
        chunk1.extend(std::iter::repeat_n(b'9', 5_000));
        let chunk2 =
            br#","vars":["s"]},"results":{"bindings":[{"s":{"type":"uri","value":"urn:num"}}"#
                .to_vec();
        let mut body = Scripted {
            chunks: VecDeque::from([chunk1, chunk2]),
            reads: 0,
        };
        let mut ch = StagingCharge::from_cursor(charge(&ctx).unwrap());
        let (start, retries) = json_start_inner(&mut body, &mut ch, &ctx).unwrap();
        assert!(!start.late);
        assert_eq!(body.reads, 3);
        assert_eq!(retries, 1);
    }

    /// Eager head probing pre-admits parser-owned error-recovery event copies (missing
    /// `:` or `,` before plain ASCII strings or numbers) and parser nesting `state_stack`
    /// growth before calling `parse_next`.
    #[test]
    fn eager_json_probe_pre_admits_error_event_copies_and_nesting_stack() {
        let store = Store::in_memory(StoreOptions::default());

        // 1. Missing colon before a 3,000-byte plain ASCII string in the first 4 KiB read
        // triggers `owned_event` cloning in `LowLevelJsonParser::buffered_event`
        // (4,096 B staging + 3,000 B copy > 6 KiB).
        {
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 6 << 10;
            let ctx = Arc::new(raw);
            ctx.attach_owner();

            let payload = format!(r#"{{"head":{{"x" "{}"}}}}"#, "a".repeat(3_000));
            assert!(payload.len() < READ_STEP);
            let mut cur = io::Cursor::new(payload.as_bytes());
            let mut staging = eager_charge(&ctx).unwrap();
            let err = json_start(&mut cur, &mut staging, &ctx).err().unwrap();
            assert!(
                matches!(
                    crate::codec::io_error(err),
                    Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
                ),
                "missing-colon plain ASCII string error copy must refuse on memory budget"
            );
        }

        // 2. Missing colon before a 3,000-digit plain number in the first 4 KiB read.
        {
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 6 << 10;
            let ctx = Arc::new(raw);
            ctx.attach_owner();

            let payload = format!(r#"{{"head":{{"x" {}}}}}"#, "9".repeat(3_000));
            assert!(payload.len() < READ_STEP);
            let mut cur = io::Cursor::new(payload.as_bytes());
            let mut staging = eager_charge(&ctx).unwrap();
            let err = json_start(&mut cur, &mut staging, &ctx).err().unwrap();
            assert!(
                matches!(
                    crate::codec::io_error(err),
                    Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
                ),
                "missing-colon plain number error copy must refuse on memory budget"
            );
        }

        // 3. Missing comma inside an array before a 3,000-byte plain ASCII string.
        {
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 6 << 10;
            let ctx = Arc::new(raw);
            ctx.attach_owner();

            let payload = format!(r#"{{"head":{{"links":["ok" "{}"]}}}}"#, "b".repeat(3_000));
            assert!(payload.len() < READ_STEP);
            let mut cur = io::Cursor::new(payload.as_bytes());
            let mut staging = eager_charge(&ctx).unwrap();
            let err = json_start(&mut cur, &mut staging, &ctx).err().unwrap();
            assert!(
                matches!(
                    crate::codec::io_error(err),
                    Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
                ),
                "missing-comma array string error copy must refuse on memory budget"
            );
        }

        // 4. Valid 1,000-level nested array head extension fits in one 4 KiB read and
        // stays within `nesting::MAX_ELEMENTS` (1,024), but `state_stack` growth past
        // 256 states (4,096 B staging + 256 + 512 B stack growth peak = 4,864 B) must
        // refuse under a 4,608-byte grant while succeeding under a 64 KiB grant.
        let nested = format!(
            r#"{{"head":{{"ext":{}0{},"vars":["s"]}},"results":{{"bindings":[]}}}}"#,
            "[".repeat(1_000),
            "]".repeat(1_000),
        );
        assert!(nested.len() < READ_STEP);
        {
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 4_608;
            let ctx = Arc::new(raw);
            ctx.attach_owner();

            let mut cur = io::Cursor::new(nested.as_bytes());
            let mut staging = eager_charge(&ctx).unwrap();
            let err = json_start(&mut cur, &mut staging, &ctx).err().unwrap();
            assert!(
                matches!(
                    crate::codec::io_error(err),
                    Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
                ),
                "1,000 nested arrays must refuse when staging + parser state_stack exceed 4,608 B"
            );
        }
        {
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 64 << 10;
            let ctx = Arc::new(raw);
            ctx.attach_owner();

            let mut cur = io::Cursor::new(nested.as_bytes());
            let mut staging = eager_charge(&ctx).unwrap();
            let start = json_start(&mut cur, &mut staging, &ctx).unwrap();
            assert!(!start.late);
        }
    }

    /// Error-recovery admission covers the diagnostic `String` that coexists with
    /// `buffered_event` at exact byte boundaries, while valid borrowed strings and
    /// numbers (first array value, first object key, and continuations after a consumed
    /// `,` or `:` separator) reserve zero copy space.
    #[test]
    fn eager_json_probe_bounds_recovery_diagnostics_and_skips_valid_borrowed_copies() {
        let store = Store::in_memory(StoreOptions::default());

        struct Chunked(VecDeque<Vec<u8>>);
        impl Read for Chunked {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let Some(chunk) = self.0.pop_front() else {
                    return Ok(0);
                };
                let n = chunk.len().min(buf.len());
                buf[..n].copy_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    self.0.push_front(chunk[n..].to_vec());
                }
                Ok(n)
            }
        }

        // 1. Exact-boundary missing-colon recovery: 4,096 B staging + 8 B stack +
        // 3,000 B cloned value + 27 B diagnostic = 7,131 B. Must refuse at 7,104 B and
        // 7,130 B, and admit at 7,131 B (for both plain ASCII string and plain number).
        let colon_str = format!(r#"{{"head":{{"x" "{}"}}}}"#, "a".repeat(3_000));
        let colon_num = format!(r#"{{"head":{{"x" {}}}}}"#, "9".repeat(3_000));
        assert!(colon_str.len() < READ_STEP && colon_num.len() < READ_STEP);
        for payload in [colon_str.as_bytes(), colon_num.as_bytes()] {
            for limit in [7_104u64, 7_130u64] {
                let mut raw = Ctx::new(store.snapshot());
                raw.mem_limit = limit;
                let ctx = Arc::new(raw);
                ctx.attach_owner();
                let mut cur = io::Cursor::new(payload);
                let mut staging = eager_charge(&ctx).unwrap();
                let err = json_start(&mut cur, &mut staging, &ctx).err().unwrap();
                assert!(
                    matches!(
                        crate::codec::io_error(err),
                        Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
                    ),
                    "missing-colon recovery must refuse under {limit}-byte grant"
                );
            }
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 7_131;
            let ctx = Arc::new(raw);
            ctx.attach_owner();
            let mut cur = io::Cursor::new(payload);
            let mut staging = eager_charge(&ctx).unwrap();
            let start = json_start(&mut cur, &mut staging, &ctx).unwrap();
            assert!(!start.late);
        }

        // 2. Exact-boundary missing-comma array recovery: 4,096 B staging + 8 B stack +
        // 3,000 B cloned string + 97 B diagnostic = 7,201 B. Must refuse at 7,104 B and
        // 7,200 B, and admit at 7,201 B.
        let comma_str = format!(r#"{{"head":{{"links":["ok" "{}"]}}}}"#, "b".repeat(3_000));
        assert!(comma_str.len() < READ_STEP);
        for limit in [7_104u64, 7_200u64] {
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = limit;
            let ctx = Arc::new(raw);
            ctx.attach_owner();
            let mut cur = io::Cursor::new(comma_str.as_bytes());
            let mut staging = eager_charge(&ctx).unwrap();
            let err = json_start(&mut cur, &mut staging, &ctx).err().unwrap();
            assert!(
                matches!(
                    crate::codec::io_error(err),
                    Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
                ),
                "missing-comma array recovery must refuse under {limit}-byte grant"
            );
        }
        {
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 7_201;
            let ctx = Arc::new(raw);
            ctx.attach_owner();
            let mut cur = io::Cursor::new(comma_str.as_bytes());
            let mut staging = eager_charge(&ctx).unwrap();
            let start = json_start(&mut cur, &mut staging, &ctx).unwrap();
            assert!(!start.late);
        }

        // 3. Valid 3,000-byte plain ASCII first array value (`ArrayValueOrEnd`) and first
        // object key (`ObjectKeyOrEnd`) emit borrowed events without copying and finish
        // the probe under a 6 KiB (and even exact 4,104 B) staging grant.
        let first_link = format!(
            r#"{{"head":{{"links":["{}"],"vars":["s"]}},"results":{{"bindings":[]}}}}"#,
            "a".repeat(3_000)
        );
        let first_key = format!(
            r#"{{"head":{{"{}":1,"vars":["s"]}},"results":{{"bindings":[]}}}}"#,
            "k".repeat(3_000)
        );
        assert!(first_link.len() < READ_STEP && first_key.len() < READ_STEP);
        for payload in [first_link.as_bytes(), first_key.as_bytes()] {
            for limit in [4_104u64, 6 << 10] {
                let mut raw = Ctx::new(store.snapshot());
                raw.mem_limit = limit;
                let ctx = Arc::new(raw);
                ctx.attach_owner();
                let mut cur = io::Cursor::new(payload);
                let mut staging = eager_charge(&ctx).unwrap();
                let start = json_start(&mut cur, &mut staging, &ctx).unwrap();
                assert!(!start.late);
            }
        }

        // 4. Valid continuations after a consumed `,` or `:` separator in an earlier
        // read (`ArrayCommaOrEnd -> ArrayValue`, `ObjectColon -> ObjectValue`) reserve
        // zero copy space for 9,000-byte plain borrowed strings and numbers (staging
        // grows 4,096 -> 8,192 -> 16,384 with peak 24,584 B, fitting under 25,000 B,
        // whereas a false 9,000 B copy charge would demand 16,384 + 8 + 9,000 = 25,392 B).
        let continuation_cases = [
            (
                br#"{"head":{"links":["ok","#.to_vec(),
                format!(
                    r#""{}"],"vars":["s"]}},"results":{{"bindings":[]}}}}"#,
                    "c".repeat(9_000)
                )
                .into_bytes(),
            ),
            (
                br#"{"head":{"ext":"#.to_vec(),
                format!(
                    r#""{}","vars":["s"]}},"results":{{"bindings":[]}}}}"#,
                    "d".repeat(9_000)
                )
                .into_bytes(),
            ),
            (
                br#"{"head":{"ext":"#.to_vec(),
                format!(
                    r#"{},"vars":["s"]}},"results":{{"bindings":[]}}}}"#,
                    "7".repeat(9_000)
                )
                .into_bytes(),
            ),
        ];
        for (c1, c2) in continuation_cases {
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 25_000;
            let ctx = Arc::new(raw);
            ctx.attach_owner();
            let mut body = Chunked(VecDeque::from([c1, c2]));
            let mut staging = eager_charge(&ctx).unwrap();
            let start = json_start(&mut body, &mut staging, &ctx).unwrap();
            assert!(!start.late);
        }

        // 5. Contrast: when chunk 1 ends right after an object key WITHOUT `:`, chunk 2's
        // 9,000-byte plain ASCII string still triggers missing-colon recovery and refuses
        // under 25,000 B (16,384 + 8 + 9,000 + 27 = 25,419 > 25,000).
        {
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 25_000;
            let ctx = Arc::new(raw);
            ctx.attach_owner();
            let mut body = Chunked(VecDeque::from([
                br#"{"head":{"ext""#.to_vec(),
                format!(r#" "{}"}}}}"#, "e".repeat(9_000)).into_bytes(),
            ]));
            let mut staging = eager_charge(&ctx).unwrap();
            let err = json_start(&mut body, &mut staging, &ctx).err().unwrap();
            assert!(
                matches!(
                    crate::codec::io_error(err),
                    Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
                ),
                "chunk-split missing-colon recovery must still refuse under 25,000 B"
            );
        }
    }

    /// Head probing matches `JsonLexer::read_string`'s 6-byte surrogate consumption so a
    /// malformed surrogate escape like `\ud800"qwert` followed by a long ASCII run does
    /// not trigger quadratic per-chunk retries, while valid chunked surrogate pairs still
    /// resume immediately when the string closes.
    #[test]
    fn json_start_avoids_quadratic_rescan_on_malformed_surrogate_escape() {
        let store = Store::in_memory(StoreOptions::default());
        let mut raw = Ctx::new(store.snapshot());
        raw.mem_limit = 256 << 20;
        raw.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(raw);
        ctx.attach_owner();

        // 1. Contiguous 128 KiB string after malformed `\ud800"qwert` (> 32 reads of 4 KiB).
        let tail = "a".repeat(128 << 10);
        let payload = format!(r#"{{"head":{{"links":["\ud800"qwert{tail}"]}}}}"#);
        let mut cur = io::Cursor::new(payload.as_bytes());
        let mut ch = StagingCharge::from_cursor(charge(&ctx).unwrap());
        let (_start, retries) = json_start_inner(&mut cur, &mut ch, &ctx).unwrap();
        assert!(
            retries <= 2,
            "expected O(1) retries for malformed surrogate escape, got {retries}"
        );

        // 2. Chunk boundaries splitting `\uD8|00"qw|ert` + 128 KiB ASCII tail.
        struct Chunked {
            chunks: VecDeque<Vec<u8>>,
            reads: usize,
        }
        impl Read for Chunked {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.reads += 1;
                let Some(chunk) = self.chunks.pop_front() else {
                    return Ok(0);
                };
                let n = chunk.len().min(buf.len());
                buf[..n].copy_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    self.chunks.push_front(chunk[n..].to_vec());
                }
                Ok(n)
            }
        }
        let mut body = Chunked {
            chunks: VecDeque::from([
                br#"{"head":{"links":["\uD8"#.to_vec(),
                br#"00"qw"#.to_vec(),
                format!("ert{tail}\"]}}}}").into_bytes(),
            ]),
            reads: 0,
        };
        let mut ch = StagingCharge::from_cursor(charge(&ctx).unwrap());
        let (_start, retries) = json_start_inner(&mut body, &mut ch, &ctx).unwrap();
        assert!(
            retries <= 2,
            "expected O(1) retries for chunk-split malformed surrogate escape, got {retries}"
        );

        // 3. Valid surrogate pair split across chunks closes promptly without an extra
        // blocking read after `"results"` arrives.
        struct PanicOnExtra {
            chunks: VecDeque<Vec<u8>>,
            reads: usize,
        }
        impl Read for PanicOnExtra {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.reads += 1;
                let Some(chunk) = self.chunks.pop_front() else {
                    panic!(
                        "unexpected blocking read #{} after surrogate string closed",
                        self.reads
                    );
                };
                let n = chunk.len().min(buf.len());
                buf[..n].copy_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    self.chunks.push_front(chunk[n..].to_vec());
                }
                Ok(n)
            }
        }
        let mut c1 = br#"{"head":{"links":[""#.to_vec();
        c1.extend(std::iter::repeat_n(b'a', 5_000));
        c1.extend_from_slice(br"\uD83D\uDE");
        let c2 =
            br#"00"],"vars":["s"]},"results":{"bindings":[{"s":{"type":"uri","value":"urn:ok"}}"#
                .to_vec();
        let mut body = PanicOnExtra {
            chunks: VecDeque::from([c1, c2]),
            reads: 0,
        };
        let mut ch = StagingCharge::from_cursor(charge(&ctx).unwrap());
        let (start, retries) = json_start_inner(&mut body, &mut ch, &ctx).unwrap();
        assert!(!start.late);
        assert_eq!(body.reads, 3);
        assert_eq!(retries, 1);
    }
}
