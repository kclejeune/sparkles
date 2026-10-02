//! The formatter's promises on any input, shared by the fuzz targets and by the regression
//! tests (`tests/fuzz_regressions.rs` includes this file):
//!
//! - it never panics;
//! - it either refuses with a syntax error positioned inside the input, or prints output
//!   that means what the input means (the reference parse of each), keeps every comment,
//!   and formats to itself under the same options;
//! - a cursor maps to a character boundary of the output;
//! - N-Triples and N-Quads print the same bytes in memory and streamed, in any chunking,
//!   with or without spilling sorted runs to disk.
//!
//! Two other refusals are deliberate, and pass: nesting deeper than the formatter takes,
//! and SPARQL that only the reference parser's leniency accepts (see [`lenient_reading`]).
//!
//! The options come from a header of [`HEADER`] bytes before the text, so the fuzzer
//! explores them along with the input; an all-zero header is the default options.

#![allow(dead_code)]

use sparkles_fmt::check::{comments, graph, json, sparql_equivalent, sparql_reference};
use sparkles_fmt::lex::{LexMode, TokenKind, lex};
use sparkles_fmt::lines::format_stream_chunked;
use sparkles_fmt::lines::scan::{LineKind, scan, split_lines};
use sparkles_fmt::sparql::keywords::Kw;
use sparkles_fmt::{
    DirectiveStyle, FormatError, Formatted, Language, LinesConfig, OperatorPosition, Options,
    QuoteStyle, TurtleLayout, format,
};
use std::path::PathBuf;

/// The bytes of option settings before the text.
pub const HEADER: usize = 5;

/// What the header asks for beyond the [`Options`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Extra {
    /// TriG rather than Turtle, N-Quads rather than N-Triples
    pub variant: bool,
    /// a sort budget so small that every statement spills (line formats)
    pub spill: bool,
    /// the chunk size of the streamed run (line formats)
    pub chunk_bytes: usize,
}

/// The options, the extras and the text of a fuzz input. An input shorter than the header
/// reads as the default options and no text.
///
/// - byte 0: `sort`, `prune-prefixes`, `canonicalize`, `directive-style = "turtle"`,
///   `type-shorthand = false`, `compact-iris = false`, `quote-style = "preserve"`,
///   `operator-position = "trailing"` (bits 0 to 7);
/// - byte 1: `turtle-layout = "conventional"`, `align-values`, two prefix groups, a
///   cursor, the language variant, spilling (bits 0 to 5), the chunk size (bits 6 and 7:
///   the default, 1, 7 or 64 bytes);
/// - byte 2: `line-width` (0 is the default, 100; otherwise 40 to 400);
/// - byte 3: `indent-width` (0 is the default, 2; otherwise 1 to 8);
/// - byte 4: where the cursor goes, as a fraction of the text.
pub fn decode(data: &[u8]) -> (Options, Extra, &[u8]) {
    let mut h = [0u8; HEADER];
    let n = data.len().min(HEADER);
    h[..n].copy_from_slice(&data[..n]);
    let text = &data[n..];
    let bit = |byte: u8, i: u32| byte & (1 << i) != 0;
    let mut o = Options {
        sort: bit(h[0], 0),
        prune_prefixes: bit(h[0], 1),
        canonicalize: bit(h[0], 2),
        type_shorthand: !bit(h[0], 4),
        compact_iris: !bit(h[0], 5),
        align_values: bit(h[1], 1),
        ..Options::default()
    };
    if bit(h[0], 3) {
        o.directive_style = DirectiveStyle::Turtle;
    }
    if bit(h[0], 6) {
        o.quote_style = QuoteStyle::Preserve;
    }
    if bit(h[0], 7) {
        o.operator_position = OperatorPosition::Trailing;
    }
    if bit(h[1], 0) {
        o.turtle_layout = TurtleLayout::Conventional;
    }
    if bit(h[1], 2) {
        o.prefix_groups = vec![
            vec!["rdf".into(), "rdfs".into(), "owl".into(), "xsd".into()],
            vec!["".into(), "ex".into()],
        ];
    }
    if bit(h[1], 3) {
        // on a character boundary, as the front ends' UTF-16 offsets convert
        let mut c = text.len() * h[4] as usize / 255;
        if let Ok(s) = std::str::from_utf8(text) {
            while !s.is_char_boundary(c) {
                c -= 1;
            }
        }
        o.cursor = Some(c);
    }
    if h[2] != 0 {
        o.line_width = (40 + (h[2] as u32 - 1) * 360 / 254) as u16;
    }
    if h[3] != 0 {
        o.indent_width = 1 + (h[3] - 1) % 8;
    }
    let extra = Extra {
        variant: bit(h[1], 4),
        spill: bit(h[1], 5),
        chunk_bytes: [0, 1, 7, 64][(h[1] >> 6) as usize],
    };
    (o, extra, text)
}

