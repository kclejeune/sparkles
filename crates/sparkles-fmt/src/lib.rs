//! `sparkles-fmt`: an opinionated formatter for SPARQL 1.2 queries and updates (Turtle,
//! TriG, N-Triples, N-Quads and JSON-LD come later).
//!
//! Output depends only on the syntax tree, the comments, the blank-line groups and the
//! [`Options`]. Formatting never loses a comment, and it refuses its own output unless the
//! output parses to the same algebra as the input, keeps every comment, and formats to
//! itself again ([`FormatError::Unsafe`]).
//!
//! ```
//! use sparkles_fmt::{Language, Options, format};
//!
//! let out = format("SELECT * WHERE { ?s ?p ?o }\n", Language::Sparql, &Options::default()).unwrap();
//! assert!(!out.text.is_empty());
//! ```
//!
//! The pipeline: a lossless lexer ([`lex`]) and concrete syntax tree ([`tree`]), a
//! reference parse with spargebra ([`check`]), the language's printer building a document
//! ([`doc`]) with comments attached ([`trivia`]), the printer, then the safety checks.
//! Only [`format`], [`detect`], [`Options`] and the types around them are the stable API;
//! the other modules are public for the crate's own test suites.

use std::path::Path;
use std::time::Instant;

#[doc(hidden)]
pub mod check;
#[doc(hidden)]
pub mod cursor;
#[doc(hidden)]
pub mod doc;
#[doc(hidden)]
pub mod lex;
#[doc(hidden)]
pub mod normalize;
pub mod options;
#[doc(hidden)]
pub mod pragma;
#[doc(hidden)]
pub mod sparql;
#[doc(hidden)]
pub mod syntax;
#[doc(hidden)]
pub mod tree;
#[doc(hidden)]
pub mod trivia;

// ------------------------------------------------------------------ languages ------

/// A syntax the formatter knows about. Only [`Language::is_implemented`] ones format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    Sparql,
    Turtle,
    TriG,
    NTriples,
    NQuads,
    JsonLd,
}

impl Language {
    pub const ALL: [Language; 6] = [
        Language::Sparql,
        Language::Turtle,
        Language::TriG,
        Language::NTriples,
        Language::NQuads,
        Language::JsonLd,
    ];

    /// `sparql`, `turtle`, `trig`, `ntriples`, `nquads`, `jsonld`: the names of
    /// `--language` and of the HTTP `language` parameter.
    pub fn name(self) -> &'static str {
        match self {
            Language::Sparql => "sparql",
            Language::Turtle => "turtle",
            Language::TriG => "trig",
            Language::NTriples => "ntriples",
            Language::NQuads => "nquads",
            Language::JsonLd => "jsonld",
        }
    }

    /// The name people know it by, for messages (`SPARQL syntax error: …`).
    pub fn display_name(self) -> &'static str {
        match self {
            Language::Sparql => "SPARQL",
            Language::Turtle => "Turtle",
            Language::TriG => "TriG",
            Language::NTriples => "N-Triples",
            Language::NQuads => "N-Quads",
            Language::JsonLd => "JSON-LD",
        }
    }

    /// The language of a [`Language::name`] (ASCII case-insensitive).
    pub fn from_name(name: &str) -> Option<Language> {
        let name = name.trim();
        Language::ALL
            .into_iter()
            .find(|l| l.name().eq_ignore_ascii_case(name))
    }

    /// The language of a raw HTTP body's media type (parameters ignored). `text/plain`
    /// and anything else give `None`.
    pub fn from_media_type(media_type: &str) -> Option<Language> {
        let mt = media_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        Some(match mt.as_str() {
            "application/sparql-query" | "application/sparql-update" => Language::Sparql,
            "text/turtle" => Language::Turtle,
            "application/trig" => Language::TriG,
            "application/n-triples" => Language::NTriples,
            "application/n-quads" => Language::NQuads,
            "application/ld+json" => Language::JsonLd,
            _ => return None,
        })
    }

    /// Whether this build formats the language (SPARQL only, for now).
    pub fn is_implemented(self) -> bool {
        matches!(self, Language::Sparql)
    }
}

