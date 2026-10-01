//! N-Triples 1.2 and N-Quads 1.2: one statement per line in the term spelling of
//! canonical N-Triples, each statement formatted and checked on its own (the oxttl quad
//! of the input statement equals the one of its formatted line), comments and blank
//! lines kept as written, source order kept unless `sort` (lines sorted by their bytes,
//! identical comment-free lines merged) or `canonicalize` (RDFC-1.0 labels, sorted, no
//! comments). [`format_stream`] reads line-aligned chunks and formats them on every core
//! (feature `parallel`); sorting past [`LinesConfig::sort_memory`] spills LZ4-framed
//! sorted runs and merges them.
//!
//! - [`scan`]: line breaks, and what a line holds;
//! - [`chunk`]: chunks of lines read from the input, formatted and checked;
//! - [`canonical`]: the canonical term spelling;
//! - [`assemble`]: header, comment blocks, blank lines, the order of the output;
//! - [`sort`]: the in-memory and external sort;
//! - [`canon`]: `canonicalize`.

pub mod assemble;
pub mod canon;
pub mod canonical;
pub mod chunk;
pub mod scan;
pub mod sort;

use crate::{FormatError, Formatted, Language, LineStats, LinesConfig, Options};
use assemble::{Assembler, Mode, io_error};
use chunk::{CHUNK_BYTES, Chunk, ChunkReader, RawChunk, format_chunk};
use std::collections::VecDeque;
use std::io::{BufRead, Write};

/// Whether N-Triples and N-Quads format.
pub const IMPLEMENTED: bool = true;
/// Whether `sort` acts on the line formats.
pub const SORT_IMPLEMENTED: bool = true;
/// Whether `canonicalize` acts on the line formats.
pub const CANONICALIZE_IMPLEMENTED: bool = true;

/// Format a whole N-Triples or N-Quads document in memory (what [`crate::format`] and
/// the HTTP endpoint use). It never spills to disk.
pub fn format_str(text: &str, lang: Language, opts: &Options) -> Result<Formatted, FormatError> {
    // no spill directory: `LinesConfig::default()` would ask the system for its
    // temporary directory, which WebAssembly has none of
    let cfg = LinesConfig {
        spill_dir: std::path::PathBuf::new(),
        sort_memory: u64::MAX,
        // `LinesConfig`'s default
        max_canonicalize_quads: 20_000_000,
        threads: 0,
    };
    let cursor = opts.cursor.map(|c| locate(text, c));
    let mut out = Vec::with_capacity(text.len() + text.len() / 16 + 1);
    let (stats, mapped) = run(
        text.as_bytes(),
        &mut out,
        lang,
        opts,
        &cfg,
        cursor,
        CHUNK_BYTES,
    )?;
    let out = String::from_utf8(out).expect("the output is built from strings");
    let changed = out != text;
    debug_assert_eq!(changed, stats.changed, "the streaming change tracking");
    let cursor = opts.cursor.map(|c| {
        mapped.unwrap_or_else(|| {
            let mut c = c.min(out.len());
            while !out.is_char_boundary(c) {
                c -= 1;
            }
            c
        })
    });
    Ok(Formatted {
        text: out,
        changed,
        cursor,
        language: lang,
        warnings: stats.warnings,
    })
}

/// Format N-Triples or N-Quads from `r` to `w` in bounded memory (what
/// [`crate::format_lines`] and `sparkles fmt` use for files).
pub fn format_stream(
    r: impl BufRead,
    w: impl Write,
    lang: Language,
    opts: &Options,
    cfg: &LinesConfig,
) -> Result<LineStats, FormatError> {
    run(r, w, lang, opts, cfg, None, CHUNK_BYTES).map(|(s, _)| s)
}

/// [`format_stream`] reading chunks of about `chunk_bytes` (tests use small ones, so that
/// their inputs span many chunks).
pub fn format_stream_chunked(
    r: impl BufRead,
    w: impl Write,
    lang: Language,
    opts: &Options,
    cfg: &LinesConfig,
    chunk_bytes: usize,
) -> Result<LineStats, FormatError> {
    run(r, w, lang, opts, cfg, None, chunk_bytes.max(1)).map(|(s, _)| s)
}

