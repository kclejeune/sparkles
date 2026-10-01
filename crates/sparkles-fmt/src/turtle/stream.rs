//! Streaming Turtle and TriG: a document of any size formatted in one pass (two with
//! `prune-prefixes`) in bounded memory, printing what [`crate::format`] prints, byte for
//! byte. `sort` needs the whole document and does not stream ([`can_stream`]).
//!
//! **Splitting.** A scanner lexes the input a line at a time and finds where top-level
//! items end: a run of directives, a statement (its `.` outside any bracket), a TriG graph
//! block (its `}`); inside a graph block, each statement. Statements are formatted in
//! windows of about [`StreamConfig::window_bytes`] of input, each a document of its own:
//!
//! ```text
//! PREFIX … / BASE …          the prefixes and base in force (as written)
//! [] <#b> <#b> # b
//! .  GRAPH g {               a barrier statement; the graph block being resumed
//! ctx  new items …           the last item of the previous window again, then the new ones
//! }                          when the window ends inside a graph block
//! <#b> <#b> <#b> .           a trailer
//! ```
//!
//! What the printer makes of a statement depends on the statement, its comments, the
//! prefixes in force and the items next to it (blank lines, the comments between them);
//! the window gives it all of these. The previous window's last item comes again (`ctx`)
//! so that the separator and the comments between it and the first new item are the
//! printer's own; its output is cut right after that item's last printed token (where the
//! previous window stopped), and the window's output ends where its last item's does,
//! before the trailer. The prefixes in force are re-declared in their order of
//! declaration, each relative namespace under the base it was declared under, so that
//! IRI compaction, `a` and the numeric shorthands see the same scope. The barrier keeps a
//! context directive block from joining the synthetic prologue (only the document's
//! first directive block is sorted), and its trailing comment keeps a comment that starts
//! the context item's line from trailing the barrier. The document's file header and
//! leading directive block are in the first window, which is the document's own
//! beginning. A graph block that does not fit a window is resumed with a synthetic
//! `GRAPH g {` (unless an ignore pragma may keep it as written: then it is never split).
//!
//! `prune-prefixes` needs facts about the whole document: whether a declaration is used
//! anywhere in its scope (as printed: compacted IRIs count, `a` and dropped datatypes do
//! not), and whether a label is bound to different namespaces anywhere. A first pass
//! over the same windows collects them ([`crate::normalize::prune`] on each window), and
//! the formatting pass hands them to the printer: a trailer that uses every prefix whose
//! declaration is used after the window, and a synthetic second binding of each label
//! bound twice in the document. A window never ends right after a directive block then,
//! so a block that prints nothing always has its neighbors in the same window.
//!
//! **Safety checks.** Each window goes through [`crate::check::run`] (comments,
//! idempotence), and its output slice through two checks over the whole document's
//! sequence of slices:
//! - *Graph.* The input slice (after the context item, up to the window's last item) and
//!   the output slice are parsed with oxttl after a prologue of the prefixes and base in
//!   force *in that text*: the input's for the input, the ones the output declared so
//!   far for the output. Their quads must be isomorphic with every labeled blank node
//!   fixed (labels are document-scoped and printed as written; only anonymous nodes may
//!   be renamed). The input slices partition the document's statements and the output
//!   slices the output's, a statement's quads depend only on the prefixes and base in
//!   force, and anonymous nodes of different slices are different nodes, so the union of
//!   the per-slice isomorphisms is an isomorphism of the whole input and output: the
//!   whole-document graph check holds. Directives make no quads; the output's directives
//!   are checked through the statements after them, parsed under the prefixes the output
//!   declared.
//! - *Comments.* The comment texts of all input slices and all output slices are counted
//!   as one multiset, which must be empty at the end: the whole-document comment check.
//!
//! The per-window idempotence check covers each statement in the same surroundings the
//! whole output gives it, which is what formatting the output again sees. A window's
//! output after the context item must start with what the previous window printed for
//! it (the same bytes after its last printed token); anything else refuses the output.

use super::print;
use crate::check::graph::{self, RDF_BASE};
use crate::check::{self, LangImpl};
use crate::doc::Printed;
use crate::lex::{self, LexMode, Token, TokenKind};
use crate::lines::assemble::io_error;
use crate::normalize::{PrefixScope, prune};
use crate::syntax::NodeKind;
use crate::tree::{NodeId, TokenId, Tree};
use crate::trivia::{CommentRules, Comments};
use crate::{Check, FormatError, Language, LineStats, Options, Warning};
use oxrdf::{BlankNode, GraphName, NamedNode, NamedOrBlankNode, Quad, Term, Triple};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, Write};
use std::ops::Range;
use std::rc::Rc;

/// What streaming needs beyond the [`Options`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamConfig {
    /// bytes of input formatted together (more when one statement is larger)
    pub window_bytes: usize,
    /// the largest statement, directive block or unsplittable graph block held in
    /// memory; beyond it the input is refused with [`FormatError::TooLarge`]
    pub max_statement_bytes: u64,
    /// bytes read from the input at a time
    pub read_bytes: usize,
}

impl Default for StreamConfig {
    fn default() -> StreamConfig {
        StreamConfig {
            window_bytes: 64 << 10,
            max_statement_bytes: 256 << 20,
            read_bytes: 64 << 10,
        }
    }
}

/// Whether `opts` let Turtle and TriG stream: everything but `sort`, which orders the
/// whole document.
pub fn can_stream(opts: &Options) -> bool {
    !(opts.sort && super::sort::IMPLEMENTED)
}

/// Whether streaming with `opts` reads the input twice (`prune-prefixes`).
pub fn reads_twice(opts: &Options) -> bool {
    opts.prune_prefixes && prune::IMPLEMENTED
}

/// Format Turtle or TriG from `open()` to `w` in bounded memory: the output is
/// [`crate::format`]'s. `open` is called once, or twice with `prune-prefixes`
/// ([`reads_twice`]). Output is written as each window passes its checks: after an
/// error, what was written is only the start of an output that does not exist, so the
/// CLI writes to a temporary file and keeps it only on success.
pub fn format_stream<R: BufRead>(
    mut open: impl FnMut() -> std::io::Result<R>,
    w: impl Write,
    lang: Language,
    opts: &Options,
    cfg: &StreamConfig,
) -> Result<LineStats, FormatError> {
    let trig = match lang {
        Language::Turtle => false,
        Language::TriG => true,
        _ => return Err(FormatError::unsupported_language(lang)),
    };
    if !can_stream(opts) {
        return Err(FormatError::UnsupportedLanguage {
            language: lang.name().to_string(),
            message: format!(
                "sorted {} is formatted in memory: it cannot stream",
                lang.display_name()
            ),
        });
    }
    let mut opts = crate::clamped(opts);
    opts.cursor = None;
    let cfg = StreamConfig {
        window_bytes: cfg.window_bytes.max(1),
        read_bytes: cfg.read_bytes.max(1),
        ..cfg.clone()
    };
    let mut facts = None;
    if reads_twice(&opts) {
        let mut a = Analysis {
            trig,
            opts: &opts,
            used: HashSet::new(),
        };
        let scanned = drive(open().map_err(io_error)?, trig, true, &cfg, &mut a)?;
        if scanned.any_statement {
            let used = a.used;
            facts = Some(Facts {
                used,
                conflicts: conflicts(&scanned.bindings),
            });
        } else {
            // a document of directives alone is printed as written
            opts.prune_prefixes = false;
        }
    }
    let mut f = Formatter {
        trig,
        lang,
        opts: &opts,
        w,
        facts: facts.as_ref(),
        out_state: Prologue::default(),
        tail: String::new(),
        comments: HashMap::new(),
        changed: false,
        warnings: Vec::new(),
        seen: HashSet::new(),
    };
    let prune_mode = opts.prune_prefixes;
    let scanned = drive(open().map_err(io_error)?, trig, prune_mode, &cfg, &mut f)?;
    f.w.flush().map_err(io_error)?;
    if f.comments.values().any(|&n| n != 0) {
        return Err(FormatError::Unsafe {
            check: Check::Comments,
        });
    }
    Ok(LineStats {
        statements: scanned.statements,
        changed: f.changed && !scanned.verbatim,
        warnings: f.warnings,
    })
}