/// What a path or a text is, for the CLI's walks and for formatting without a language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detection {
    Lang(Language),
    /// `.rdf`, `.owl`, `.xml`, or content starting `<?xml` / `<rdf:RDF`: never formatted
    RdfXml,
    /// `.gz`, `.zst`, `.br`, `.lz4`: decompress first
    Compressed,
    /// `.n3`, `.shc`, `.json`: skipped in directory walks; an explicit path needs a
    /// language
    SkipInWalk,
    /// nothing to go by (an empty or comment-only text without a known extension)
    Unknown,
}

/// The message for RDF/XML, which is never formatted.
pub const RDF_XML_MESSAGE: &str =
    "RDF/XML formatting is not supported; convert to Turtle to format";

/// The detection of a path by its extension alone (ASCII case-insensitive), `None` for an
/// unknown extension. Directory walks visit only paths this recognizes.
pub fn detect_path(path: &Path) -> Option<Detection> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rq" | "ru" | "sparql" => Detection::Lang(Language::Sparql),
        "ttl" | "turtle" => Detection::Lang(Language::Turtle),
        "trig" => Detection::Lang(Language::TriG),
        "nt" => Detection::Lang(Language::NTriples),
        "nq" => Detection::Lang(Language::NQuads),
        "jsonld" => Detection::Lang(Language::JsonLd),
        "rdf" | "owl" | "xml" => Detection::RdfXml,
        "n3" | "shc" | "json" => Detection::SkipInWalk,
        "gz" | "zst" | "br" | "lz4" => Detection::Compressed,
        _ => return None,
    })
}

/// The language of a document: by the extension of `path` when it has a known one, else
/// by sniffing `text` (the first significant token after comments, whitespace and a
/// `PREFIX`/`BASE`/`VERSION` prologue). Sniffing never answers N-Triples, which is also
/// valid Turtle; it needs `.nt` or an explicit language.
pub fn detect(path: Option<&Path>, text: &str) -> Detection {
    if let Some(d) = path.and_then(detect_path) {
        return d;
    }
    sniff(text)
}

fn sniff(text: &str) -> Detection {
    use lex::TokenKind as T;
    let tokens = lex::lex(text, lex::LexMode::Sparql);
    let sig: Vec<lex::Token> = tokens.into_iter().filter(|t| !t.kind.is_trivia()).collect();
    let at = |i: usize| sig.get(i).map_or(T::Eof, |t| t.kind);
    let word = |i: usize| match sig.get(i) {
        Some(t) if matches!(t.kind, T::Word | T::LangDir) => {
            t.text(text).trim_start_matches('@').to_ascii_lowercase()
        }
        _ => String::new(),
    };
    let Some(first) = sig.first().filter(|t| t.kind != T::Eof) else {
        return Detection::Unknown;
    };
    let rest = &text[first.start as usize..];
    if rest.starts_with("<?xml") || rest.starts_with("<rdf:RDF") {
        return Detection::RdfXml;
    }
    if matches!(first.kind, T::LBrace | T::LBracket) {
        return Detection::Lang(Language::JsonLd);
    }
    // the prologue: SPARQL-style directives, and Turtle's `@prefix` family (lexed as
    // language tags here) with their final `.`
    let mut i = 0;
    loop {
        let turtle = at(i) == T::LangDir;
        let skip = match word(i).as_str() {
            "prefix" => 3,
            "base" | "version" => 2,
            _ => break,
        };
        i += skip;
        if turtle && at(i) == T::Dot {
            i += 1;
        }
    }
    const SPARQL: [&str; 14] = [
        "select",
        "construct",
        "ask",
        "describe",
        "insert",
        "delete",
        "with",
        "load",
        "clear",
        "drop",
        "create",
        "add",
        "move",
        "copy",
    ];
    if at(i) == T::Word && SPARQL.contains(&word(i).as_str()) {
        return Detection::Lang(Language::Sparql);
    }
    if word(i) == "graph" || at(i) == T::LBrace || at(i + 1) == T::LBrace {
        return Detection::Lang(Language::TriG);
    }
    // N-Quads: four terms (IRIs, blank nodes, literals) before the `.`
    let mut terms = 0;
    let mut j = i;
    loop {
        match at(j) {
            T::IriRef | T::BlankNodeLabel => {}
            T::String1 | T::String2 | T::StringLong1 | T::StringLong2 => {
                if at(j + 1) == T::LangDir {
                    j += 1;
                } else if at(j + 1) == T::HatHat && at(j + 2) == T::IriRef {
                    j += 2;
                }
            }
            T::Dot if terms == 4 => return Detection::Lang(Language::NQuads),
            _ => break,
        }
        terms += 1;
        j += 1;
    }
    Detection::Lang(Language::Turtle)
}

