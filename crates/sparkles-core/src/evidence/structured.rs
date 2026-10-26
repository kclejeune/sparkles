//! One explicitly versioned selected-value encoding, without a floating-point tree.

use super::{RecordKey, Sha256Digest};
use crate::{Budget, BudgetKind, Error, Result, nesting, task::Control};
use serde::Deserialize;
use std::{borrow::Cow, collections::BTreeSet, io::Read};

/// The selected JSON value's exact retained UTF-8 bytes, without outer whitespace.
/// Object order, number spelling and string escapes are preserved. This is not a
/// universal semantic canonicalization, and it makes no authenticity claim.
pub const JSON_LEXICAL_UTF8_V1: &str = "urn:x-sparkles:evidence:normalization:json-lexical-utf8:1";

/// The encoding is identified by both its immutable method IRI and this version.
pub const JSON_LEXICAL_UTF8_VERSION: &str = "1";

const MEMORY_LIMIT: usize = 64 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct SelectedJsonValue<'a> {
    pub utf8: &'a str,
    pub digest: Sha256Digest,
}

/// Resolve a strict RFC 6901 pointer without reserializing JSON or rounding numbers.
/// Duplicate decoded object keys anywhere in the revision are refused, including
/// in a subtree outside the selected value. All strings must contain Unicode scalar
/// values. There are no URI-fragment pointers or append-only array selectors here.
pub fn select_json_lexical_utf8<'a>(
    source: &'a str,
    pointer: &str,
    control: &Control,
) -> Result<SelectedJsonValue<'a>> {
    control.check()?;
    input_bound(source.len())?;
    validate_pointer(pointer, control)?;
    let mut memory = Memory::new(source.len().saturating_add(pointer.len()))?;
    let tokens = pointer_tokens(pointer, &mut memory, control)?;
    validate_json(source, control)?;
    let bytes = source.as_bytes();
    let mut position = whitespace(bytes, 0, control)?;
    let mut matched = Some(0);
    let mut stack: Vec<Frame> = Vec::new();
    let mut selected = None;
    loop {
        control.check()?;
        let start = position;
        match bytes[position] {
            b'{' | b'[' => {
                memory.add(std::mem::size_of::<Frame>().saturating_mul(2))?;
                stack.push(Frame {
                    start,
                    matched,
                    first: true,
                    kind: if bytes[position] == b'{' {
                        Children::Object {
                            keys: BTreeSet::new(),
                            charged: 0,
                        }
                    } else {
                        Children::Array { next: 0 }
                    },
                });
                position += 1;
            }
            b'"' => {
                position = string_end(bytes, position, control)?;
                // IgnoredAny validates JSON syntax without converting numbers. Its
                // ignored strings do not require paired UTF-16 escapes, so validate
                // escaped string values separately using the string deserializer.
                let raw = &source[start..position];
                if raw.as_bytes().contains(&b'\\') {
                    let decoded = decode_string(raw, &mut memory, control)?;
                    memory.remove(decoded.capacity().saturating_add(96));
                }
                select(start, position, matched, tokens.len(), &mut selected);
            }
            _ => {
                while position < bytes.len()
                    && !matches!(
                        bytes[position],
                        b',' | b']' | b'}' | b' ' | b'\n' | b'\r' | b'\t'
                    )
                {
                    if position.is_multiple_of(CHUNK) {
                        control.check()?;
                    }
                    position += 1;
                }
                select(start, position, matched, tokens.len(), &mut selected);
            }
        }
        loop {
            control.check()?;
            position = whitespace(bytes, position, control)?;
            let Some(frame) = stack.last_mut() else {
                let (start, end) = selected.ok_or_else(|| {
                    Error::invalid("span-mismatch: JSON Pointer does not select a retained value")
                })?;
                let utf8 = &source[start..end];
                return Ok(SelectedJsonValue {
                    utf8,
                    digest: Sha256Digest::of_bytes(utf8.as_bytes(), control)?,
                });
            };
            let closing = match frame.kind {
                Children::Object { .. } => b'}',
                Children::Array { .. } => b']',
            };
            if bytes[position] == closing {
                position += 1;
                let frame = stack.pop().expect("last frame exists");
                select(
                    frame.start,
                    position,
                    frame.matched,
                    tokens.len(),
                    &mut selected,
                );
                if let Children::Object { charged, .. } = frame.kind {
                    memory.remove(charged);
                }
                memory.remove(std::mem::size_of::<Frame>().saturating_mul(2));
                continue;
            }
            if !frame.first {
                // The grammar was validated before traversal, so only a comma can
                // appear between children of an open container.
                position = whitespace(bytes, position + 1, control)?;
            }
            frame.first = false;
            matched = match &mut frame.kind {
                Children::Object { keys, charged } => {
                    let end = string_end(bytes, position, control)?;
                    let key = decode_string(&source[position..end], &mut memory, control)?;
                    let key_charge = key.capacity().saturating_add(96);
                    let child = frame.matched.and_then(|depth| {
                        (tokens.get(depth).map(Cow::as_ref) == Some(key.as_str()))
                            .then_some(depth + 1)
                    });
                    if !keys.insert(key) {
                        return Err(Error::invalid(
                            "span-mismatch: duplicate decoded JSON object key",
                        ));
                    }
                    *charged += key_charge;
                    position = whitespace(bytes, end, control)?;
                    position = whitespace(bytes, position + 1, control)?; // colon
                    child
                }
                Children::Array { next } => {
                    let child = frame.matched.and_then(|depth| {
                        (tokens.get(depth).map(Cow::as_ref) == Some(next.to_string().as_str()))
                            .then_some(depth + 1)
                    });
                    *next += 1;
                    child
                }
            };
            break;
        }
    }
}