// ------------------------------------------------------------------ the input ------

/// The part of the input still needed: from the next window's start to what was read.
struct Buffer {
    bytes: Vec<u8>,
    /// the input offset of `bytes[0]`
    base: u64,
    eof: bool,
    /// line breaks before `base`, and characters between the last one and `base`
    line: u64,
    col: u64,
}

impl Buffer {
    fn end(&self) -> u64 {
        self.base + self.bytes.len() as u64
    }

    /// The end of the complete lines read (at the end of the input: the end).
    fn lines_end(&self) -> u64 {
        if self.eof {
            return self.end();
        }
        let n = self
            .bytes
            .iter()
            .rposition(|&b| b == b'\n' || b == b'\r')
            .map_or(0, |i| i + 1);
        self.base + n as u64
    }

    fn bytes(&self, r: Range<u64>) -> &[u8] {
        &self.bytes[(r.start - self.base) as usize..(r.end - self.base) as usize]
    }

    /// The text of a range the scanner has read (valid UTF-8, on character boundaries).
    fn text(&self, r: Range<u64>) -> &str {
        std::str::from_utf8(self.bytes(r)).expect("scanned text is UTF-8")
    }

    /// Forget everything before `to`.
    fn discard(&mut self, to: u64) {
        let n = (to.max(self.base) - self.base) as usize;
        if n == 0 {
            return;
        }
        let gone = &self.bytes[..n];
        let skip = match self.base {
            0 if gone.starts_with(b"\xEF\xBB\xBF") => 3,
            _ => 0,
        };
        match gone.iter().rposition(|&b| b == b'\n') {
            Some(i) => {
                self.line += gone.iter().filter(|&&b| b == b'\n').count() as u64;
                self.col = chars(&gone[i + 1..]);
            }
            None => self.col += chars(&gone[skip..]),
        }
        self.bytes.drain(..n);
        self.base = to;
    }

    /// The 1-based line and column (Unicode scalar values; a BOM is no column) of input
    /// offset `at`, which is still in the buffer.
    fn position(&self, at: u64) -> (u32, u32) {
        let at = at.clamp(self.base, self.end());
        let mut before = &self.bytes[..(at - self.base) as usize];
        if self.base == 0 && before.starts_with(b"\xEF\xBB\xBF") {
            before = &before[3..];
        }
        let (line, col) = match before.iter().rposition(|&b| b == b'\n') {
            Some(i) => (
                self.line + before.iter().filter(|&&b| b == b'\n').count() as u64,
                chars(&before[i + 1..]),
            ),
            None => (self.line, self.col + chars(before)),
        };
        let clamp = |n: u64| (n + 1).min(u64::from(u32::MAX)) as u32;
        (clamp(line), clamp(col))
    }
}

/// Characters in UTF-8 bytes (continuation bytes do not count).
fn chars(b: &[u8]) -> u64 {
    b.iter().filter(|&&c| (c & 0xC0) != 0x80).count() as u64
}