// -------------------------------------------------------------------- options ------

/// How Turtle and TriG write directives and graph blocks (`directive-style`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DirectiveStyle {
    /// `PREFIX`/`BASE`/`VERSION` without a final `.`, and `GRAPH g {`
    #[default]
    Sparql,
    /// `@prefix`/`@base`/`@version` with the final ` .`, and `g {`
    Turtle,
}

/// String quotes (`quote-style`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum QuoteStyle {
    /// `'…'` becomes `"…"` when the content has no `"`
    #[default]
    Double,
    /// every string keeps its quotes and escapes
    Preserve,
}

/// Where a broken `&&`/`||` chain puts its operators (`operator-position`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OperatorPosition {
    /// at the start of each continuation line
    #[default]
    Leading,
    /// at the end of the line before
    Trailing,
}

/// The layout of multi-line Turtle/TriG statements (`turtle-layout`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TurtleLayout {
    /// subject alone, a trailing `;` on every predicate line and a lone `.`
    #[default]
    Diff,
    /// the SPARQL compact form: `s p o ;`, further entries indented, ` .` after the last
    Conventional,
}

/// The style options (the twelve `.sparklesfmt.toml` keys, see [`options::KEYS`]) and the
/// per-call inputs (`canonicalize`, `cursor`, `deadline`). Set them through
/// [`options::set`], or check a built value with [`options::validate`]: [`format`] clamps
/// out-of-range widths rather than failing.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// 40..=400
    pub line_width: u16,
    /// 1..=8 spaces
    pub indent_width: u8,
    /// opt-in sorting (Turtle, TriG, JSON-LD terms, the line formats)
    pub sort: bool,
    /// drop prefix declarations nothing uses
    pub prune_prefixes: bool,
    /// N-Triples/N-Quads: canonical form (RDFC-1.0 labels, sorted, no comments)
    pub canonicalize: bool,
    pub directive_style: DirectiveStyle,
    /// groups of prefix labels (`""` is the empty label), in order
    pub prefix_groups: Vec<Vec<String>>,
    /// `rdf:type` → `a`, and `a` entries first
    pub type_shorthand: bool,
    /// full IRI → prefixed name
    pub compact_iris: bool,
    pub quote_style: QuoteStyle,
    pub operator_position: OperatorPosition,
    pub turtle_layout: TurtleLayout,
    /// align the cells of multi-variable `VALUES` rows
    pub align_values: bool,
    /// a cursor position in the input, in bytes, mapped to [`Formatted::cursor`]
    pub cursor: Option<usize>,
    /// give up with [`FormatError::Timeout`] after this instant
    pub deadline: Option<Instant>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            line_width: 100,
            indent_width: 2,
            sort: false,
            prune_prefixes: false,
            canonicalize: false,
            directive_style: DirectiveStyle::Sparql,
            prefix_groups: Vec::new(),
            type_shorthand: true,
            compact_iris: true,
            quote_style: QuoteStyle::Double,
            operator_position: OperatorPosition::Leading,
            turtle_layout: TurtleLayout::Diff,
            align_values: false,
            cursor: None,
            deadline: None,
        }
    }
}