/// The header that [`decode`] reads as `opts` (prefix groups and the cursor aside) and
/// `extra`: what the seed corpus and the regression tests put before a text.
pub fn encode(opts: &Options, extra: Extra) -> [u8; HEADER] {
    let mut h = [0u8; HEADER];
    let flags0 = [
        opts.sort,
        opts.prune_prefixes,
        opts.canonicalize,
        opts.directive_style == DirectiveStyle::Turtle,
        !opts.type_shorthand,
        !opts.compact_iris,
        opts.quote_style == QuoteStyle::Preserve,
        opts.operator_position == OperatorPosition::Trailing,
    ];
    for (i, on) in flags0.into_iter().enumerate() {
        h[0] |= (on as u8) << i;
    }
    let flags1 = [
        opts.turtle_layout == TurtleLayout::Conventional,
        opts.align_values,
        !opts.prefix_groups.is_empty(),
        false,
        extra.variant,
        extra.spill,
    ];
    for (i, on) in flags1.into_iter().enumerate() {
        h[1] |= (on as u8) << i;
    }
    h[1] |= match extra.chunk_bytes {
        1 => 1,
        7 => 2,
        64 => 3,
        _ => 0,
    } << 6;
    if opts.line_width != 100 {
        h[2] = ((opts.line_width.clamp(40, 400) as u32 - 40) * 254).div_ceil(360) as u8 + 1;
    }
    if opts.indent_width != 2 {
        h[3] = opts.indent_width.clamp(1, 8);
    }
    h
}

/// Check every promise for `text` as `lang` under `opts`: `Err` says which one broke.
pub fn check(text: &str, lang: Language, opts: &Options) -> Result<(), String> {
    match format(text, lang, opts) {
        Err(e) => refused(text, lang, opts, &e),
        Ok(f) => formatted(text, lang, opts, &f),
    }
}

/// A refusal is fine when it is the user's syntax error, positioned in the input, or one
/// of the deliberate refusals.
fn refused(text: &str, lang: Language, opts: &Options, e: &FormatError) -> Result<(), String> {
    match e {
        FormatError::Unsupported { message, .. } if message.starts_with("nesting deeper than") => {
            Ok(())
        }
        FormatError::Unsupported { .. }
            if lang == Language::Sparql && lenient_reading(text, opts) =>
        {
            Ok(())
        }
        FormatError::Syntax {
            line,
            column,
            offset,
            ..
        } => {
            // N-Triples and N-Quads also break lines at a lone `\r`
            let lines = 1 + text.bytes().filter(|&b| b == b'\n' || b == b'\r').count();
            let inside = *line >= 1
                && *column >= 1
                && *offset <= text.len()
                && text.is_char_boundary(*offset)
                && *line as usize <= lines;
            match inside {
                true => Ok(()),
                false => Err(format!("syntax error positioned outside the input: {e:?}")),
            }
        }
        _ => Err(format!("refused with `{}`: {e}", e.code())),
    }
}