/// Read up to `n` more bytes.
fn fill(r: &mut impl BufRead, buf: &mut Buffer, n: usize) -> Result<(), FormatError> {
    loop {
        match r.fill_buf() {
            Ok([]) => {
                buf.eof = true;
                return Ok(());
            }
            Ok(b) => {
                let k = b.len().min(n);
                buf.bytes.extend_from_slice(&b[..k]);
                r.consume(k);
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(io_error(e)),
        }
    }
}

// ---------------------------------------------------------------- the prologue ------

/// The prefixes and base in force: what a window re-declares.
#[derive(Clone, Debug, Default)]
struct Prologue {
    /// in the order of their latest declaration
    prefixes: Vec<Prefix>,
    /// the base in force, resolved (`None`: the synthetic base of the reference parse)
    base: Option<Rc<str>>,
}

#[derive(Clone, Debug)]
struct Prefix {
    label: String,
    /// as written, `<` `>` included
    iri: String,
    /// the base it was declared under
    base: Option<Rc<str>>,
    /// the declaration's number in the document
    id: u64,
}

impl Prologue {
    fn declare(&mut self, label: &str, iri: &str, id: u64) {
        self.prefixes.retain(|p| p.label != label);
        self.prefixes.push(Prefix {
            label: label.to_string(),
            iri: iri.to_string(),
            base: self.base.clone(),
            id,
        });
    }

    fn rebase(&mut self, written: &str) {
        self.base = resolve(self.base.as_deref(), written);
    }

    /// Apply one directive's significant tokens (`PREFIX` / `@prefix`, `BASE` /
    /// `@base`; anything else, a `VERSION` or a malformed directive, changes nothing).
    fn apply(&mut self, toks: &[(TokenKind, &str)], id: u64) -> Option<(String, String)> {
        let kw = toks.first()?.1.trim_start_matches('@').to_ascii_lowercase();
        match (kw.as_str(), toks.get(1), toks.get(2)) {
            ("prefix", Some(&(TokenKind::PnameNs, label)), Some(&(TokenKind::IriRef, iri))) => {
                let label = label.strip_suffix(':').unwrap_or(label);
                self.declare(label, iri, id);
                Some((label.to_string(), iri[1..iri.len() - 1].to_string()))
            }
            ("base", Some(&(TokenKind::IriRef, iri)), _) => {
                self.rebase(iri);
                None
            }
            _ => None,
        }
    }

    /// Whether both declare the same prefixes (as written, under the same bases) and
    /// base.
    fn same(&self, other: &Prologue) -> bool {
        self.base == other.base
            && self.prefixes.len() == other.prefixes.len()
            && self.prefixes.iter().all(|p| {
                other
                    .prefixes
                    .iter()
                    .any(|q| q.label == p.label && q.iri == p.iri && q.base == p.base)
            })
    }

    /// The directives, one per line, a relative namespace after the base it was declared
    /// under; `extra` first (synthetic declarations).
    fn write(&self, out: &mut String, extra: &[(String, String)]) {
        for (label, iri) in extra {
            out.push_str(&format!("PREFIX {label}: {iri}\n"));
        }
        let mut written: Option<Rc<str>> = None;
        let base = |out: &mut String, b: &Option<Rc<str>>| {
            out.push_str("BASE ");
            out.push_str(&iri_ref(b.as_deref().unwrap_or(RDF_BASE)));
            out.push('\n');
        };
        for p in &self.prefixes {
            if !absolute(&p.iri) && p.base != written {
                base(out, &p.base);
                written = p.base.clone();
            }
            out.push_str(&format!("PREFIX {}: {}\n", p.label, p.iri));
        }
        if self.base != written {
            base(out, &self.base);
        }
    }
}

/// An IRI written as `<…>` with an absolute scheme and no escapes: no base applies.
fn absolute(iri: &str) -> bool {
    let inner = iri.trim_start_matches('<');
    let Some(colon) = inner.find(':') else {
        return false;
    };
    let scheme = &inner[..colon];
    !inner.contains('\\')
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// `written` (`<…>`) resolved against `base` by the reference parser itself.
fn resolve(base: Option<&str>, written: &str) -> Option<Rc<str>> {
    let text = format!("<urn:s> <urn:p> {written} .");
    let t = oxttl::TurtleParser::new()
        .with_base_iri(base.unwrap_or(RDF_BASE))
        .ok()?
        .for_slice(&text)
        .next()?
        .ok()?;
    match t.object {
        Term::NamedNode(n) => Some(n.into_string().into()),
        _ => None,
    }
}

/// `<iri>` with what `IRIREF` does not allow escaped.
fn iri_ref(iri: &str) -> String {
    let mut s = String::with_capacity(iri.len() + 2);
    s.push('<');
    for c in iri.chars() {
        if c <= ' ' || "<>\"{}|^`\\".contains(c) {
            s.push_str(&format!("\\u{:04X}", c as u32));
        } else {
            s.push(c);
        }
    }
    s.push('>');
    s
}

// ----------------------------------------------------------------- the scanner ------

/// What an item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// consecutive directives
    Directives,
    /// a statement outside graph blocks
    Statement,
    /// a TriG graph block
    Block,
    /// a statement in a graph block (ending with its `.`)
    Inner,
}

/// A graph block.
#[derive(Debug)]
struct Block {
    /// `GRAPH label {` (or `{`), to resume the block in a window of its own
    opener: String,
    /// a window ended inside it
    split: Cell<bool>,
    /// it may be under an ignore pragma (printed as written): never split
    atomic: bool,
}

/// The end of an item: where a window may end.
#[derive(Clone, Debug)]
struct Cut {
    kind: Kind,
    /// the item's first and last significant tokens
    start: u64,
    end: u64,
    /// the end of the item before it at its level (or the level's start)
    gap: u64,
    /// `gap` ends an item, which a comment on its line may trail
    after_item: bool,
    /// the block of an inner statement, or the block itself
    block: Option<Rc<Block>>,
    /// the document's first top-level item
    first: bool,
    /// the prologue before and after the item
    before: Rc<Prologue>,
    after: Rc<Prologue>,
    /// the prefix declarations of a directive block: where their IRI starts, their id
    decls: Vec<(u64, u64)>,
}

impl Cut {
    /// The graph block an inner statement is in.
    fn inner(&self) -> Option<&Block> {
        match self.kind {
            Kind::Inner => self.block.as_deref(),
            _ => None,
        }
    }

    /// Whether a window may end after this item.
    fn allowed(&self, prune: bool) -> bool {
        match self.kind {
            // the leading block is sorted as the document's first; with prune-prefixes
            // a block may print nothing, which needs its neighbors in the window
            Kind::Directives => !self.first && !prune,
            Kind::Statement => true,
            Kind::Block => !self.block.as_ref().is_some_and(|b| b.split.get()),
            Kind::Inner => !self.block.as_ref().is_some_and(|b| b.atomic),
        }
    }
}

/// Where the scanner is between significant tokens at the top level.
#[derive(Debug)]
enum Top {
    Idle,
    /// in a directive: whether it is an `@` form, its significant tokens
    Directive {
        at_form: bool,
        toks: Vec<(TokenKind, Range<u64>)>,
    },
    /// after a directive: the run goes on if another one follows
    Run,
    /// in a statement: bracket depth, significant tokens, the first two
    Statement {
        depth: u32,
        sig: u32,
        first: Vec<(TokenKind, Range<u64>)>,
    },
    /// after `GRAPH`: the label's tokens
    Head {
        label: Vec<Range<u64>>,
    },
}

/// A directive run being scanned.
#[derive(Debug)]
struct Run {
    start: u64,
    end: u64,
    gap: u64,
    after_item: bool,
    first: bool,
    before: Rc<Prologue>,
    decls: Vec<(u64, u64)>,
}

/// A graph block being scanned.
#[derive(Debug)]
struct Open {
    info: Rc<Block>,
    start: u64,
    gap: u64,
    after_item: bool,
    first: bool,
    /// the bracket depth in the current statement (`None` between statements)
    depth: Option<u32>,
    inner_start: u64,
    inner_gap: u64,
    inner_after: bool,
}

struct Scanner {
    trig: bool,
    /// what a window may end after
    prune: bool,
    /// where lexing resumes
    pos: u64,
    top: Top,
    item_start: u64,
    /// the current item's leading comments may end with an ignore pragma
    item_pragma: bool,
    gap: u64,
    after_item: bool,
    run: Option<Run>,
    block: Option<Open>,
    state: Rc<Prologue>,
    next_id: u64,
    /// the last comment between items is an ignore pragma
    pragma: bool,
    /// top-level items so far
    items: u64,
    statements: u64,
    any_statement: bool,
    /// a significant token was seen (the file header is over)
    started: bool,
    /// the file header holds `# sparkles-fmt: ignore-file`
    ignore_file: bool,
    /// every namespace each label is bound to
    bindings: HashMap<String, HashSet<String>>,
    cuts: VecDeque<Cut>,
    /// a token no Turtle or TriG document holds was seen (a malformed IRI lexes as
    /// `<` and words): where items end is unknown from there, so the rest is one item,
    /// and the reference parser reports the error
    poisoned: bool,
}

/// What the scanner saw of the whole input.
struct Scanned {
    statements: u64,
    any_statement: bool,
    bindings: HashMap<String, HashSet<String>>,
    /// copied as written (`ignore-file`)
    verbatim: bool,
}

fn depth_delta(k: TokenKind) -> i32 {
    use TokenKind as T;
    match k {
        T::LBracket | T::LParen | T::LBrace | T::LtLt | T::LtLtParen | T::LBracePipe => 1,
        T::RBracket | T::RParen | T::RBrace | T::GtGt | T::ParenGtGt | T::PipeRBrace => -1,
        _ => 0,
    }
}

fn step(depth: u32, k: TokenKind) -> u32 {
    (depth as i32 + depth_delta(k)).max(0) as u32
}

/// Whether a significant token can be part of a Turtle or TriG document: SPARQL's
/// variables and operators, a lone `<` (a malformed IRI) and words other than the
/// keywords cannot.
fn turtle_token(k: TokenKind, text: &str) -> bool {
    use TokenKind as T;
    match k {
        T::Word => ["a", "true", "false", "prefix", "base", "version", "graph"]
            .iter()
            .any(|w| text.eq_ignore_ascii_case(w)),
        T::Var1
        | T::Var2
        | T::Pipe
        | T::OrOr
        | T::AndAnd
        | T::Bang
        | T::NotEq
        | T::Eq
        | T::Lt
        | T::Le
        | T::Gt
        | T::Ge
        | T::Plus
        | T::Minus
        | T::Star
        | T::Slash
        | T::Hat
        | T::Question
        | T::Colon
        | T::Unknown => false,
        _ => true,
    }
}

/// A graph label: an IRI or a blank node.
fn is_label(k: TokenKind) -> bool {
    use TokenKind as T;
    matches!(
        k,
        T::IriRef | T::PnameLn | T::PnameNs | T::BlankNodeLabel | T::Anon
    )
}

impl Scanner {
    fn new(trig: bool, prune: bool, bom: u64) -> Scanner {
        Scanner {
            trig,
            prune,
            pos: 0,
            top: Top::Idle,
            item_start: 0,
            item_pragma: false,
            gap: bom,
            after_item: false,
            run: None,
            block: None,
            state: Rc::new(Prologue::default()),
            next_id: 0,
            pragma: false,
            items: 0,
            statements: 0,
            any_statement: false,
            started: false,
            ignore_file: false,
            bindings: HashMap::new(),
            cuts: VecDeque::new(),
            poisoned: false,
        }
    }

    fn push(&mut self, cut: Cut) {
        if !self.poisoned {
            self.cuts.push_back(cut);
        }
    }

    /// Lex the complete lines read since the last call and push the items that end in
    /// them. Stops before a token more input may change (an unterminated string, `[` or
    /// `(` with only whitespace after it).
    fn scan(&mut self, buf: &Buffer) -> Result<(), FormatError> {
        let end = buf.lines_end();
        if self.pos >= end {
            return Ok(());
        }
        let from = self.pos;
        let bytes = buf.bytes(from..end);
        let text = std::str::from_utf8(bytes).map_err(|e| {
            let at = from + e.valid_up_to() as u64;
            let (line, column) = buf.position(at);
            FormatError::Syntax {
                message: "the input is not valid UTF-8".into(),
                line,
                column,
                offset: at as usize,
            }
        })?;
        let tokens = lex::lex(text, LexMode::Turtle);
        for (i, t) in tokens.iter().enumerate() {
            if t.kind == TokenKind::Eof {
                break;
            }
            let at = from + u64::from(t.start);
            let r = at..at + u64::from(t.len);
            if !buf.eof && self.unsure(text, &tokens, i) {
                self.pos = at;
                return Ok(());
            }
            match t.kind {
                TokenKind::Whitespace => {}
                TokenKind::Comment => self.comment(t.text(text)),
                k => self.significant(buf, k, r)?,
            }
        }
        self.pos = end;
        Ok(())
    }

    /// Whether token `i` may lex differently once more input is read.
    fn unsure(&self, text: &str, tokens: &[Token], i: usize) -> bool {
        let t = tokens[i];
        match t.kind {
            TokenKind::Unknown => t.text(text).starts_with(['"', '\'']),
            TokenKind::LBracket | TokenKind::LParen => tokens[i + 1..]
                .iter()
                .all(|t| matches!(t.kind, TokenKind::Whitespace | TokenKind::Eof)),
            _ => false,
        }
    }

    fn comment(&mut self, text: &str) {
        if !self.started {
            self.ignore_file |= crate::pragma::is_ignore_file(text);
        }
        let between = match &self.block {
            Some(b) => b.depth.is_none(),
            None => matches!(self.top, Top::Idle | Top::Run),
        };
        if between {
            self.pragma = crate::pragma::is_ignore(text);
        }
    }

    fn significant(
        &mut self,
        buf: &Buffer,
        k: TokenKind,
        r: Range<u64>,
    ) -> Result<(), FormatError> {
        self.started = true;
        self.poisoned |= !turtle_token(k, buf.text(r.clone()));
        if self.block.is_some() {
            self.inner(k, r);
            return Ok(());
        }
        let text = |r: &Range<u64>| buf.text(r.clone());
        match &mut self.top {
            Top::Idle | Top::Run => {
                let directive = match k {
                    TokenKind::LangDir => {
                        matches!(text(&r), "@prefix" | "@base" | "@version").then_some(true)
                    }
                    TokenKind::Word => ["prefix", "base", "version"]
                        .iter()
                        .any(|w| text(&r).eq_ignore_ascii_case(w))
                        .then_some(false),
                    _ => None,
                };
                if let Some(at_form) = directive {
                    if self.run.is_none() {
                        self.run = Some(Run {
                            start: r.start,
                            end: r.end,
                            gap: self.gap,
                            after_item: self.after_item,
                            first: self.items == 0,
                            before: self.state.clone(),
                            decls: Vec::new(),
                        });
                    }
                    self.pragma = false;
                    self.top = Top::Directive {
                        at_form,
                        toks: vec![(k, r)],
                    };
                    return Ok(());
                }
                self.finish_run();
                self.item_start = r.start;
                self.item_pragma = std::mem::take(&mut self.pragma);
                let pragma = self.item_pragma;
                if self.trig && k == TokenKind::Word && text(&r).eq_ignore_ascii_case("graph") {
                    self.top = Top::Head { label: Vec::new() };
                } else if self.trig && k == TokenKind::LBrace {
                    self.open_block("{".to_string(), r.start, r.end, pragma);
                } else {
                    self.top = Top::Statement {
                        depth: step(0, k),
                        sig: 1,
                        first: vec![(k, r.clone())],
                    };
                    if k == TokenKind::Dot {
                        self.finish_statement(r.end);
                    }
                }
            }
            Top::Directive { at_form, toks } => {
                toks.push((k, r.clone()));
                let n = toks.len();
                let kw = text(&toks[0].1)
                    .trim_start_matches('@')
                    .to_ascii_lowercase();
                let done = match *at_form {
                    true => k == TokenKind::Dot,
                    false => n == if kw == "prefix" { 3 } else { 2 },
                };
                if done {
                    let toks = std::mem::take(toks);
                    let words: Vec<(TokenKind, &str)> =
                        toks.iter().map(|(k, r)| (*k, text(r))).collect();
                    let id = self.next_id;
                    let state = Rc::make_mut(&mut self.state);
                    if let Some((label, iri)) = state.apply(&words, id) {
                        self.next_id += 1;
                        self.bindings.entry(label).or_default().insert(iri);
                        let run = self.run.as_mut().expect("a directive run");
                        run.decls.push((toks[2].1.start, id));
                    }
                    self.run.as_mut().expect("a directive run").end = r.end;
                    self.top = Top::Run;
                }
            }
            Top::Statement { depth, sig, first } => {
                let label_like = match first.as_slice() {
                    [(k0, _)] => *sig == 1 && is_label(*k0),
                    [(TokenKind::LBracket, _), (TokenKind::RBracket, _)] => *sig == 2,
                    _ => false,
                };
                if self.trig && *depth == 0 && k == TokenKind::LBrace && label_like {
                    let label: String = first.iter().map(|(_, r)| text(r)).collect();
                    let (start, pragma) = (self.item_start, self.item_pragma);
                    self.open_block(format!("GRAPH {label} {{"), start, r.end, pragma);
                    return Ok(());
                }
                *depth = step(*depth, k);
                *sig += 1;
                if first.len() < 2 {
                    first.push((k, r.clone()));
                }
                if k == TokenKind::Dot && *depth == 0 {
                    self.finish_statement(r.end);
                }
            }
            Top::Head { label } => {
                if k == TokenKind::LBrace {
                    let label: String = label.iter().map(text).collect();
                    let (start, pragma) = (self.item_start, self.item_pragma);
                    self.open_block(format!("GRAPH {label} {{"), start, r.end, pragma);
                } else {
                    label.push(r);
                }
            }
        }
        Ok(())
    }

    fn open_block(&mut self, opener: String, start: u64, brace_end: u64, pragma: bool) {
        self.block = Some(Open {
            info: Rc::new(Block {
                opener,
                split: Cell::new(false),
                atomic: pragma,
            }),
            start,
            gap: self.gap,
            after_item: self.after_item,
            first: self.items == 0,
            depth: None,
            inner_start: 0,
            inner_gap: brace_end,
            inner_after: false,
        });
        self.top = Top::Idle;
        self.any_statement = true;
    }

    /// A significant token inside a graph block.
    fn inner(&mut self, k: TokenKind, r: Range<u64>) {
        let b = self.block.as_mut().expect("in a block");
        match b.depth {
            None if k == TokenKind::RBrace => self.close_block(r.end),
            None => {
                self.pragma = false;
                b.inner_start = r.start;
                b.depth = Some(step(0, k));
                if k == TokenKind::Dot {
                    self.finish_inner(r.end);
                }
            }
            Some(0) if k == TokenKind::RBrace => {
                // the last statement, without its `.`
                self.statements += 1;
                self.close_block(r.end);
            }
            Some(d) => {
                let d = step(d, k);
                b.depth = Some(d);
                if k == TokenKind::Dot && d == 0 {
                    self.finish_inner(r.end);
                }
            }
        }
    }

    fn finish_inner(&mut self, end: u64) {
        let b = self.block.as_mut().expect("in a block");
        let cut = Cut {
            kind: Kind::Inner,
            start: b.inner_start,
            end,
            gap: b.inner_gap,
            after_item: b.inner_after,
            block: Some(b.info.clone()),
            first: false,
            before: self.state.clone(),
            after: self.state.clone(),
            decls: Vec::new(),
        };
        b.depth = None;
        b.inner_gap = end;
        b.inner_after = true;
        self.statements += 1;
        self.push(cut);
    }

    fn close_block(&mut self, end: u64) {
        let b = self.block.take().expect("in a block");
        self.push(Cut {
            kind: Kind::Block,
            start: b.start,
            end,
            gap: b.gap,
            after_item: b.after_item,
            block: Some(b.info),
            first: b.first,
            before: self.state.clone(),
            after: self.state.clone(),
            decls: Vec::new(),
        });
        self.items += 1;
        self.gap = end;
        self.after_item = true;
        self.top = Top::Idle;
    }

    fn finish_statement(&mut self, end: u64) {
        self.statements += 1;
        self.any_statement = true;
        self.push(Cut {
            kind: Kind::Statement,
            start: self.item_start,
            end,
            gap: self.gap,
            after_item: self.after_item,
            block: None,
            first: self.items == 0,
            before: self.state.clone(),
            after: self.state.clone(),
            decls: Vec::new(),
        });
        self.items += 1;
        self.gap = end;
        self.after_item = true;
        self.top = Top::Idle;
    }

    /// End a directive run (the next item is no directive, or the input ended).
    fn finish_run(&mut self) {
        if !matches!(self.top, Top::Run) {
            return;
        }
        let run = self.run.take().expect("a directive run");
        self.push(Cut {
            kind: Kind::Directives,
            start: run.start,
            end: run.end,
            gap: run.gap,
            after_item: run.after_item,
            block: None,
            first: run.first,
            before: run.before,
            after: self.state.clone(),
            decls: run.decls,
        });
        self.items += 1;
        self.gap = run.end;
        self.after_item = true;
        self.top = Top::Idle;
    }

    /// The first cut a window may end at with at least `budget` bytes after `from`.
    fn pick(&self, from: u64, budget: usize) -> Option<usize> {
        self.cuts
            .iter()
            .position(|c| c.allowed(self.prune) && c.end - from >= budget as u64)
    }
}

// ------------------------------------------------------------------ the windows ------

/// One window: the previous window's last item, the new items, where its text starts.
struct Window<'a> {
    ctx: Option<&'a Cut>,
    new: &'a [Cut],
    /// where the window's text starts in the input
    g: u64,
    /// where it ends (`None`: the end of the input)
    end: Option<u64>,
}