// --------------------------------------------------------------------- results ------

/// A formatted document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Formatted {
    pub text: String,
    /// whether `text` differs from the input
    pub changed: bool,
    /// [`Options::cursor`] mapped into `text` (bytes)
    pub cursor: Option<usize>,
    pub language: Language,
    pub warnings: Vec<Warning>,
}

/// Something the caller should know that did not stop formatting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    /// `undeclared-prefix`, `comment-moved` or `option-not-implemented`
    pub code: &'static str,
    pub message: String,
    /// 1-based line of the input; 0 when the warning has no position
    pub line: u32,
    /// 1-based column in Unicode scalar values; 0 when the warning has no position
    pub column: u32,
}

/// A safety check that refused the formatter's output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Check {
    /// the output parses to a different SPARQL algebra (or does not parse)
    Algebra,
    /// the output parses to a different (non-isomorphic) graph or dataset
    Graph,
    /// a comment of the input is missing from the output
    Comments,
    /// formatting the output changes it again
    Idempotence,
}

impl Check {
    /// `algebra differs`, `graph differs`, `comment lost`, `not idempotent`
    pub fn message(self) -> &'static str {
        match self {
            Check::Algebra => "algebra differs",
            Check::Graph => "graph differs",
            Check::Comments => "comment lost",
            Check::Idempotence => "not idempotent",
        }
    }
}

impl std::fmt::Display for Check {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// Why a document was not formatted. Lines are 1-based; columns are 1-based and count
/// Unicode scalar values.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FormatError {
    /// the reference parser rejected the input (its message and position)
    #[error("syntax error at {line}:{column}: {message}")]
    Syntax {
        message: String,
        line: u32,
        column: u32,
        /// byte offset in the input
        offset: usize,
    },
    /// the reference parser accepted the input but the formatter's own parser cannot
    /// handle it: a formatter bug
    #[error("unsupported syntax at {line}:{column}: {message}")]
    Unsupported {
        message: String,
        line: u32,
        column: u32,
    },
    /// a safety check refused the output; the input must be left as it is
    #[error("formatter refused its own output ({check}); input left unchanged; please report")]
    Unsafe { check: Check },
    #[error("{message}")]
    UnsupportedLanguage { language: String, message: String },
    #[error("formatting did not finish before its deadline")]
    Timeout,
    #[error("the input is too large to format")]
    TooLarge,
}

impl FormatError {
    /// The machine-readable code (the HTTP error bodies' `code`): `syntax`,
    /// `unsupported-syntax`, `unsafe-format`, `unstable-format`, `unsupported-language`,
    /// `timeout` or `too-large`.
    pub fn code(&self) -> &'static str {
        match self {
            FormatError::Syntax { .. } => "syntax",
            FormatError::Unsupported { .. } => "unsupported-syntax",
            FormatError::Unsafe {
                check: Check::Idempotence,
            } => "unstable-format",
            FormatError::Unsafe { .. } => "unsafe-format",
            FormatError::UnsupportedLanguage { .. } => "unsupported-language",
            FormatError::Timeout => "timeout",
            FormatError::TooLarge => "too-large",
        }
    }

    /// A language this build does not format (RDF/XML gets its own message).
    pub fn unsupported_language(lang: Language) -> FormatError {
        FormatError::UnsupportedLanguage {
            language: lang.name().to_string(),
            message: format!(
                "{} formatting is not available yet",
                lang.name().to_ascii_lowercase()
            ),
        }
    }
}

/// Statistics of [`format_lines`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineStats {
    /// statements read
    pub statements: u64,
    /// whether any output line differs from its input line (or the order changed)
    pub changed: bool,
}

// ------------------------------------------------------------------ formatting ------