pub(super) fn validate_pointer(pointer: &str, control: &Control) -> Result<()> {
    control.check()?;
    input_bound(pointer.len())?;
    if !pointer.is_empty() && !pointer.starts_with('/') {
        return Err(Error::invalid("JSON Pointer must be empty or start with /"));
    }
    let mut bytes = pointer.bytes().enumerate();
    while let Some((index, byte)) = bytes.next() {
        if index.is_multiple_of(CHUNK) {
            control.check()?;
        }
        if byte == b'~' && !matches!(bytes.next(), Some((_, b'0' | b'1'))) {
            return Err(Error::invalid(
                "JSON Pointer requires RFC 6901 ~0/~1 escapes",
            ));
        }
    }
    Ok(())
}

fn pointer_tokens<'a>(
    pointer: &'a str,
    memory: &mut Memory,
    control: &Control,
) -> Result<Vec<Cow<'a, str>>> {
    let mut tokens = Vec::new();
    if let Some(pointer) = pointer.strip_prefix('/') {
        for raw in pointer.split('/') {
            control.check()?;
            // Covers geometric Vec growth, and a decoded token when needed.
            memory.add(
                std::mem::size_of::<Cow<'_, str>>()
                    .saturating_mul(2)
                    .saturating_add(raw.len().saturating_mul(2)),
            )?;
            let token = if raw.contains('~') {
                Cow::Owned(raw.replace("~1", "/").replace("~0", "~"))
            } else {
                Cow::Borrowed(raw)
            };
            tokens.push(token);
        }
    }
    Ok(tokens)
}

fn input_bound(len: usize) -> Result<()> {
    if len > RecordKey::MAX_BYTES {
        return Err(Error::BudgetExceeded(Budget {
            kind: BudgetKind::DecompressedBytes,
            limit: RecordKey::MAX_BYTES as u64,
            requested: len as u64,
        }));
    }
    Ok(())
}

fn select(
    start: usize,
    end: usize,
    matched: Option<usize>,
    token_count: usize,
    selected: &mut Option<(usize, usize)>,
) {
    if matched == Some(token_count) {
        *selected = Some((start, end));
    }
}

struct Frame {
    start: usize,
    matched: Option<usize>,
    first: bool,
    kind: Children,
}

enum Children {
    Object {
        keys: BTreeSet<String>,
        charged: usize,
    },
    Array {
        next: usize,
    },
}

fn whitespace(bytes: &[u8], mut position: usize, control: &Control) -> Result<usize> {
    while position < bytes.len() && matches!(bytes[position], b' ' | b'\n' | b'\r' | b'\t') {
        if position.is_multiple_of(CHUNK) {
            control.check()?;
        }
        position += 1;
    }
    Ok(position)
}

fn string_end(bytes: &[u8], start: usize, control: &Control) -> Result<usize> {
    let mut position = start + 1;
    while position < bytes.len() {
        if position.is_multiple_of(CHUNK) {
            control.check()?;
        }
        match bytes[position] {
            b'"' => return Ok(position + 1),
            b'\\' => position += 2,
            _ => position += 1,
        }
    }
    Err(Error::invalid("invalid JSON string"))
}