/// Whether `text`, which the formatter's parser refused after the reference parser
/// (spargebra) accepted it, formats once the tokens that spargebra alone reads as two are
/// spaced apart: a keyword glued to what follows it (`PREFIXex:` is `PREFIX ex:` to
/// spargebra, `PREFIX:` is `PREFIX :`, `GRAPHex:g` is `GRAPH ex:g`, `CONSTRUCTWHERE` is
/// `CONSTRUCT WHERE`), a prefixed name whose local part starts with `:` (`::q` is
/// `: :q`), or a decimal glued to the dot before it (`?o .2 ?s` is `?o . 2 ?s`). By the
/// grammar's longest match each of these is one token (a prefixed name, a word, a
/// decimal), so such a query is not SPARQL. Each step spaces the last such token at or
/// before where the formatter's parser stopped.
pub fn lenient_reading(text: &str, opts: &Options) -> bool {
    let mut spaced = text.to_string();
    for _ in 0..64 {
        match format(&spaced, Language::Sparql, opts) {
            Ok(_) => return spaced != text,
            Err(FormatError::Unsupported { line, column, .. }) => {
                let at = sparkles_fmt::offset_of(&spaced, line, column);
                match space_glued(&spaced, at) {
                    Some(next) => spaced = next,
                    None => return false,
                }
            }
            Err(_) => return false,
        }
    }
    false
}

/// `text` with the last glued token (see [`lenient_reading`]) that starts at or before
/// `at` spaced apart.
pub fn space_glued(text: &str, at: usize) -> Option<String> {
    let (start, splits) = lex(text, LexMode::Sparql)
        .into_iter()
        .take_while(|t| t.start as usize <= at)
        .filter_map(|t| {
            let start = t.start as usize;
            let word = &text[start..t.end()];
            let splits: Vec<usize> = match t.kind {
                // one keyword, then the prefix name (`PREFIXin:` is `PREFIX in:`); or a
                // local name that starts with `:` (`::q` is `: :q`)
                TokenKind::PnameNs | TokenKind::PnameLn => {
                    let (prefix, local) = word.split_once(':').unwrap_or((word, ""));
                    match leading_keyword(prefix) {
                        Some(k) => vec![k],
                        None if local.starts_with(':') => vec![prefix.len() + 1],
                        None => Vec::new(),
                    }
                }
                // keywords all the way (`CONSTRUCTWHERE`)
                TokenKind::Word => {
                    let mut splits = Vec::new();
                    let mut from = 0;
                    while let Some(k) = leading_keyword(&word[from..])
                        && from + k < word.len()
                    {
                        from += k;
                        splits.push(from);
                    }
                    splits
                }
                TokenKind::Decimal if word.starts_with('.') => vec![1],
                _ => Vec::new(),
            };
            (!splits.is_empty()).then_some((start, splits))
        })
        .last()?;
    let mut spaced = text[..start].to_string();
    let mut from = start;
    for s in splits {
        spaced.push_str(&text[from..start + s]);
        spaced.push(' ');
        from = start + s;
    }
    spaced.push_str(&text[from..]);
    Some(spaced)
}

/// The length of the longest keyword (two letters or more) `name` starts with.
fn leading_keyword(name: &str) -> Option<usize> {
    Kw::ALL
        .iter()
        .map(|k| k.canonical())
        .filter(|k| {
            k.len() >= 2
                && name.is_char_boundary(k.len())
                && name[..k.len()].eq_ignore_ascii_case(k)
        })
        .map(str::len)
        .max()
}

/// Output must mean what the input means, keep its comments, and be a fixpoint.
fn formatted(text: &str, lang: Language, opts: &Options, f: &Formatted) -> Result<(), String> {
    if f.language != lang {
        return Err(format!("formatted as {:?}, asked for {lang:?}", f.language));
    }
    if f.changed != (f.text != text) {
        return Err(format!("`changed` is {} for {:?}", f.changed, f.text));
    }
    if let Some(c) = opts.cursor {
        match f.cursor {
            Some(m) if m <= f.text.len() && f.text.is_char_boundary(m) => {}
            other => return Err(format!("cursor {c} mapped to {other:?}")),
        }
    }
    if f.changed {
        same_meaning(text, lang, opts, &f.text)?;
    }
    let again = Options {
        cursor: None,
        ..opts.clone()
    };
    match format(&f.text, lang, &again) {
        Ok(g) if g.text == f.text && !g.changed => Ok(()),
        Ok(g) => Err(format!(
            "not a fixpoint:\n--- first\n{}\n--- second\n{}",
            f.text, g.text
        )),
        Err(e) => Err(format!(
            "the output does not format ({e:?}):\n--- output\n{}",
            f.text
        )),
    }
}