impl Window<'_> {
    fn last(&self) -> Option<&Cut> {
        match self.end {
            Some(_) => self.new.last(),
            None => None,
        }
    }

    fn end(&self, buf: &Buffer) -> u64 {
        self.end.unwrap_or(buf.end())
    }

    /// Where the input slice of the window's own items starts: after the context item.
    fn from(&self) -> u64 {
        self.ctx.map_or(0, |c| c.end)
    }

    /// The same-line comment after the gap of the last item, which may trail the item
    /// before it.
    fn c0(&self, buf: &Buffer) -> Option<Range<u64>> {
        let last = self.last()?;
        if !last.after_item {
            return None;
        }
        let gap = buf.bytes(last.gap..last.start);
        let at = gap.iter().position(|&b| b != b' ' && b != b'\t')?;
        if gap[at] != b'#' {
            return None;
        }
        let len = gap[at..]
            .iter()
            .position(|&b| b == b'\n' || b == b'\r')
            .unwrap_or(gap.len() - at);
        let start = last.gap + at as u64;
        Some(start..start + len as u64)
    }
}

/// The barrier between a window's prologue and its context item.
const BARRIER: &str = "[] <#b> <#b> # b\n. ";

/// What a pass does with each window.
trait Pass {
    /// Handle a window; whether the comment [`Window::c0`] trails the item before.
    fn window(&mut self, buf: &Buffer, w: &Window<'_>) -> Result<bool, FormatError>;

    /// Copy input as it is (`ignore-file`); whether to go on.
    fn verbatim(&mut self, bytes: &[u8]) -> Result<bool, FormatError>;
}

/// The window texts of both passes.
struct Texts {
    /// what the printer formats
    window: String,
    /// where the input starts in `window`
    head: usize,
    /// where the input ends in `window`
    body_end: usize,
}

/// The text of window `w`: its prologue and barrier (the opener of a resumed graph
/// block), the input from `w.g`, then the closer of a graph block left open and the
/// trailer using the prefixes `uses`. The second bindings of the labels bound twice
/// (`conflicts`) open the prologue, or follow the trailer in the first window.
fn window_text(
    buf: &Buffer,
    w: &Window<'_>,
    conflicts: &[(String, String)],
    uses: &[String],
) -> Texts {
    let end = w.end(buf);
    let mut text = String::with_capacity((end - w.g) as usize + 512);
    let mut trailing_conflicts = conflicts;
    if let Some(ctx) = w.ctx {
        ctx.before.write(&mut text, conflicts);
        trailing_conflicts = &[];
        text.push_str(BARRIER);
        if let Some(b) = ctx.inner() {
            text.push_str(&b.opener);
            text.push(' ');
        }
    }
    let head = text.len();
    text.push_str(buf.text(w.g..end));
    let body_end = text.len();
    if let Some(last) = w.last() {
        if last.kind == Kind::Inner {
            text.push_str("\n}");
        }
        text.push_str("\n<#b> <#b> ");
        match uses.is_empty() {
            true => text.push_str("<#b>"),
            false => text.push_str(&uses.join(", ")),
        }
        text.push_str(" .\n");
        for (label, iri) in trailing_conflicts {
            text.push_str(&format!("PREFIX {label}: {iri}\n"));
        }
    }
    Texts {
        window: text,
        head,
        body_end,
    }
}

/// Read, scan and hand windows to `pass`.
fn drive(
    mut r: impl BufRead,
    trig: bool,
    prune: bool,
    cfg: &StreamConfig,
    pass: &mut dyn Pass,
) -> Result<Scanned, FormatError> {
    let mut buf = Buffer {
        bytes: Vec::new(),
        base: 0,
        eof: false,
        line: 0,
        col: 0,
    };
    // the BOM is no part of the text after it
    while buf.bytes.len() < 3 && !buf.eof {
        fill(&mut r, &mut buf, cfg.read_bytes)?;
    }
    let bom = match buf.bytes.starts_with(b"\xEF\xBB\xBF") {
        true => 3,
        false => 0,
    };
    let mut sc = Scanner::new(trig, prune, bom);
    let mut ctx: Option<Cut> = None;
    let mut g = 0;
    loop {
        sc.scan(&buf)?;
        if sc.ignore_file && (sc.started || buf.eof) {
            // as written
            let mut go = pass.verbatim(&buf.bytes)?;
            while go && !buf.eof {
                buf.bytes.clear();
                fill(&mut r, &mut buf, cfg.read_bytes.max(64 << 10))?;
                go = pass.verbatim(&buf.bytes)?;
            }
            return Ok(Scanned {
                statements: 0,
                any_statement: true,
                bindings: HashMap::new(),
                verbatim: true,
            });
        }
        loop {
            let from = ctx.as_ref().map_or(0, |c| c.end);
            // past the statement limit, every item that ends goes
            let budget = match buf.end() - from > cfg.max_statement_bytes {
                true => 0,
                false => cfg.window_bytes,
            };
            let Some(k) = sc.pick(from, budget) else {
                break;
            };
            let new: Vec<Cut> = sc.cuts.drain(..=k).collect();
            // a split graph block is held a part at a time
            let whole = |c: &&Cut| {
                c.kind != Kind::Block || !c.block.as_ref().is_some_and(|b| b.split.get())
            };
            if new
                .iter()
                .filter(whole)
                .any(|c| c.end - c.start > cfg.max_statement_bytes)
            {
                return Err(FormatError::TooLarge);
            }
            let last = new.last().expect("a cut").clone();
            if last.kind == Kind::Inner
                && let Some(b) = &last.block
            {
                b.split.set(true);
            }
            let w = Window {
                ctx: ctx.as_ref(),
                new: &new,
                g,
                end: Some(last.end),
            };
            let c0 = w.c0(&buf);
            let trails = pass.window(&buf, &w)?;
            g = match (c0, trails) {
                (Some(c), true) => c.end,
                _ => last.gap,
            };
            ctx = Some(last);
            buf.discard(g);
        }
        if buf.eof && sc.pos >= buf.end() {
            break;
        }
        let from = ctx.as_ref().map_or(0, |c| c.end);
        if buf.end() - from > cfg.max_statement_bytes {
            return Err(too_large(&buf, ctx.as_ref(), g, trig));
        }
        fill(&mut r, &mut buf, cfg.read_bytes)?;
    }
    sc.finish_run();
    let new: Vec<Cut> = sc.cuts.drain(..).collect();
    let w = Window {
        ctx: ctx.as_ref(),
        new: &new,
        g,
        end: None,
    };
    pass.window(&buf, &w)?;
    Ok(Scanned {
        statements: sc.statements,
        any_statement: sc.any_statement,
        bindings: std::mem::take(&mut sc.bindings),
        verbatim: false,
    })
}

/// A statement over the size limit: the reference parser's error when it finds one
/// before the end of what was read, else [`FormatError::TooLarge`].
fn too_large(buf: &Buffer, ctx: Option<&Cut>, g: u64, trig: bool) -> FormatError {
    let from = ctx.map_or(g, |c| c.end);
    let mut text = String::new();
    if let Some(c) = ctx {
        c.after.write(&mut text, &[]);
        if let Some(b) = c.inner() {
            text.push_str(&b.opener);
            text.push(' ');
        }
    }
    let head = text.len();
    let end = buf.lines_end();
    let Ok(rest) = std::str::from_utf8(buf.bytes(from..end)) else {
        return FormatError::TooLarge;
    };
    text.push_str(rest);
    let lang = match trig {
        true => Language::TriG,
        false => Language::Turtle,
    };
    match graph::parse(&text, lang) {
        Err(FormatError::Syntax {
            message, offset, ..
        }) if offset + 64 < text.len() => {
            let at = from + offset.saturating_sub(head) as u64;
            let (line, column) = buf.position(at);
            FormatError::Syntax {
                message,
                line,
                column,
                offset: at as usize,
            }
        }
        _ => FormatError::TooLarge,
    }
}

/// The labels bound to different namespaces, each with a synthetic namespace none of
/// them uses (not plain, so it compacts nothing).
fn conflicts(bindings: &HashMap<String, HashSet<String>>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = bindings
        .iter()
        .filter(|(_, iris)| iris.len() > 1)
        .map(|(label, iris)| {
            let iri = (0..)
                .map(|n| format!("urn:x-sparkles-fmt:./{n}"))
                .find(|i| !iris.contains(i))
                .expect("a free namespace");
            (label.clone(), format!("<{iri}>"))
        })
        .collect();
    out.sort();
    out
}

// ------------------------------------------------------------- the first pass ------

/// The global facts `prune-prefixes` needs.
struct Facts {
    /// the declarations used in their scope
    used: HashSet<u64>,
    /// the labels bound to different namespaces, with a synthetic namespace each
    conflicts: Vec<(String, String)>,
}

/// The first pass: which declarations the printed document uses.
struct Analysis<'a> {
    trig: bool,
    opts: &'a Options,
    used: HashSet<u64>,
}