fn decode_string(raw: &str, memory: &mut Memory, control: &Control) -> Result<String> {
    control.check()?;
    // String parsing may temporarily grow a decode scratch allocation. Reserve
    // before allocating, then retain only the actual string and tree-node bound.
    let temporary = raw.len().saturating_mul(4).saturating_add(96);
    memory.add(temporary)?;
    let decoded: String = serde_json::from_str(raw)
        .map_err(|_| Error::invalid("JSON strings require valid Unicode scalar values"))?;
    let retained = decoded.capacity().saturating_add(96);
    memory.remove(temporary);
    memory.add(retained)?;
    control.check()?;
    Ok(decoded)
}

struct Memory(usize);
impl Memory {
    fn new(bytes: usize) -> Result<Self> {
        let mut memory = Self(0);
        memory.add(bytes)?;
        Ok(memory)
    }
    fn add(&mut self, bytes: usize) -> Result<()> {
        let requested = self.0.saturating_add(bytes);
        if requested > MEMORY_LIMIT {
            return Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::Memory,
                limit: MEMORY_LIMIT as u64,
                requested: requested as u64,
            }));
        }
        self.0 = requested;
        Ok(())
    }
    fn remove(&mut self, bytes: usize) {
        self.0 = self.0.saturating_sub(bytes);
    }
}

fn validate_json(source: &str, control: &Control) -> Result<()> {
    let mut nesting = nesting::Scanner::new(nesting::Syntax::Json, nesting::MAX_ELEMENTS);
    for chunk in source.as_bytes().chunks(CHUNK) {
        control.check()?;
        nesting
            .feed(chunk)
            .map_err(|_| Error::invalid("JSON reference exceeds the nesting bound"))?;
    }
    let mut reader = ControlledRead {
        bytes: source.as_bytes(),
        until_check: 0,
        control,
        failure: None,
    };
    let parsed = {
        let mut parser = serde_json::Deserializer::from_reader(&mut reader);
        serde::de::IgnoredAny::deserialize(&mut parser).and_then(|_| parser.end())
    };
    if let Some(error) = reader.failure {
        return Err(error);
    }
    parsed.map_err(|_| Error::invalid("invalid retained JSON revision"))?;
    control.check()
}