/// Format `text` as `lang`. A leading UTF-8 BOM is dropped; `# sparkles-fmt: ignore-file`
/// in the file header returns the input unchanged.
pub fn format(text: &str, lang: Language, opts: &Options) -> Result<Formatted, FormatError> {
    if !lang.is_implemented() {
        return Err(FormatError::unsupported_language(lang));
    }
    // token offsets are u32
    if text.len() > u32::MAX as usize {
        return Err(FormatError::TooLarge);
    }
    let opts = clamped(opts);
    match lang {
        Language::Sparql => check::run(&sparql::Sparql, text, &opts),
        _ => Err(FormatError::unsupported_language(lang)),
    }
}

/// Format N-Triples or N-Quads from `r` to `w` in one streaming pass (sorting spills runs
/// under `spill`). Not implemented yet: every language is refused.
pub fn format_lines(
    r: impl std::io::BufRead,
    w: impl std::io::Write,
    lang: Language,
    opts: &Options,
    spill: &Path,
) -> Result<LineStats, FormatError> {
    let _ = (r, w, opts, spill);
    Err(FormatError::unsupported_language(lang))
}

/// `opts` with the widths in range, for library callers that skipped validation.
fn clamped(opts: &Options) -> Options {
    let mut o = opts.clone();
    o.line_width = o.line_width.clamp(
        *options::LINE_WIDTH.start() as u16,
        *options::LINE_WIDTH.end() as u16,
    );
    o.indent_width = o.indent_width.clamp(
        *options::INDENT_WIDTH.start() as u8,
        *options::INDENT_WIDTH.end() as u8,
    );
    o
}

/// The `option-not-implemented` warnings of keys that are accepted but do nothing yet.
pub(crate) fn option_warnings(opts: &Options) -> Vec<Warning> {
    let mut w = Vec::new();
    for (on, key) in [
        (opts.prune_prefixes, "prune-prefixes"),
        (opts.align_values, "align-values"),
    ] {
        if on {
            w.push(Warning {
                code: "option-not-implemented",
                message: format!("{key} is not implemented yet and has no effect"),
                line: 0,
                column: 0,
            });
        }
    }
    w
}

/// The 1-based line and column (in Unicode scalar values) of byte `offset` in `src`. A
/// leading BOM is not a column.
pub fn line_col(src: &str, offset: usize) -> (u32, u32) {
    let bom = bom_len(src);
    let src = &src[bom..];
    let mut offset = offset.saturating_sub(bom).min(src.len());
    while !src.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &src[..offset];
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let line = before.bytes().filter(|&b| b == b'\n').count() + 1;
    let column = before[line_start..].chars().count() + 1;
    (line as u32, column as u32)
}

/// The byte offset of a 1-based line and column (Unicode scalar values, a leading BOM
/// not counted) in `src`, clamped to the end of the line and of the text.
pub fn offset_of(src: &str, line: u32, column: u32) -> usize {
    let bom = bom_len(src);
    let src = &src[bom..];
    let mut start = 0;
    for _ in 1..line.max(1) {
        match src[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return bom + src.len(),
        }
    }
    let line_text = &src[start..];
    let line_end = line_text.find('\n').unwrap_or(line_text.len());
    let col = line_text[..line_end]
        .char_indices()
        .nth(column.max(1) as usize - 1)
        .map_or(line_end, |(i, _)| i);
    bom + start + col
}