impl Pass for Analysis<'_> {
    fn window(&mut self, buf: &Buffer, w: &Window<'_>) -> Result<bool, FormatError> {
        let t = window_text(buf, w, &[], &[]);
        let text = t.window.as_str();
        let tokens = lex::lex(text, LexMode::Turtle);
        let tree = match super::parse::parse(text, tokens, self.trig) {
            Ok(tree) => tree,
            // the formatting pass reports it (as the reference parser's syntax error)
            Err(_) => return Ok(false),
        };
        let comments = Comments::attach(&tree, &print::RULES);
        let scope = PrefixScope::from_tree(&tree);
        let unused =
            prune::unused_declarations_with(&tree, &scope, self.opts, |n| comments.ignored(n));
        let mut decls: HashMap<u64, u64> = HashMap::new();
        for c in w.ctx.into_iter().chain(w.new) {
            decls.extend(c.decls.iter().copied());
        }
        let prologue = w.ctx.map(|c| &c.before);
        for d in tree.child_nodes(tree.root()) {
            if tree.kind(d) != NodeKind::PrefixDecl || unused.contains(&d) {
                continue;
            }
            let (label, iri_at) = decl_parts(&tree, d);
            let id = match iri_at < t.head {
                true => prologue
                    .and_then(|p| p.prefixes.iter().find(|p| p.label == label))
                    .map(|p| p.id),
                false => decls.get(&(w.g + (iri_at - t.head) as u64)).copied(),
            };
            self.used.extend(id);
        }
        Ok(trails(&tree, &comments, w, buf, t.head))
    }

    fn verbatim(&mut self, _: &[u8]) -> Result<bool, FormatError> {
        Ok(false)
    }
}