/// The line index and the byte in that line of byte `offset` of `text` (a BOM is not
/// part of the first line).
fn locate(text: &str, offset: usize) -> (u64, usize) {
    let bom = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let offset = offset.max(bom).min(text.len());
    let mut line = 0;
    let mut start = bom;
    for (l, eol) in scan::split_lines(&text[bom..]) {
        let end = start + l.len() + eol.len();
        if end > offset || eol.is_empty() {
            break;
        }
        line += 1;
        start = end;
    }
    (line, offset - start)
}

/// Whether a raw line (no break) is blank or a comment, and whether it is
/// `# sparkles-fmt: ignore-file`: `None` for any other line.
fn header_line(line: &[u8]) -> Option<bool> {
    let t = line.trim_ascii_start();
    let t = t
        .strip_prefix(b"\xEF\xBB\xBF")
        .unwrap_or(t)
        .trim_ascii_start();
    if t.is_empty() {
        return Some(false);
    }
    if t[0] != b'#' {
        return None;
    }
    Some(std::str::from_utf8(t).is_ok_and(crate::pragma::is_ignore_file))
}

/// The header's verdict on a chunk: `Some(true)` for `ignore-file`, `Some(false)` once
/// a line that is neither blank nor a comment shows, `None` when the whole chunk is
/// header.
fn header_verdict(bytes: &[u8]) -> Option<bool> {
    for line in bytes.split(|&b| b == b'\n' || b == b'\r') {
        match header_line(line) {
            None => return Some(false),
            Some(true) => return Some(true),
            Some(false) => {}
        }
    }
    None
}

fn run(
    r: impl BufRead,
    w: impl Write,
    lang: Language,
    opts: &Options,
    cfg: &LinesConfig,
    cursor: Option<(u64, usize)>,
    chunk_bytes: usize,
) -> Result<(LineStats, Option<usize>), FormatError> {
    if !lang.is_line_format() {
        return Err(FormatError::unsupported_language(lang));
    }
    let mut reader = ChunkReader::new(r);
    let mut w = std::io::BufWriter::with_capacity(64 << 10, w);
    // the header is read ahead: `# sparkles-fmt: ignore-file` there keeps the input as
    // it is, without even a reference parse
    let mut pending: VecDeque<RawChunk> = VecDeque::new();
    while let Some(c) = reader.next_chunk(chunk_bytes).map_err(io_error)? {
        let verdict = header_verdict(&c.bytes);
        pending.push_back(c);
        match verdict {
            Some(true) => {
                if reader.bom {
                    w.write_all(b"\xEF\xBB\xBF").map_err(io_error)?;
                }
                for c in &pending {
                    w.write_all(&c.bytes).map_err(io_error)?;
                }
                let (rest, mut r) = reader.into_rest();
                w.write_all(&rest).map_err(io_error)?;
                std::io::copy(&mut r, &mut w).map_err(io_error)?;
                w.flush().map_err(io_error)?;
                return Ok((LineStats::default(), None));
            }
            Some(false) => break,
            None => {}
        }
    }

    let mode = match (opts.canonicalize, opts.sort) {
        (true, _) => Mode::Canon,
        (false, true) => Mode::Sort,
        (false, false) => Mode::Plain,
    };
    let pool = Pool::new(cfg.threads);
    let mut asm = Assembler::new(&mut w, mode, lang, cfg, cursor);
    loop {
        while pending.len() < pool.batch() {
            match reader.next_chunk(chunk_bytes).map_err(io_error)? {
                Some(c) => pending.push_back(c),
                None => break,
            }
        }
        if pending.is_empty() {
            break;
        }
        crate::check::deadline(opts.deadline)?;
        let batch: Vec<RawChunk> = pending.drain(..).collect();
        for c in pool.format(batch, lang, mode == Mode::Canon) {
            asm.chunk(c?)?;
        }
    }
    let (done, _) = asm.finish(reader.bom)?;
    w.flush().map_err(io_error)?;
    let mut warnings = crate::option_warnings(opts, lang);
    warnings.extend(done.warnings);
    Ok((
        LineStats {
            statements: done.statements,
            changed: done.changed,
            warnings,
        },
        done.cursor,
    ))
}

/// Where chunks are formatted: rayon's current pool, or one of `threads` threads.
struct Pool {
    #[cfg(feature = "parallel")]
    own: Option<rayon::ThreadPool>,
    threads: usize,
}