fn bom_len(src: &str) -> usize {
    if src.starts_with('\u{feff}') { 3 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_by_name_and_media_type() {
        for l in Language::ALL {
            assert_eq!(Language::from_name(l.name()), Some(l));
        }
        assert_eq!(Language::from_name("SPARQL"), Some(Language::Sparql));
        assert_eq!(Language::from_name("rdfxml"), None);
        assert_eq!(
            Language::from_media_type("application/sparql-update; charset=utf-8"),
            Some(Language::Sparql)
        );
        assert_eq!(
            Language::from_media_type("Text/Turtle"),
            Some(Language::Turtle)
        );
        assert_eq!(Language::from_media_type("text/plain"), None);
        assert!(Language::Sparql.is_implemented());
        assert!(!Language::Turtle.is_implemented());
    }

    #[test]
    fn detects_by_extension() {
        let d = |p: &str| detect(Some(Path::new(p)), "");
        assert_eq!(d("q.rq"), Detection::Lang(Language::Sparql));
        assert_eq!(d("Q.RU"), Detection::Lang(Language::Sparql));
        assert_eq!(d("a/b.sparql"), Detection::Lang(Language::Sparql));
        assert_eq!(d("x.ttl"), Detection::Lang(Language::Turtle));
        assert_eq!(d("x.turtle"), Detection::Lang(Language::Turtle));
        assert_eq!(d("x.trig"), Detection::Lang(Language::TriG));
        assert_eq!(d("x.nt"), Detection::Lang(Language::NTriples));
        assert_eq!(d("x.nq"), Detection::Lang(Language::NQuads));
        assert_eq!(d("x.jsonld"), Detection::Lang(Language::JsonLd));
        assert_eq!(d("x.owl"), Detection::RdfXml);
        assert_eq!(d("x.n3"), Detection::SkipInWalk);
        assert_eq!(d("x.ttl.gz"), Detection::Compressed);
        assert_eq!(d("x.txt"), Detection::Unknown);
        assert_eq!(detect_path(Path::new("x.txt")), None);
        assert_eq!(detect_path(Path::new("Makefile")), None);
    }

    #[test]
    fn sniffs_content() {
        let s = |t: &str| detect(None, t);
        assert_eq!(s(""), Detection::Unknown);
        assert_eq!(s("# only a comment\n"), Detection::Unknown);
        assert_eq!(
            s("PREFIX ex: <http://example.org/>\nselect * { ?s ?p ?o }"),
            Detection::Lang(Language::Sparql)
        );
        assert_eq!(
            s("# c\nBASE <http://x/> VERSION \"1.2\" INSERT DATA {}"),
            Detection::Lang(Language::Sparql)
        );
        assert_eq!(s("CLEAR ALL"), Detection::Lang(Language::Sparql));
        assert_eq!(s("{ \"@id\": \"x\" }"), Detection::Lang(Language::JsonLd));
        assert_eq!(s("<?xml version=\"1.0\"?>"), Detection::RdfXml);
        assert_eq!(
            s("<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">"),
            Detection::RdfXml
        );
        assert_eq!(
            s("@prefix ex: <http://example.org/> .\nex:a ex:b ex:c ."),
            Detection::Lang(Language::Turtle)
        );
        assert_eq!(
            s("<http://a> <http://b> <http://c> ."),
            Detection::Lang(Language::Turtle)
        );
        assert_eq!(
            s("<http://a> <http://b> \"x\"@en <http://g> ."),
            Detection::Lang(Language::NQuads)
        );
        assert_eq!(
            s("_:a <http://b> \"1\"^^<http://int> _:g ."),
            Detection::Lang(Language::NQuads)
        );
        assert_eq!(
            s("PREFIX ex: <http://example.org/>\nGRAPH ex:g { ex:a ex:b ex:c }"),
            Detection::Lang(Language::TriG)
        );
        assert_eq!(
            s("ex:g { ex:a ex:b ex:c }"),
            Detection::Lang(Language::TriG)
        );
    }

    #[test]
    fn error_codes() {
        let syntax = FormatError::Syntax {
            message: "expected one of …".into(),
            line: 1,
            column: 9,
            offset: 8,
        };
        assert_eq!(syntax.code(), "syntax");
        assert_eq!(syntax.to_string(), "syntax error at 1:9: expected one of …");
        let e = |check| FormatError::Unsafe { check };
        assert_eq!(e(Check::Algebra).code(), "unsafe-format");
        assert_eq!(e(Check::Comments).code(), "unsafe-format");
        assert_eq!(e(Check::Idempotence).code(), "unstable-format");
        assert_eq!(
            e(Check::Algebra).to_string(),
            "formatter refused its own output (algebra differs); input left unchanged; please report"
        );
        let t = FormatError::unsupported_language(Language::Turtle);
        assert_eq!(t.code(), "unsupported-language");
        assert_eq!(t.to_string(), "turtle formatting is not available yet");
        assert_eq!(FormatError::Timeout.code(), "timeout");
        assert_eq!(FormatError::TooLarge.code(), "too-large");
    }

    #[test]
    fn positions() {
        let s = "ab\nçd\n\nx";
        assert_eq!(line_col(s, 0), (1, 1));
        assert_eq!(line_col(s, 3), (2, 1));
        assert_eq!(line_col(s, 5), (2, 2));
        assert_eq!(line_col(s, s.len()), (4, 2));
        for off in [0, 1, 3, 5, 6, 7, 8] {
            let (l, c) = line_col(s, off);
            assert_eq!(offset_of(s, l, c), off, "{off}");
        }
        assert_eq!(offset_of(s, 9, 1), s.len());
        assert_eq!(offset_of(s, 1, 99), 2);
        let b = "\u{feff}ab\nc";
        assert_eq!(line_col(b, 3), (1, 1));
        assert_eq!(line_col(b, 6), (2, 1));
        assert_eq!(offset_of(b, 1, 1), 3);
        assert_eq!(offset_of(b, 2, 2), b.len());
    }

    #[test]
    fn identity_pipeline() {
        let q = "PREFIX ex: <http://e/>\n# c\nselect * { ?s ex:p [ ex:q 1 ] ; dc:x ?o }\n";
        let opts = Options {
            cursor: Some(5),
            ..Options::default()
        };
        let f = format(q, Language::Sparql, &opts).unwrap();
        assert_eq!(f.text, q);
        assert!(!f.changed);
        assert_eq!(f.cursor, Some(5));
        assert_eq!(f.language, Language::Sparql);
        assert_eq!(f.warnings.len(), 1);
        assert_eq!(f.warnings[0].code, "undeclared-prefix");
        assert_eq!((f.warnings[0].line, f.warnings[0].column), (3, 33));

        let u = "INSERT DATA { <a> <b> <c> } ;\nCLEAR ALL";
        assert_eq!(
            format(u, Language::Sparql, &Options::default())
                .unwrap()
                .text,
            u
        );

        let e = format("select * {", Language::Sparql, &Options::default()).unwrap_err();
        assert!(
            matches!(
                e,
                FormatError::Syntax {
                    line: 1,
                    column: 11,
                    offset: 10,
                    ..
                }
            ),
            "{e:?}"
        );
    }

    #[test]
    fn ignore_file_and_option_warnings() {
        let t = "# sparkles-fmt: ignore-file\nnot SPARQL at all {";
        let f = format(t, Language::Sparql, &Options::default()).unwrap();
        assert_eq!(f.text, t);
        assert!(!f.changed && f.warnings.is_empty());

        let opts = Options {
            prune_prefixes: true,
            align_values: true,
            ..Options::default()
        };
        let f = format("ASK {}", Language::Sparql, &opts).unwrap();
        let codes: Vec<_> = f.warnings.iter().map(|w| w.code).collect();
        assert_eq!(codes, ["option-not-implemented", "option-not-implemented"]);
    }

    #[test]
    fn a_bom_is_dropped() {
        let f = format("\u{feff}ASK {}", Language::Sparql, &Options::default()).unwrap();
        assert_eq!(f.text, "ASK {}");
        assert!(f.changed);
    }

    #[test]
    fn other_languages_are_refused() {
        let e = format("<a> <b> <c> .", Language::Turtle, &Options::default()).unwrap_err();
        assert_eq!(e, FormatError::unsupported_language(Language::Turtle));
    }
}