/// A prefix declaration's label and the offset of its IRI token.
fn decl_parts(tree: &Tree<'_>, d: NodeId) -> (String, usize) {
    let (mut label, mut at) = (String::new(), 0);
    for e in tree.children(d) {
        if let crate::tree::Element::Token(t) = *e {
            match tree.token_kind(t) {
                TokenKind::PnameNs => {
                    let s = tree.token_text(t);
                    label = s.strip_suffix(':').unwrap_or(s).to_string();
                }
                TokenKind::IriRef => at = tree.token(t).start as usize,
                _ => {}
            }
        }
    }
    (label, at)
}

/// Whether the comment [`Window::c0`] trails a node of the window's tree.
fn trails(tree: &Tree<'_>, comments: &Comments, w: &Window<'_>, buf: &Buffer, head: usize) -> bool {
    let Some(c) = w.c0(buf) else {
        return false;
    };
    let at = head as u64 + (c.start - w.g);
    let Ok(i) = tree
        .tokens
        .binary_search_by_key(&at, |t| u64::from(t.start))
    else {
        return false;
    };
    let id = TokenId(i as u32);
    (0..tree.len()).any(|n| comments.trailing(NodeId(n as u32)).contains(&id))
}

// ---------------------------------------------------------- the formatting pass ------

/// The formatting pass.
struct Formatter<'a, W: Write> {
    trig: bool,
    lang: Language,
    opts: &'a Options,
    w: W,
    facts: Option<&'a Facts>,
    /// the prefixes and base the output declared so far
    out_state: Prologue,
    /// what the previous window printed after its last item's last printed token
    tail: String,
    /// comment texts: input minus output
    comments: HashMap<String, i64>,
    changed: bool,
    warnings: Vec<Warning>,
    seen: HashSet<(&'static str, u32, u32, String)>,
}

/// What the printer hook records about a window.
#[derive(Default)]
struct Stash {
    /// the reference quads of the input slice
    quads: Option<Vec<Quad>>,
    /// output offsets: the context item's and the last item's last printed tokens, the
    /// trailer's first
    ctx_last: Option<usize>,
    last_last: Option<usize>,
    trailer: Option<usize>,
    c0_trails: bool,
    /// the labels written in the window
    labels: HashSet<String>,
    /// the comments of the input slice
    comments: Vec<String>,
    printed: bool,
}

/// A window in the pipeline: oxttl parses the input slice (with its prologue) for the
/// syntax errors and the graph check, and the printer records where the items are.
struct WindowLang<'a> {
    trig: bool,
    /// the input slice after the prologue in force
    check: &'a str,
    /// window offsets: the context item, the last item, the trailer, the input slice
    ctx: Option<Range<usize>>,
    last: Option<Range<usize>>,
    trailer: Option<usize>,
    slice: Range<usize>,
    c0: Option<usize>,
    stash: RefCell<Stash>,
}

