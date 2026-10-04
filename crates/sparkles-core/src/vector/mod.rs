//! Vector similarity: the `spk:vector` literal datatype, a deterministic scoring kernel,
//! top-k search (`spk:vectorSearch`) and configured indexes with an HNSW graph.
//!
//! A vector is an ordinary RDF literal, `"[0.1, 0.2, 0.3]"^^<urn:x-sparkles:vector>`:
//! a JSON array of 1..=16384 finite numbers, each mapped to the nearest `f32`. A compact
//! form, `"zczMPc3MTD6amZk+"^^<urn:x-sparkles:vectorB64>`, holds the base64 of the
//! values as little-endian binary32. Both datatypes are vectors everywhere. The literal
//! stays authoritative (Sparkles never rewrites it).
//!
//! Search reads packed `f32` segments of a generation's base index. Without a
//! configuration they are packed per (predicate, dimension) on a predicate's first
//! search. A configured index (`vector.json`, see [`config`]) packs one predicate and
//! dimension in the background, adds an HNSW graph over the packed rows ([`hnsw`]), and
//! writes both to a file of the generation ([`persist`]). Every query overlays its
//! snapshot's inserted and deleted quads exactly, so each snapshot sees exactly its own
//! vectors whichever path answers it ([`search`]).

/// Datatype IRI of vector literals.
pub const DATATYPE: &str = "urn:x-sparkles:vector";
/// Datatype IRI of compact vector literals: the base64 (RFC 4648, with padding) of the
/// values as little-endian IEEE 754 binary32.
pub const DATATYPE_B64: &str = "urn:x-sparkles:vectorB64";

/// Whether `dt` is one of the two vector datatypes.
pub fn is_datatype(dt: &str) -> bool {
    dt == DATATYPE || dt == DATATYPE_B64
}

/// The vector of a literal of either vector datatype (`None` for another datatype).
pub fn parse_typed(lex: &str, dt: &str) -> Option<std::result::Result<Vec<f32>, String>> {
    match dt {
        DATATYPE => Some(parse(lex)),
        DATATYPE_B64 => Some(parse_b64(lex)),
        _ => None,
    }
}

/// Parse a compact vector's lexical form: base64 of 1..=16384 little-endian binary32
/// values, all finite. The error names the offset of the first problem.
pub fn parse_b64(lex: &str) -> std::result::Result<Vec<f32>, String> {
    if lex.len() > 1 << 20 {
        return Err("lexical form longer than 1 MiB".into());
    }
    let at =
        |i: usize, what: &str| format!("malformed spk:vectorB64 literal at offset {i}: {what}");
    let b = lex.as_bytes();
    if b.is_empty() || !b.len().is_multiple_of(4) {
        return Err(at(b.len(), "the length is not a positive multiple of 4"));
    }
    let val = |i: usize, c: u8| -> std::result::Result<u32, String> {
        Ok(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(at(i, "not a base64 character")),
        } as u32)
    };
    let mut bytes = Vec::with_capacity(b.len() / 4 * 3);
    for (q, chunk) in b.chunks(4).enumerate() {
        let i = q * 4;
        let last = i + 4 == b.len();
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return Err(at(i + 4 - pad, "misplaced '='"));
        }
        let mut n = 0u32;
        for (j, &c) in chunk[..4 - pad].iter().enumerate() {
            n |= val(i + j, c)? << (18 - 6 * j);
        }
        // the bits a padded group leaves over must be zero (a canonical encoding)
        if (pad == 1 && n & 0xff != 0) || (pad == 2 && n & 0xffff != 0) {
            return Err(at(i + 3 - pad, "non-zero padding bits"));
        }
        let three = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        bytes.extend_from_slice(&three[..3 - pad]);
    }
    if !bytes.len().is_multiple_of(4) {
        return Err(at(b.len(), "not a whole number of 4-byte values"));
    }
    let out: Vec<f32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    if out.len() > MAX_DIM {
        return Err(at(0, "more than 16384 elements"));
    }
    if let Some(i) = out.iter().position(|x| !x.is_finite()) {
        // value i starts at byte 4i, in the base64 group that begins at character 4i / 3 * 4
        return Err(at(i * 4 / 3 * 4, "a value that is not finite"));
    }
    Ok(out)
}