struct ControlledRead<'a> {
    bytes: &'a [u8],
    until_check: usize,
    control: &'a Control,
    failure: Option<Error>,
}
impl Read for ControlledRead<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.until_check == 0 {
            if let Err(error) = self.control.check() {
                self.failure = Some(error);
                return Err(std::io::Error::other("JSON reference interrupted"));
            }
            self.until_check = CHUNK;
        }
        let len = buffer.len().min(self.bytes.len()).min(self.until_check);
        buffer[..len].copy_from_slice(&self.bytes[..len]);
        self.bytes = &self.bytes[len..];
        self.until_check -= len;
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected<'a>(source: &'a str, pointer: &str) -> SelectedJsonValue<'a> {
        select_json_lexical_utf8(source, pointer, &Control::none()).unwrap()
    }

    #[test]
    fn selection_preserves_all_json_types_lexical_bytes_and_pointer_escapes() {
        let source = r#" {"a/b":{"m~n":[null,true,false,-0,1.00000000000000000000000001,1e999999,"\u0041",{"": "Café"}]},"01":"object key","~1":"escaped once"} "#;
        for (pointer, expected) in [
            ("/a~1b/m~0n/0", "null"),
            ("/a~1b/m~0n/1", "true"),
            ("/a~1b/m~0n/2", "false"),
            ("/a~1b/m~0n/3", "-0"),
            ("/a~1b/m~0n/4", "1.00000000000000000000000001"),
            ("/a~1b/m~0n/5", "1e999999"),
            ("/a~1b/m~0n/6", r#""\u0041""#),
            ("/a~1b/m~0n/7/", "\"Café\""),
            ("/01", "\"object key\""),
            ("/~01", "\"escaped once\""),
        ] {
            let value = selected(source, pointer);
            assert_eq!(value.utf8, expected, "{pointer}");
            assert_eq!(
                value.digest,
                Sha256Digest::of_bytes(expected.as_bytes(), &Control::none()).unwrap()
            );
        }
        assert_eq!(selected(source, "").utf8, source.trim());
        assert_eq!(selected(r#" { "x" : [ 1, 2 ] } "#, "/x").utf8, "[ 1, 2 ]");
        for source in ["[]", "{}", "null", "true", "1", r#""value""#] {
            assert_eq!(selected(source, "").utf8, source);
        }
        // Independent hashlib SHA-256 golden of the exact selected ASCII bytes.
        assert_eq!(
            selected("[true]", "/0").digest.to_string(),
            "sha256:b5bea41b6c623f7c09f1bf24dcae58ebab3c0cdd90ad966bc43a45b44867e12b"
        );
    }

    #[test]
    fn lexical_normalization_does_not_collapse_distinct_spellings_or_unicode() {
        for (first, second) in [
            ("1", "1.0"),
            (r#""A""#, r#""\u0041""#),
            ("\"é\"", "\"e\u{301}\""),
        ] {
            assert_ne!(selected(first, "").digest, selected(second, "").digest);
        }
        let huge = format!("[{}]", "9".repeat(5000));
        assert_eq!(selected(&huge, "/0").utf8.len(), 5000);
        let magic = r#"{"$serde_json::private::RawValue":"ordinary data","$serde_json::private::Number":"also data"}"#;
        assert_eq!(
            selected(magic, "/$serde_json::private::RawValue").utf8,
            "\"ordinary data\""
        );
    }

    #[test]
    fn ambiguous_or_missing_selectors_and_invalid_json_are_refused() {
        for (source, pointer) in [
            (r#"{"x":1,"x":2}"#, "/x"),
            (r#"{"x":1,"\u0078":2}"#, "/x"),
            (r#"{"selected":1,"other":{"x":1,"x":2}}"#, "/selected"),
            ("[1]", "/01"),
            ("[1]", "/-"),
            ("[1]", "/1"),
            ("[1]", "/-1"),
            ("[1]", "/+0"),
            ("[1]", "/9999999999999999999999999999999999"),
            (r#"{"x":1}"#, "/missing"),
            (r#"{"x":1}"#, "/x/child"),
            ("null", "/"),
            ("{}", "#"),
            ("{}", "/bad~"),
            ("{}", "/bad~2"),
            ("", ""),
            ("[1,]", ""),
            (r#"{"x":1,}"#, ""),
            ("01", ""),
            ("1e", ""),
            ("+1", ""),
            ("NaN", ""),
            ("true false", ""),
            (r#""\uD800""#, ""),
            (r#"{"bad":"\uDC00","ok":1}"#, "/ok"),
            (r#"{"\uD800":1}"#, ""),
        ] {
            assert!(
                select_json_lexical_utf8(source, pointer, &Control::none()).is_err(),
                "{source} {pointer}"
            );
        }
        assert_eq!(selected(r#"{"\uD83D\uDE80":7}"#, "/🚀").utf8, "7");
        assert_eq!(selected(r#"{"\u0000":7}"#, "/\0").utf8, "7");
    }

    #[test]
    fn reference_nesting_memory_input_and_control_bounds_are_enforced() {
        let at_limit = format!(
            "{}0{}",
            "[".repeat(nesting::MAX_ELEMENTS),
            "]".repeat(nesting::MAX_ELEMENTS)
        );
        assert_eq!(selected(&at_limit, "").utf8, at_limit);
        let too_deep = format!("[{at_limit}]");
        assert!(select_json_lexical_utf8(&too_deep, "", &Control::none()).is_err());
        let source = " ".repeat(RecordKey::MAX_BYTES + 1);
        assert!(matches!(
            select_json_lexical_utf8(&source, "", &Control::none()),
            Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::DecompressedBytes,
                ..
            }))
        ));
        let pointer = "/".repeat(1_500_000);
        assert!(matches!(
            select_json_lexical_utf8("{}", &pointer, &Control::none()),
            Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::Memory,
                ..
            }))
        ));
        let control = Control::none();
        control.cancel.cancel();
        assert!(matches!(
            select_json_lexical_utf8("{}", "", &control),
            Err(Error::Cancelled)
        ));
        let mut memory = Memory::new(MEMORY_LIMIT).unwrap();
        assert!(matches!(
            memory.add(1),
            Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::Memory,
                ..
            }))
        ));
    }
}