impl LangImpl for WindowLang<'_> {
    type Reference = ();

    fn language(&self) -> Language {
        match self.trig {
            false => Language::Turtle,
            true => Language::TriG,
        }
    }

    fn lex_mode(&self) -> LexMode {
        LexMode::Turtle
    }

    fn reference(&self, _text: &str, _tokens: &[Token]) -> Result<(), FormatError> {
        let quads = graph::parse(self.check, self.language())?;
        self.stash.borrow_mut().quads = Some(quads);
        Ok(())
    }

    fn warnings(&self, _r: &()) -> Vec<Warning> {
        Vec::new()
    }

    fn cst<'s>(&self, text: &'s str, tokens: Vec<Token>, _r: &()) -> Result<Tree<'s>, FormatError> {
        super::parse::parse(text, tokens, self.trig)
    }

    fn rules(&self) -> &dyn CommentRules {
        &print::RULES
    }

    fn print(
        &self,
        tree: &Tree<'_>,
        comments: &Comments,
        opts: &Options,
    ) -> Result<Printed, FormatError> {
        let printed = print::print(tree, comments, opts, self.trig)?;
        let mut s = self.stash.borrow_mut();
        if s.printed {
            return Ok(printed);
        }
        s.printed = true;
        let within =
            |r: &Option<Range<usize>>, at: usize| r.as_ref().is_some_and(|r| r.contains(&at));
        for &(id, out, _) in &printed.tok_out {
            let at = tree.tokens[id.0 as usize].start as usize;
            let out = out as usize;
            if within(&self.ctx, at) {
                s.ctx_last = s.ctx_last.max(Some(out));
            }
            if within(&self.last, at) {
                s.last_last = s.last_last.max(Some(out));
            }
            if self.trailer.is_some_and(|t| at >= t) {
                s.trailer = Some(s.trailer.map_or(out, |t| t.min(out)));
            }
        }
        if let Some(c) = self.c0
            && let Ok(i) = tree.tokens.binary_search_by_key(&(c as u32), |t| t.start)
        {
            let id = TokenId(i as u32);
            s.c0_trails =
                (0..tree.len()).any(|n| comments.trailing(NodeId(n as u32)).contains(&id));
        }
        for t in &tree.tokens {
            match t.kind {
                TokenKind::BlankNodeLabel => {
                    s.labels.insert(t.text(tree.src)[2..].to_string());
                }
                TokenKind::Comment if self.slice.contains(&(t.start as usize)) => {
                    s.comments.push(t.text(tree.src).trim_end().to_string());
                }
                _ => {}
            }
        }
        Ok(printed)
    }

    fn equivalent(&self, _r: &(), _output: &str) -> Result<(), FormatError> {
        // checked on the output slice, after the window ([`Formatter::check_graph`])
        Ok(())
    }
}

impl<W: Write> Pass for Formatter<'_, W> {
    fn window(&mut self, buf: &Buffer, w: &Window<'_>) -> Result<bool, FormatError> {
        check::deadline(self.opts.deadline)?;
        let end = w.end(buf);
        let last = w.last();
        // the prefixes used after the window, and the labels bound twice
        let (conflicts, uses) = match self.facts {
            None => (&[][..], Vec::new()),
            Some(f) => {
                let uses = last
                    .map(|l| {
                        l.after
                            .prefixes
                            .iter()
                            .filter(|p| f.used.contains(&p.id))
                            .map(|p| format!("{}:", p.label))
                            .collect()
                    })
                    .unwrap_or_default();
                // a window holding the whole document has all its bindings
                let whole = w.ctx.is_none() && last.is_none();
                (if whole { &[][..] } else { &f.conflicts[..] }, uses)
            }
        };
        let t = window_text(buf, w, conflicts, &uses);
        let at = |abs: u64| t.head + (abs - w.g) as usize;
        let from = w.from();
        // the input slice after the prologue in force
        let mut check_in = String::new();
        let opener = w.ctx.and_then(Cut::inner).map(|b| b.opener.as_str());
        if let Some(ctx) = w.ctx {
            ctx.after.write(&mut check_in, &[]);
        }
        if let Some(o) = opener {
            check_in.push_str(o);
            check_in.push(' ');
        }
        let check_head = check_in.len();
        check_in.push_str(buf.text(from..end));
        let closer = last.is_some_and(|l| l.kind == Kind::Inner);
        if closer {
            check_in.push_str("\n}");
        }
        let lang = WindowLang {
            trig: self.trig,
            check: &check_in,
            ctx: w.ctx.map(|c| at(c.start)..at(c.end)),
            last: last.map(|c| at(c.start)..at(c.end)),
            trailer: last.map(|_| t.body_end),
            slice: match w.ctx {
                Some(c) => at(c.end)..t.body_end,
                None => 0..t.body_end,
            },
            c0: w.c0(buf).map(|c| at(c.start)),
            stash: RefCell::new(Stash::default()),
        };
        let f = check::run(&lang, &t.window, self.opts).map_err(|e| match e {
            FormatError::Syntax {
                message, offset, ..
            } => {
                let abs = from + offset.saturating_sub(check_head) as u64;
                syntax(buf, message, abs.min(end))
            }
            FormatError::Unsupported {
                message,
                line,
                column,
            } => {
                let off = crate::offset_of(&t.window, line, column);
                let abs = w.g + off.saturating_sub(t.head) as u64;
                let (line, column) = buf.position(abs.min(end));
                FormatError::Unsupported {
                    message,
                    line,
                    column,
                }
            }
            e => e,
        })?;
        let s = lang.stash.into_inner();
        let o = f.text.as_str();
        let unstable = FormatError::Unsafe {
            check: Check::Idempotence,
        };
        // the output after the context item: where the previous window stopped
        let cut = match w.ctx {
            None => 0,
            Some(_) => {
                let lt = s.ctx_last.ok_or(unstable.clone())?;
                if o.get(lt..lt + self.tail.len()) != Some(self.tail.as_str()) {
                    return Err(unstable);
                }
                lt + self.tail.len()
            }
        };
        let body_end = match (last, s.trailer) {
            (None, _) => o.len(),
            (Some(_), Some(t)) => o[..t].trim_end_matches(['\n', ' ']).len(),
            (Some(_), None) => return Err(unstable),
        };
        if body_end < cut {
            return Err(unstable);
        }
        if last.is_some() {
            let lt = s.last_last.ok_or(unstable.clone())?;
            if lt > body_end {
                return Err(unstable);
            }
            self.tail = o[lt..body_end].to_string();
        }
        let out = &o[cut..body_end];
        let input = buf.text(from..end);
        let in_state = w.ctx.map(|c| &*c.after);
        let same_context = match in_state {
            Some(p) => p.same(&self.out_state),
            None => self.out_state.prefixes.is_empty() && self.out_state.base.is_none(),
        };
        let out_tokens = lex::lex(out, LexMode::Turtle);
        if !(same_context && out == input) {
            self.changed = true;
            let quads = s.quads.as_deref().unwrap_or_default();
            self.check_graph(quads, &s.labels, out, &out_tokens, opener, closer)?;
        }
        // the comments, as one multiset over the whole document
        for c in s.comments {
            *self.comments.entry(c).or_default() += 1;
        }
        for t in out_tokens.iter().filter(|t| t.kind == TokenKind::Comment) {
            let c = t.text(out).trim_end();
            match self.comments.get_mut(c) {
                Some(n) => {
                    *n -= 1;
                    if *n == 0 {
                        self.comments.remove(c);
                    }
                }
                None => {
                    self.comments.insert(c.to_string(), -1);
                }
            }
        }
        // the directives the output declared
        let sig: Vec<(TokenKind, &str)> = out_tokens
            .iter()
            .filter(|t| !t.kind.is_trivia() && t.kind != TokenKind::Eof)
            .map(|t| (t.kind, t.text(out)))
            .collect();
        for (i, &(k, word)) in sig.iter().enumerate() {
            let directive = match k {
                TokenKind::LangDir => matches!(word, "@prefix" | "@base"),
                TokenKind::Word => {
                    word.eq_ignore_ascii_case("prefix") || word.eq_ignore_ascii_case("base")
                }
                _ => false,
            };
            if directive {
                self.out_state.apply(&sig[i..sig.len().min(i + 3)], 0);
            }
        }
        for warning in f.warnings {
            self.warn(warning, &t, w, buf, end);
        }
        self.w.write_all(out.as_bytes()).map_err(io_error)?;
        Ok(s.c0_trails)
    }

    fn verbatim(&mut self, bytes: &[u8]) -> Result<bool, FormatError> {
        self.w.write_all(bytes).map_err(io_error)?;
        Ok(true)
    }
}

