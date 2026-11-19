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
use crate::sparql::ctx::{Ctx, OwnedCharge};
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
const SLACK: u64 = 4 * 4096 + 8192;

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
}

/// Read the start of a JSON response up to its top-level `results` or `boolean` member,
/// charging `charge` [`head_factor`] times what it holds before it reads more. A
/// response that is not valid JSON or ends first is left for sparesults to refuse, as
/// one whose head comes first.
pub(super) fn json_start(body: &mut dyn Read, charge: &mut OwnedCharge) -> io::Result<Start> {
    let mut parser = LowLevelJsonParser::new();
    let mut bytes = Vec::new();
    let (mut at, mut ending) = (0usize, false);
    let (mut depth, mut in_head, mut vars) = (0usize, false, false);
    loop {
        let result = parser.parse_next(&bytes[at..], ending);
        at += result.consumed_bytes;
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
            return Ok(Start {
                bytes,
                decided: at,
                late,
            });
        }
        let start = bytes.len();
        let target = (start as u64 + 4096)
            .saturating_mul(head_factor())
            .saturating_add(SLACK);
        charge.resize(target).map_err(io::Error::other)?;
        bytes.resize(start + 4096, 0);
        let n = body.read(&mut bytes[start..])?;
        bytes.truncate(start + n);
        ending = n == 0;
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

/// A reader that charges what the parser above it may hold, before it hands it bytes.
pub(super) struct Metered<R> {
    inner: R,
    charge: OwnedCharge,
    /// the charge for the head, and for a buffered answer
    base: u64,
    /// Whether the parser buffers the answer before its first solution, so that each of
    /// those bytes is charged [`buffered_factor`] times.
    late: bool,
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
            window,
            high: 0,
        }
    }
}

impl<R: Read> Read for Metered<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
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

    #[test]
    fn the_factors_cover_their_worst_cases() {
        // an empty binding of a buffered answer keeps at least a pair of vectors
        assert!(buffered_factor() * 3 >= 2 * size_of::<(Vec<String>, Vec<Term>)>() as u64);
        assert!(head_factor() * 4 >= 2 * size_of::<Variable>() as u64);
    }
}