impl Pool {
    #[cfg(feature = "parallel")]
    fn new(threads: usize) -> Pool {
        let current = rayon::current_num_threads();
        let own = (threads > 0 && threads != current)
            .then(|| {
                rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .ok()
            })
            .flatten();
        Pool {
            threads: if own.is_some() { threads } else { current },
            own,
        }
    }

    #[cfg(not(feature = "parallel"))]
    fn new(_threads: usize) -> Pool {
        Pool { threads: 1 }
    }

    /// Chunks formatted at once: a couple per thread, so that memory stays bounded by the
    /// chunk size.
    fn batch(&self) -> usize {
        self.threads.max(1) * 2
    }

    fn format(
        &self,
        batch: Vec<RawChunk>,
        lang: Language,
        quads: bool,
    ) -> Vec<Result<Chunk, FormatError>> {
        let f = |c: RawChunk| format_chunk(c, lang, quads);
        #[cfg(feature = "parallel")]
        if batch.len() > 1 {
            use rayon::prelude::*;
            let go = || batch.into_par_iter().map(f).collect();
            return match &self.own {
                Some(p) => p.install(go),
                None => go(),
            };
        }
        batch.into_iter().map(f).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: &str = "<http://e/s> <http://e/p>";

    fn fmt(text: &str, o: &Options) -> Formatted {
        let f = format_str(text, Language::NTriples, o).unwrap();
        // a fixpoint, and the same when streamed in small chunks
        let again = format_str(&f.text, Language::NTriples, o).unwrap();
        assert_eq!(again.text, f.text, "not a fixpoint:\n{}", f.text);
        assert!(!again.changed);
        let mut streamed = Vec::new();
        let stats = format_stream_chunked(
            text.as_bytes(),
            &mut streamed,
            Language::NTriples,
            o,
            &LinesConfig::default(),
            7,
        )
        .unwrap();
        assert_eq!(String::from_utf8(streamed).unwrap(), f.text);
        assert_eq!(stats.changed, f.changed);
        f
    }

    fn plain(text: &str) -> String {
        fmt(text, &Options::default()).text
    }

    fn sorted(text: &str) -> String {
        let o = Options {
            sort: true,
            ..Options::default()
        };
        fmt(text, &o).text
    }

    #[test]
    fn line_breaks_bom_and_the_final_newline() {
        let t = format!("\u{feff}{S} \"a\" .\r\n{S} \"b\" .\r{S} \"c\" .");
        let f = fmt(&t, &Options::default());
        assert_eq!(f.text, format!("{S} \"a\" .\n{S} \"b\" .\n{S} \"c\" .\n"));
        assert!(f.changed);
        let done = format!("{S} \"a\" .\n");
        assert!(!fmt(&done, &Options::default()).changed);
        assert_eq!(plain(""), "");
        assert_eq!(plain("\n\n  \n"), "");
        assert_eq!(
            plain("# only\n\n\n# comments  \n\n"),
            "# only\n\n# comments\n"
        );
    }

    #[test]
    fn version_lines_stay_where_they_are() {
        let t = format!("# h\nVERSION   \"1.2\"   # v\n\n{S} \"b\" .\n{S} \"a\" .\n");
        assert_eq!(
            plain(&t),
            format!("# h\n\nVERSION \"1.2\" # v\n\n{S} \"b\" .\n{S} \"a\" .\n")
        );
        // a sort barrier
        let t = format!("{S} \"b\" .\nVERSION '1.2'\n{S} \"z\" .\n{S} \"a\" .\n");
        assert_eq!(
            sorted(&t),
            format!("{S} \"b\" .\nVERSION '1.2'\n{S} \"a\" .\n{S} \"z\" .\n")
        );
        let o = Options {
            canonicalize: true,
            ..Options::default()
        };
        assert_eq!(
            fmt(&t, &o).text,
            format!("VERSION '1.2'\n{S} \"a\" .\n{S} \"b\" .\n{S} \"z\" .\n")
        );
        // not a directive: oxttl's syntax error
        let e = format_str("version \"1.2\"\n", Language::NTriples, &Options::default());
        assert!(
            matches!(e, Err(FormatError::Syntax { line: 1, .. })),
            "{e:?}"
        );
    }

    #[test]
    fn canonicalize_relabels_sorts_and_drops_comments() {
        let o = Options {
            canonicalize: true,
            ..Options::default()
        };
        let a = fmt(
            "# h\n_:x <http://e/p> _:y .\n\n_:y <http://e/q> \"1\" . # c\n_:x <http://e/p> _:y .\n",
            &o,
        );
        let b = fmt("_:b <http://e/q> \"1\" .\n_:a <http://e/p> _:b .\n", &o);
        assert_eq!(a.text, b.text);
        assert!(a.text.contains("_:c14n0") && !a.text.contains('#'));
        assert_eq!(a.warnings.len(), 1);
        assert_eq!(a.warnings[0].code, "comments-dropped");
        assert_eq!(a.warnings[0].message, "canonicalize dropped 2 comments");
        assert!(b.warnings.is_empty());
        // canonical already
        assert!(!fmt(&b.text, &o).changed);
        let t = fmt(
            "<http://e/s> <http://e/p> <<( _:a <http://e/p> \"o\" )>> .\n",
            &o,
        );
        assert_eq!(t.warnings[0].code, "unstable-labels");
        // too many quads
        let e = format_stream(
            b.text.as_bytes(),
            std::io::sink(),
            Language::NTriples,
            &o,
            &LinesConfig {
                max_canonicalize_quads: 1,
                ..LinesConfig::default()
            },
        );
        assert_eq!(e.unwrap_err(), FormatError::TooLarge);
    }

    #[test]
    fn ignore_file_keeps_the_input() {
        let t = "\u{feff}# x\n\n# sparkles-fmt: ignore-file\nnot  N-Triples at all\r\n";
        let f = format_str(t, Language::NTriples, &Options::default()).unwrap();
        assert_eq!(f.text, t);
        assert!(!f.changed);
        let mut out = Vec::new();
        let s = format_stream_chunked(
            t.as_bytes(),
            &mut out,
            Language::NTriples,
            &Options::default(),
            &LinesConfig::default(),
            3,
        )
        .unwrap();
        assert_eq!(out, t.as_bytes());
        assert!(!s.changed);
        // not in the header: an ordinary comment
        let e = format_str(
            "<http://a> <http://b> <http://c> .\n# sparkles-fmt: ignore-file\nbad\n",
            Language::NTriples,
            &Options::default(),
        );
        assert!(
            matches!(e, Err(FormatError::Syntax { line: 3, .. })),
            "{e:?}"
        );
    }

    #[test]
    fn the_cursor_stays_on_its_statement() {
        let t = format!("{S}   \"b\" .\n\n\n{S} \"a\" .   # c\n");
        let at = |cursor: usize, sort: bool| {
            let o = Options {
                cursor: Some(cursor),
                sort,
                ..Options::default()
            };
            let f = format_str(&t, Language::NTriples, &o).unwrap();
            (f.cursor.unwrap(), f.text)
        };
        // on the second statement's subject
        let second = t.find("<http://e/s> <http://e/p> \"a\"").unwrap();
        let (c, out) = at(second + 4, false);
        assert!(out[c..].starts_with("p://e/s> <http://e/p> \"a\""));
        let (c, out) = at(second + 4, true);
        assert_eq!(c, 4);
        assert!(out[c..].starts_with("p://e/s> <http://e/p> \"a\""));
        // in the collapsed blank lines: the next line
        let (c, out) = at(t.find("\n\n\n").unwrap() + 2, false);
        assert!(
            out[c..].starts_with("<http://e/s> <http://e/p> \"a\""),
            "{c}"
        );
        // past the end of a shortened line: clamped to it
        let (c, out) = at(t.find(" .\n").unwrap() + 1, false);
        assert!(c <= out.find('\n').unwrap());
    }

    #[test]
    fn statement_errors_name_their_line() {
        let t = format!("# c\n{S} \"a\" .\n\n{S} .\n");
        let e = format_str(&t, Language::NTriples, &Options::default()).unwrap_err();
        assert!(matches!(e, FormatError::Syntax { line: 4, .. }), "{e:?}");
        // a graph name is not N-Triples
        let q = format!("{S} \"a\" <http://g> .\n");
        assert!(format_str(&q, Language::NTriples, &Options::default()).is_err());
        assert!(format_str(&q, Language::NQuads, &Options::default()).is_ok());
    }

    #[test]
    fn duplicates_merge_only_when_sorted_and_comment_free() {
        let t = format!("{S} \"a\" .\n{S} \"a\" .\n{S} \"a\" . # kept\n");
        assert_eq!(plain(&t), t);
        assert_eq!(sorted(&t), format!("{S} \"a\" .\n{S} \"a\" . # kept\n"));
    }
}