/// The compact lexical form of a vector (base64 of little-endian binary32).
pub fn canonical_b64(v: &[f32]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for j in 0..4 {
            if j <= c.len() {
                out.push(ALPHA[(n >> (18 - 6 * j) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
/// Namespace of the vector functions and property function.
pub const NS: &str = "urn:x-sparkles:";
/// The top-k search property function.
pub const VECTOR_SEARCH: &str = "urn:x-sparkles:vectorSearch";
/// Largest dimension.
pub const MAX_DIM: usize = 16384;
/// Largest `k`.
pub const MAX_K: usize = 10_000;

static BUDGET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(4 << 30);

/// Memory the packed vectors of one generation may use (default 4 GiB).
pub fn budget() -> u64 {
    BUDGET.load(std::sync::atomic::Ordering::Relaxed)
}

/// Set the vector memory budget of this process.
pub fn set_budget(bytes: u64) {
    BUDGET.store(bytes, std::sync::atomic::Ordering::Relaxed);
}

/// Parse a vector's lexical form. The error names the byte offset of the first problem.
pub fn parse(lex: &str) -> std::result::Result<Vec<f32>, String> {
    if lex.len() > 1 << 20 {
        return Err("lexical form longer than 1 MiB".into());
    }
    let b = lex.as_bytes();
    let mut i = 0;
    let ws = |i: &mut usize| {
        while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\n' | b'\r') {
            *i += 1;
        }
    };
    let at = |i: usize, what: &str| format!("malformed spk:vector literal at offset {i}: {what}");
    ws(&mut i);
    if b.get(i) != Some(&b'[') {
        return Err(at(i, "expected '['"));
    }
    i += 1;
    let mut out = Vec::new();
    loop {
        ws(&mut i);
        let start = i;
        // number = [ "-" ] ( "0" / [1-9] *DIGIT ) [ "." 1*DIGIT ] [ ("e"/"E") ["+"/"-"] 1*DIGIT ]
        if b.get(i) == Some(&b'-') {
            i += 1;
        }
        let digits = |i: &mut usize| {
            let s = *i;
            while *i < b.len() && b[*i].is_ascii_digit() {
                *i += 1;
            }
            *i - s
        };
        match b.get(i) {
            Some(b'0') => i += 1,
            Some(b'1'..=b'9') => {
                digits(&mut i);
            }
            _ => return Err(at(i, "expected a number")),
        }
        if b.get(i) == Some(&b'.') {
            i += 1;
            if digits(&mut i) == 0 {
                return Err(at(i, "expected a digit"));
            }
        }
        if matches!(b.get(i), Some(b'e' | b'E')) {
            i += 1;
            if matches!(b.get(i), Some(b'+' | b'-')) {
                i += 1;
            }
            if digits(&mut i) == 0 {
                return Err(at(i, "expected an exponent"));
            }
        }
        let x: f32 = lex[start..i]
            .parse()
            .map_err(|_| at(start, "not a number"))?;
        if !x.is_finite() {
            return Err(at(start, "out of the f32 range"));
        }
        out.push(x);
        if out.len() > MAX_DIM {
            return Err(at(start, "more than 16384 elements"));
        }
        ws(&mut i);
        match b.get(i) {
            Some(b',') => i += 1,
            Some(b']') => {
                i += 1;
                ws(&mut i);
                return if i == b.len() {
                    Ok(out)
                } else {
                    Err(at(i, "trailing characters"))
                };
            }
            _ => return Err(at(i, "expected ',' or ']'")),
        }
    }
}

/// Canonical lexical form (used only for literals Sparkles creates).
pub fn canonical(v: &[f32]) -> String {
    let parts: Vec<String> = v.iter().map(|x| format!("{x:?}")).collect();
    format!("[{}]", parts.join(","))
}

/// A vector literal in canonical form.
pub fn literal(v: &[f32]) -> oxrdf::Literal {
    oxrdf::Literal::new_typed_literal(canonical(v), oxrdf::NamedNode::new_unchecked(DATATYPE))
}

/// How search results are scored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Metric {
    /// cosine similarity (higher is better)
    Cosine,
    /// dot product (higher is better)
    Dot,
    /// L2 distance (lower is better)
    Euclidean,
}

impl Metric {
    pub fn parse(s: &str) -> Option<Metric> {
        Some(match s {
            "cosine" => Metric::Cosine,
            "dot" => Metric::Dot,
            "euclidean" => Metric::Euclidean,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Metric::Cosine => "cosine",
            Metric::Dot => "dot",
            Metric::Euclidean => "euclidean",
        }
    }
    /// Whether larger scores are better.
    pub fn higher_is_better(self) -> bool {
        !matches!(self, Metric::Euclidean)
    }
}

/// Σ f(aᵢ, bᵢ) with eight independent accumulators summed in a fixed order: the result is
/// the same whether or not the loop is vectorized.
#[inline]
fn lanes(a: &[f32], b: &[f32], f: impl Fn(f32, f32) -> f32) -> f32 {
    let mut acc = [0f32; 8];
    let (ca, ra) = a.as_chunks::<8>();
    let (cb, rb) = b.as_chunks::<8>();
    for (x, y) in ca.iter().zip(cb) {
        for l in 0..8 {
            acc[l] += f(x[l], y[l]);
        }
    }
    for (l, (x, y)) in ra.iter().zip(rb).enumerate() {
        acc[l] += f(*x, *y);
    }
    ((acc[0] + acc[4]) + (acc[1] + acc[5])) + ((acc[2] + acc[6]) + (acc[3] + acc[7]))
}

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    lanes(a, b, |x, y| x * y)
}

/// Squared Euclidean distance.
pub(crate) fn l2sq(a: &[f32], b: &[f32]) -> f32 {
    lanes(a, b, |x, y| (x - y) * (x - y))
}

/// The distance the HNSW graph orders by (lower is nearer): `1 − cosine`, `−dot` or the
/// squared L2 distance. Rows a metric cannot score (a zero norm with cosine) are
/// infinitely far.
#[inline]
pub(crate) fn graph_dist(m: Metric, a: &[f32], a_norm: f32, b: &[f32], b_norm: f32) -> f32 {
    let d = match m {
        Metric::Cosine => {
            let n = a_norm * b_norm;
            if n == 0.0 {
                return f32::INFINITY;
            }
            1.0 - dot(a, b) / n
        }
        Metric::Dot => -dot(a, b),
        Metric::Euclidean => l2sq(a, b),
    };
    if d.is_nan() { f32::INFINITY } else { d }
}

/// Euclidean norm.
pub fn norm(a: &[f32]) -> f32 {
    dot(a, a).sqrt()
}

/// Score of `b` against `a` (`a_norm`, `b_norm`: their norms, used by cosine). `None`
/// when undefined: different dimensions, a zero norm with cosine, or a non-finite result.
pub fn score(m: Metric, a: &[f32], a_norm: f32, b: &[f32], b_norm: f32) -> Option<f32> {
    if a.len() != b.len() {
        return None;
    }
    let s = match m {
        Metric::Dot => dot(a, b),
        Metric::Euclidean => lanes(a, b, |x, y| (x - y) * (x - y)).sqrt(),
        Metric::Cosine => {
            if a_norm == 0.0 || b_norm == 0.0 {
                return None;
            }
            (dot(a, b) / (a_norm * b_norm)).clamp(-1.0, 1.0)
        }
    };
    s.is_finite().then_some(s)
}

/// The vector of a stored literal key (`"lex 0xFF ^urn:x-sparkles:vector`, or the
/// compact datatype), if it is a well-typed vector literal.
pub fn from_key(key: &[u8]) -> Option<Vec<f32>> {
    let rest = key.strip_prefix(b"\"")?;
    let sep = rest.iter().rposition(|&b| b == 0xFF)?;
    let dt = std::str::from_utf8(rest[sep + 1..].strip_prefix(b"^")?).ok()?;
    parse_typed(std::str::from_utf8(&rest[..sep]).ok()?, dt)?.ok()
}

/// Whether a stored key is a literal of one of the vector datatypes (well-typed or not).
pub fn is_vector_key(key: &[u8]) -> bool {
    key.first() == Some(&b'"')
        && (key.ends_with(DATATYPE.as_bytes()) || key.ends_with(DATATYPE_B64.as_bytes()))
}

pub mod config;
pub mod embed;
pub mod hnsw;
mod index;
pub mod persist;
mod search;

pub use config::{HnswConfig, SearchMode, VectorConfigFile, VectorIndexConfig, VectorIndexStatus};
pub(crate) use index::{BuildCtl, Outcome, build_index};
pub use index::{Built, GenerationVectors, PackedStatus, PredicateVectors, Segment, check_files};
pub(crate) use search::overlay_counts;
pub use search::{Hit, Search, SearchInfo, search};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar() {
        assert_eq!(parse("[1, 0.5,-2e1 ]").unwrap(), [1.0, 0.5, -20.0]);
        assert_eq!(parse(" [0] ").unwrap(), [0.0]);
        for bad in [
            "[]", "[NaN]", "[+1]", "[.5]", "[1.]", "[0x1]", "[[1]]", "[1,]", "1", "[1e39]", "[01]",
            "[1] x",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(parse("[1, x]").unwrap_err().contains("offset 4"));
        let v = [1.0f32, 0.1, 1e-7, -0.0];
        assert_eq!(parse(&canonical(&v)).unwrap(), v);
    }

    #[test]
    fn compact_grammar() {
        // 0.1, 0.2, 0.3 as little-endian binary32
        assert_eq!(parse_b64("zczMPc3MTD6amZk+").unwrap(), [0.1, 0.2, 0.3]);
        for v in [
            vec![1.0f32],
            vec![1.0, -2.5],
            vec![0.1, 1e-7, -0.0, 3.0e38],
            (0..300).map(|i| (i as f32).sin()).collect(),
        ] {
            let lex = canonical_b64(&v);
            assert_eq!(parse_b64(&lex).unwrap(), v, "{lex}");
            assert_eq!(parse_typed(&lex, DATATYPE_B64).unwrap().unwrap(), v);
        }
        // three bytes are not a value; NaN and infinity are not finite
        let nan = canonical_b64(&[f32::NAN]);
        for bad in [
            "",
            "AAA",
            "AAAA",
            "AAAAAAA=",
            "zczMPc3MTD6amZk",
            "zczM Pc3M",
            "zczMPc3MTD6amZk*",
            "A===",
            "AAAA=AAA",
            &nan,
            &canonical_b64(&[f32::INFINITY]),
        ] {
            assert!(parse_b64(bad).is_err(), "{bad:?}");
        }
        assert!(parse_b64("AAA=").is_err() && parse_b64("AB==").is_err());
        assert!(parse_typed("[1]", "http://example.org/x").is_none());
        let key = [
            b"\"".as_slice(),
            b"AACAPw==",
            &[0xFF],
            b"^",
            DATATYPE_B64.as_bytes(),
        ]
        .concat();
        assert_eq!(from_key(&key), Some(vec![1.0]));
        assert!(is_vector_key(&key));
    }

    #[test]
    fn kernel() {
        let a = [1.0, 2.0, 3.0];
        let b = [4.0, 5.0, 6.0];
        assert_eq!(dot(&a, &b), 32.0);
        assert_eq!(
            score(Metric::Euclidean, &[0.0, 0.0], 0.0, &[3.0, 4.0], 5.0),
            Some(5.0)
        );
        assert_eq!(score(Metric::Cosine, &a, norm(&a), &[0.0; 3], 0.0), None);
        assert_eq!(score(Metric::Dot, &a, 0.0, &[1.0, 2.0], 0.0), None);
        // a long vector: the lanes give the same sum however the loop is compiled
        let long: Vec<f32> = (0..1000).map(|i| (i as f32).sin()).collect();
        assert_eq!(dot(&long, &long), dot(&long, &long));
        let c = score(Metric::Cosine, &long, norm(&long), &long, norm(&long)).unwrap();
        assert!((c - 1.0).abs() < 1e-6);
    }
}
