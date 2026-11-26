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

/// Conservative peak heap scratch `LowLevelJsonParser::parse_next` may allocate while
/// decoding the next token in `slice` (an owned `String` when a JSON string contains an
/// escape sequence or invalid UTF-8 bytes).
fn lexer_token_scratch(mut slice: &[u8], allow_bom: bool) -> u64 {
    if allow_bom && slice.starts_with(&[0xEF, 0xBB, 0xBF]) {
        slice = &slice[3..];
    }
    slice = skip_json_ws(slice);
    // `LowLevelJsonParser::parse_next` loops past a single `:` or `,` separator and
    // decodes the following value or key in the same call.
    if matches!(slice.first(), Some(b':' | b',')) {
        slice = skip_json_ws(&slice[1..]);
    }
    if slice.first() != Some(&b'"') {
        return 0;
    }
    let mut has_escape = false;
    let mut has_non_ascii = false;
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
                        // Surrogate code unit: `read_string` consumes 6 more bytes and
                        // pushes at most one 4-byte UTF-8 character.
                        if i + 6 > slice.len() {
                            break;
                        }
                        i += 6;
                        decoded = decoded.saturating_add(4);
                    } else {
                        // Single BMP scalar or invalid hex -> `U+FFFD` (at most 3 bytes).
                        decoded = decoded.saturating_add(3);
                    }
                }
                _ => {
                    // Unrecognized 2-byte escape pushes `U+FFFD` (3 UTF-8 bytes).
                    decoded = decoded.saturating_add(3);
                    i += 1;
                }
            }
        } else if b == b'"' {
            break;
        } else if !b.is_ascii() {
            has_non_ascii = true;
            decoded = decoded.saturating_add(3);
            i += 1;
        } else {
            decoded = decoded.saturating_add(1);
            i += 1;
        }
    }
    if !has_escape && !has_non_ascii {
        return 0;
    }
    // `String::new(); string.push_str(prefix)` allocates `prefix.len()` without rounding
    // to a power of two; a subsequent push can double `old_cap` to `2 * old_cap`, giving
    // an old + new reallocation peak bounded by `3 * decoded + 16`. When `has_escape`
    // and `has_non_ascii` both hold, an owned `from_utf8_lossy` chunk buffer can coexist
    // with `string` during `push_str`, bounded by `4 * decoded + 24`.
    let decoded = decoded as u64;
    if has_escape && has_non_ascii {
        decoded.saturating_mul(4).saturating_add(24)
    } else {
        decoded.saturating_mul(3).saturating_add(16)
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
    String { escaped: bool, skip: u8 },
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
                let mut token = Self::String {
                    escaped: false,
                    skip: 0,
                };
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
            Self::String { escaped, skip } => {
                for &b in bytes {
                    if *skip > 0 {
                        *skip -= 1;
                    } else if *escaped {
                        *escaped = false;
                        if b == b'u' {
                            *skip = 4;
                        }
                    } else if b == b'\\' {
                        *escaped = true;
                    } else if b == b'"' {
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
    depth: usize,
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
    let mut bytes = Vec::new();
    let (mut at, mut ending) = (0usize, false);
    let (mut depth, mut in_head, mut vars, mut seen_token) = (0usize, false, false, false);
    let mut incomplete_retries = 0usize;
    loop {
        let prev_at = at;
        if at < bytes.len() {
            let scratch = lexer_token_scratch(&bytes[at..], !seen_token);
            if scratch > 0 {
                let parse_peak = (bytes.capacity() as u64).saturating_add(scratch);
                if parse_peak > charge.bytes() {
                    charge.resize(parse_peak).map_err(io::Error::other)?;
                }
            }
        }
        let result = parser.parse_next(&bytes[at..], ending);
        at += result.consumed_bytes;
        if result.event.is_some() {
            seen_token = true;
        }
        // `Some(late)` once the order is known
        let decided = match result.event {
            None if ending => Some(false),
            None => None,
            Some(Err(_)) | Some(Ok(JsonEvent::Eof)) => Some(false),
            Some(Ok(event)) => match event {
                JsonEvent::StartObject | JsonEvent::StartArray => {
                    depth += 1;
                    continue;
                }
                JsonEvent::EndObject | JsonEvent::EndArray => {
                    if depth == 2 && in_head && event == JsonEvent::EndObject {
                        in_head = false;
                    }
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        Some(false)
                    } else {
                        continue;
                    }
                }
                JsonEvent::ObjectKey(key) if depth == 1 => match key.as_ref() {
                    "results" => Some(!vars),
                    "boolean" => Some(false),
                    other => {
                        in_head = other == "head";
                        continue;
                    }
                },
                JsonEvent::ObjectKey(key) if depth == 2 && in_head && key == "vars" => {
                    vars = true;
                    continue;
                }
                _ => continue,
            },
        };
        if let Some(late) = decided {
            return Ok((
                Start {
                    bytes,
                    decided: at,
                    late,
                    parser,
                    ending,
                    depth,
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
        let mut incomplete = IncompleteToken::from_slice(&bytes[at..], !seen_token);
        loop {
            ctx.check().map_err(io::Error::other)?;
            let start = bytes.len();
            let next_len = start
                .checked_add(READ_STEP)
                .ok_or_else(|| io::Error::new(io::ErrorKind::OutOfMemory, "JSON head too large"))?;
            let peak = vec_growth_peak(bytes.capacity(), next_len);
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
        mut depth,
        mut in_head,
        ..
    } = start;
    let mut expect_links_array = false;
    let mut in_links = false;
    let mut links_depth = 0usize;
    let mut incomplete_retries = 0usize;
    loop {
        if at < bytes.len() {
            let scratch = lexer_token_scratch(&bytes[at..], false);
            if scratch > 0 {
                let parse_peak = (bytes.capacity() as u64).saturating_add(scratch);
                if parse_peak > charge.bytes() {
                    charge.resize(parse_peak).map_err(io::Error::other)?;
                }
            }
        }
        let result = parser.parse_next(&bytes[at..], ending);
        at += result.consumed_bytes;
        match result.event {
            None if ending => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected end of JSON response",
                ));
            }
            None => {}
            Some(Err(e)) => {
                return Err(io::Error::new(io::ErrorKind::InvalidData, e));
            }
            Some(Ok(JsonEvent::Eof)) => {
                if depth != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unexpected end of JSON response",
                    ));
                }
                let target = (bytes.capacity() as u64)
                    .saturating_add((bytes.len() as u64).saturating_mul(buffered_factor()))
                    .saturating_add(SLACK);
                if target != charge.bytes() {
                    charge.resize(target).map_err(io::Error::other)?;
                }
                return Ok((bytes, incomplete_retries));
            }
            Some(Ok(event)) => {
                if expect_links_array {
                    expect_links_array = false;
                    if event != JsonEvent::StartArray {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "SPARQL JSON results links must be an array",
                        ));
                    }
                    depth += 1;
                    in_links = true;
                    links_depth = depth;
                    continue;
                }
                if in_links && depth == links_depth {
                    match event {
                        JsonEvent::String(_) => continue,
                        JsonEvent::EndArray => {
                            in_links = false;
                            depth = depth.saturating_sub(1);
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
                    JsonEvent::StartObject | JsonEvent::StartArray => {
                        depth += 1;
                    }
                    JsonEvent::EndObject | JsonEvent::EndArray => {
                        if depth == 2 && in_head && event == JsonEvent::EndObject {
                            in_head = false;
                        }
                        depth = depth.saturating_sub(1);
                    }
                    JsonEvent::ObjectKey(key) if depth == 1 => {
                        in_head = key.as_ref() == "head";
                    }
                    JsonEvent::ObjectKey(key) if depth == 2 && in_head && key == "links" => {
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
            // Pre-admit raw staging growth before `body.read`; size the buffered replay
            // reserve against bytes actually received so a tiny complete response is not
            // rejected before the read can observe EOF.
            let vec_peak = vec_growth_peak(cap, next_len);
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
                    .saturating_add(SLACK);
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

    /// `lexer_token_scratch` pre-admits strings immediately following `,` or `:`,
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

        // 2. Under ample grant, `ChargedCursor` releases raw capacity on EOF while
        // keeping `parser_reserve` live until `sparesults`'s buffered iterator drops.
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
            // `sparesults` read the late-head JSON through `"vars"` and buffered the
            // solution; raw staging buffer is released once EOF is observed, while
            // `parser_reserve` remains live as long as `sols` owns the buffered answers.
            assert!(
                ctx.live_bytes() >= base + parser_reserve,
                "parser_reserve must remain charged while sparesults owns buffered solutions"
            );
            let row = sols.next().unwrap().unwrap();
            assert_eq!(
                row.get("s"),
                Some(&Term::NamedNode(oxrdf::NamedNode::new("urn:tiny").unwrap()))
            );
            assert!(sols.next().is_none());
            assert!(ctx.live_bytes() >= base + parser_reserve);
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
}