impl<W: Write> Formatter<'_, W> {
    /// The graph check of an output slice: its quads, after the prologue the output
    /// declared, isomorphic to the input slice's with every labeled blank node fixed.
    fn check_graph(
        &self,
        input: &[Quad],
        in_labels: &HashSet<String>,
        out: &str,
        out_tokens: &[Token],
        opener: Option<&str>,
        closer: bool,
    ) -> Result<(), FormatError> {
        let differs = FormatError::Unsafe {
            check: Check::Graph,
        };
        let mut text = String::new();
        self.out_state.write(&mut text, &[]);
        let mut labels: HashSet<String> = HashSet::new();
        if let Some(o) = opener {
            text.push_str(o);
            text.push(' ');
            labels.extend(
                lex::lex(o, LexMode::Turtle)
                    .iter()
                    .filter(|t| t.kind == TokenKind::BlankNodeLabel)
                    .map(|t| t.text(o)[2..].to_string()),
            );
        }
        text.push_str(out);
        if closer {
            text.push_str("\n}");
        }
        labels.extend(
            out_tokens
                .iter()
                .filter(|t| t.kind == TokenKind::BlankNodeLabel)
                .map(|t| t.text(out)[2..].to_string()),
        );
        let output = graph::parse(&text, self.lang).map_err(|_| differs.clone())?;
        if graph::isomorphic(&fixed(input, in_labels), &fixed(&output, &labels)) {
            Ok(())
        } else {
            Err(differs)
        }
    }

    /// Record a warning at its place in the input (once).
    fn warn(&mut self, mut warning: Warning, t: &Texts, w: &Window<'_>, buf: &Buffer, end: u64) {
        if warning.line > 0 {
            let off = crate::offset_of(&t.window, warning.line, warning.column);
            if off < t.head {
                return;
            }
            let abs = (w.g + (off - t.head) as u64).min(end);
            let (line, column) = buf.position(abs);
            warning.message = warning.message.replacen(
                &format!("at {}:{} ", warning.line, warning.column),
                &format!("at {line}:{column} "),
                1,
            );
            warning.line = line;
            warning.column = column;
        }
        let key = (
            warning.code,
            warning.line,
            warning.column,
            warning.message.clone(),
        );
        if self.seen.insert(key) {
            self.warnings.push(warning);
        }
    }
}

/// A syntax error at input offset `at`.
fn syntax(buf: &Buffer, message: String, at: u64) -> FormatError {
    let (line, column) = buf.position(at);
    FormatError::Syntax {
        message,
        line,
        column,
        offset: at as usize,
    }
}

/// The namespace labeled blank nodes stand in for in the graph check.
const LABELED: &str = "http://sparkles-fmt.invalid/label/";

/// `quads` with the blank nodes labeled `labels` as fixed IRIs.
fn fixed(quads: &[Quad], labels: &HashSet<String>) -> Vec<Quad> {
    let blank = |b: &BlankNode| -> Option<NamedNode> {
        labels
            .contains(b.as_str())
            .then(|| NamedNode::new_unchecked(format!("{LABELED}{}", b.as_str())))
    };
    let subject = |s: &NamedOrBlankNode| match s {
        NamedOrBlankNode::BlankNode(b) => blank(b).map_or_else(|| s.clone(), Into::into),
        _ => s.clone(),
    };
    fn term(t: &Term, subject: &dyn Fn(&NamedOrBlankNode) -> NamedOrBlankNode) -> Term {
        match t {
            Term::BlankNode(b) => match subject(&NamedOrBlankNode::BlankNode(b.clone())) {
                NamedOrBlankNode::NamedNode(n) => n.into(),
                NamedOrBlankNode::BlankNode(b) => b.into(),
            },
            Term::Triple(tr) => Term::Triple(Box::new(Triple::new(
                subject(&tr.subject),
                tr.predicate.clone(),
                term(&tr.object, subject),
            ))),
            t => t.clone(),
        }
    }
    quads
        .iter()
        .map(|q| Quad {
            subject: subject(&q.subject),
            predicate: q.predicate.clone(),
            object: term(&q.object, &subject),
            graph_name: match &q.graph_name {
                GraphName::BlankNode(b) => {
                    blank(b).map_or_else(|| q.graph_name.clone(), Into::into)
                }
                g => g.clone(),
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(text: &str, trig: bool, opts: &Options, window: usize, read: usize) -> String {
        let cfg = StreamConfig {
            window_bytes: window,
            read_bytes: read,
            ..StreamConfig::default()
        };
        let lang = match trig {
            true => Language::TriG,
            false => Language::Turtle,
        };
        let mut out = Vec::new();
        format_stream(|| Ok(text.as_bytes()), &mut out, lang, opts, &cfg)
            .unwrap_or_else(|e| panic!("{text:?}: {e}"));
        String::from_utf8(out).unwrap()
    }

    fn same(text: &str, trig: bool, opts: &Options) {
        let lang = match trig {
            true => Language::TriG,
            false => Language::Turtle,
        };
        let expected = crate::format(text, lang, opts).unwrap().text;
        for (window, read) in [(1, 1), (1, 7), (40, 3), (1 << 20, 1 << 16)] {
            assert_eq!(
                stream(text, trig, opts, window, read),
                expected,
                "window {window}, read {read}: {text:?}"
            );
        }
    }

    #[test]
    fn statements_and_directives() {
        let o = Options::default();
        same("", false, &o);
        same("# only a comment\n", false, &o);
        same(
            "@prefix ex: <http://e/> .\nex:a ex:b ex:c .\nex:d ex:e 1, 2 .\n\n\nex:f ex:g ex:h . # t\n# end\n",
            false,
            &o,
        );
        same(
            "# header\n\n@prefix z: <http://z/> .\n@prefix a: <http://a/> .\nz:s a:p <http://z/o> .\nBASE <http://b/x/>\nPREFIX r: <r/>\n<s> r:p <http://b/x/r/o> .\nPREFIX a: <http://other/>\n<http://a/s> a:p <http://other/o> .",
            false,
            &o,
        );
        same(
            "PREFIX ex: <http://e/>\nex:s ex:p ex:o . # sparkles-fmt: ignore\nex:t   ex:p   ex:o .\n# sparkles-fmt: ignore\nex:u   ex:p   ex:o .\nex:v ex:p [ ex:q 1 ; ex:r ( 1 2 ) ] .\n",
            false,
            &o,
        );
    }

    #[test]
    fn graph_blocks() {
        let o = Options::default();
        same(
            "PREFIX ex: <http://e/>\nex:g { ex:a ex:b ex:c . ex:d ex:e ex:f . ex:h ex:i 1, 2 } # t\n{ ex:a ex:b ex:c }\nGRAPH _:g { ex:a ex:b ex:c .\n # c\n ex:d ex:e ex:f\n # d\n }\nex:x ex:y ex:z .\n[] { ex:a ex:b ex:c . ex:a ex:b ex:d }\n",
            true,
            &o,
        );
    }

    #[test]
    fn pruned_prefixes() {
        let o = Options {
            prune_prefixes: true,
            ..Options::default()
        };
        same(
            "# about\nPREFIX a: <http://e/a#>\nPREFIX b: <http://e/b#>\na:s a:p 1 .\nPREFIX c: <http://e/c#>\n<http://e/s> <http://e/p> 1 .\n<http://e/s> <http://e/p> 2 .\nPREFIX d: <http://e/d#>\n<http://e/s> <http://e/p> <http://e/b#o> .\nPREFIX a: <http://e/2#>\n<http://e/1#x> <http://e/p> 3 .\n",
            false,
            &o,
        );
        same(
            "PREFIX a: <http://e/a#>\n\n# one\n\nPREFIX b: <http://e/b#>\n",
            false,
            &o,
        );
    }

    #[test]
    fn errors_are_placed_in_the_input() {
        let text = "PREFIX ex: <http://e/>\nex:a ex:b ex:c .\nex:a ex:b .\n";
        let cfg = StreamConfig {
            window_bytes: 1,
            ..StreamConfig::default()
        };
        let e = format_stream(
            || Ok(text.as_bytes()),
            std::io::sink(),
            Language::Turtle,
            &Options::default(),
            &cfg,
        )
        .unwrap_err();
        let expected = crate::format(text, Language::Turtle, &Options::default()).unwrap_err();
        assert_eq!(e, expected);
        let e = format_stream(
            || Ok(&b"<a> <b> \"\xff\" .\n"[..]),
            std::io::sink(),
            Language::Turtle,
            &Options::default(),
            &cfg,
        )
        .unwrap_err();
        assert!(
            matches!(
                e,
                FormatError::Syntax {
                    line: 1,
                    column: 10,
                    ..
                }
            ),
            "{e:?}"
        );
    }
}