/// `text` (a BOM dropped) with its `VERSION` lines emptied: oxttl's N-Triples and
/// N-Quads parsers do not know the directive, which the line formats check themselves.
fn without_versions(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = String::with_capacity(text.len());
    for (line, eol) in split_lines(text) {
        if scan(line).kind != LineKind::Version {
            out.push_str(line);
        }
        out.push_str(eol);
    }
    out
}

/// The independent equivalence and comment checks of a changed output.
fn same_meaning(text: &str, lang: Language, opts: &Options, out: &str) -> Result<(), String> {
    let meaning = match lang {
        Language::Sparql => {
            let r = sparql_reference(text, &lex(text, LexMode::Sparql))
                .map_err(|e| format!("the input formatted, but its reference parse fails: {e}"))?;
            sparql_equivalent(&r, out)
        }
        Language::JsonLd => {
            let r = json::json_reference(text)
                .map_err(|e| format!("the input formatted, but its reference parse fails: {e}"))?;
            json::json_equivalent(&r, out)
        }
        Language::NTriples | Language::NQuads => {
            let r = graph::rdf_reference(&without_versions(text), lang)
                .map_err(|e| format!("the input formatted, but its reference parse fails: {e}"))?;
            graph::rdf_equivalent(&r, &without_versions(out))
        }
        _ => {
            let r = graph::rdf_reference(text, lang)
                .map_err(|e| format!("the input formatted, but its reference parse fails: {e}"))?;
            graph::rdf_equivalent(&r, out)
        }
    };
    meaning.map_err(|e| format!("{e}:\n--- output\n{out}"))?;
    let mode = match lang {
        Language::Sparql => Some(LexMode::Sparql),
        Language::JsonLd => None,
        // `canonicalize` drops every comment, and says so
        Language::NTriples | Language::NQuads if opts.canonicalize => None,
        _ => Some(LexMode::Turtle),
    };
    if let Some(mode) = mode {
        comments::same(text, out, mode).map_err(|e| format!("{e}:\n--- output\n{out}"))?;
    }
    Ok(())
}

/// The line formats streamed from `bytes` (any bytes, not only UTF-8) print what
/// [`format`] prints, or fail as it does.
pub fn check_stream(
    bytes: &[u8],
    lang: Language,
    opts: &Options,
    extra: Extra,
) -> Result<(), String> {
    let opts = Options {
        cursor: None,
        ..opts.clone()
    };
    let cfg = LinesConfig {
        spill_dir: spill_dir(),
        // every statement spills its own run
        sort_memory: if extra.spill { 1 } else { u64::MAX },
        max_canonicalize_quads: 20_000_000,
        threads: 0,
    };
    let chunk_bytes = match extra.chunk_bytes {
        0 => 1 << 20,
        n => n,
    };
    let mut out = Vec::new();
    let streamed = format_stream_chunked(bytes, &mut out, lang, &opts, &cfg, chunk_bytes);
    let Ok(text) = std::str::from_utf8(bytes) else {
        // not UTF-8: refused, or kept byte for byte by `# sparkles-fmt: ignore-file`
        return match streamed {
            Ok(stats) if out != bytes || stats.changed => {
                Err("formatted input that is not UTF-8".into())
            }
            _ => Ok(()),
        };
    };
    match (streamed, format(text, lang, &opts)) {
        (Ok(stats), Ok(f)) if out == f.text.as_bytes() && stats.changed == f.changed => Ok(()),
        (Ok(stats), Ok(f)) => Err(format!(
            "streamed (changed: {}):\n{}\n--- in memory (changed: {}):\n{}",
            stats.changed,
            String::from_utf8_lossy(&out),
            f.changed,
            f.text
        )),
        (Err(a), Err(b)) if a.code() == b.code() => Ok(()),
        (a, b) => Err(format!(
            "streamed: {:?}\nin memory: {:?}",
            a.map(|_| String::from_utf8_lossy(&out).into_owned()),
            b.map(|f| f.text)
        )),
    }
}

/// A spill directory of this process's own (the sorter makes its run directories in it).
fn spill_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sparkles-fmt-fuzz-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}
