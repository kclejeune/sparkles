//! The document around the statements: the file header, comment blocks, blank lines and
//! the order of the output, put together from the formatted chunks in input order.
//!
//! - Comments before the first statement are the **header**: printed first, their blank
//!   lines kept (runs collapsed to one), then one blank line. A last block ending with
//!   `# sparkles-fmt: ignore` right before the first statement leads that statement
//!   instead.
//! - A comment block directly followed by a statement **leads** it and moves with it; a
//!   block followed by a blank line is **detached**; one at the end of the input
//!   **dangles**. Detached blocks and `VERSION` lines are sort barriers.
//! - Blank lines are kept where the input had them (runs collapsed to one), except
//!   between the statements of a sorted run; none at the start or the end.
//! - A statement whose leading block ends with `# sparkles-fmt: ignore` is printed as
//!   written, from its first significant character; it sorts by that text.
//! - A sorted statement with leading comments that lands at the top of the body gets a
//!   blank line between them and itself: comments before the first statement are the
//!   header, so that is how formatting the output again would print them.
//!
//! Whether the output differs from the input is known without holding the input: each
//! printed line says which input line it comes from and whether it equals that line, and
//! the output equals the input exactly when those are all the input's lines, in order,
//! each unchanged.

use super::canon;
use super::chunk::{Chunk, Line};
use super::scan::LineKind;
use super::sort::{Dedup, Record, Sorter};
use crate::{Check, FormatError, Language, LinesConfig, Warning};
use oxrdf::Quad;
use std::io::{self, Write};

/// What a run does with the statements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// source order
    Plain,
    /// `sort`
    Sort,
    /// `canonicalize`
    Canon,
}

/// A line held back: a comment (or a blank line when `text` is empty), with the input line
/// it comes from.
#[derive(Clone, Debug)]
struct Held {
    text: String,
    src: u64,
    same: bool,
}

/// Where the cursor's input line ended up.
struct CursorTrack {
    line: u64,
    col: usize,
    exact: Option<usize>,
    after: Option<usize>,
}

/// Writes the output lines and keeps track of how they relate to the input's.
struct Emitter<W: Write> {
    w: W,
    /// the next input line an unchanged output prints
    next: u64,
    changed: bool,
    bytes: usize,
    cursor: Option<CursorTrack>,
}

impl<W: Write> Emitter<W> {
    /// Print one line, which comes from input line `src` (`None`: from none), and is that
    /// line unchanged when `same`.
    fn line(&mut self, text: &str, src: Option<u64>, same: bool) -> io::Result<()> {
        match src {
            Some(s) if same && s == self.next && !self.changed => self.next += 1,
            _ => self.changed = true,
        }
        if let (Some(c), Some(s)) = (&mut self.cursor, src) {
            if s == c.line && c.exact.is_none() {
                let mut col = c.col.min(text.len());
                while !text.is_char_boundary(col) {
                    col -= 1;
                }
                c.exact = Some(self.bytes + col);
            } else if s > c.line && c.after.is_none() {
                c.after = Some(self.bytes);
            }
        }
        self.w.write_all(text.as_bytes())?;
        self.w.write_all(b"\n")?;
        self.bytes += text.len() + 1;
        Ok(())
    }

    fn held(&mut self, h: &Held) -> io::Result<()> {
        self.line(&h.text, Some(h.src), h.same)
    }

    /// A sorted record's lines; `detach`: a blank line between its leading comments and
    /// its statement (see [`detached_leading`]).
    fn record(&mut self, r: &Record, detach: bool) -> io::Result<()> {
        let Some(t) = &r.text else {
            return self.line(&r.key, Some(r.seq), r.same);
        };
        let n = t.split('\n').count();
        for (i, l) in t.split('\n').enumerate() {
            if detach && i + 1 == n {
                self.line("", None, false)?;
            }
            self.line(l, Some(r.seq + i as u64), r.same)?;
        }
        Ok(())
    }
}

/// Whether the leading comments of a record sorted to the top of the body come apart
/// from it, and whether the statement is then without comments. Comments before the
/// first statement are the header, so formatting the output again would make them that;
/// a blank line after them prints them as it would, and the statement merges with its
/// duplicates as it then would. Leading comments ending with `# sparkles-fmt: ignore`
/// keep leading.
fn detached_leading(r: &Record) -> (bool, bool) {
    let Some(t) = &r.text else {
        return (false, true);
    };
    let mut lines = t.rsplit('\n');
    let statement = lines.next().unwrap_or_default();
    match lines.next() {
        Some(last) if !crate::pragma::is_ignore(last) => (true, statement == &*r.key),
        _ => (false, false),
    }
}

/// The output of a run.
pub struct Done {
    pub statements: u64,
    pub changed: bool,
    pub warnings: Vec<Warning>,
    /// the byte offset in the output of the input position `Assembler::new` tracked
    pub cursor: Option<usize>,
}

/// Puts the formatted chunks together and writes the output.
pub struct Assembler<W: Write> {
    mode: Mode,
    lang: Language,
    em: Emitter<W>,
    in_header: bool,
    header: Vec<Held>,
    /// own-line comments not attached yet
    block: Vec<Held>,
    /// the first blank line before the next element
    blank: Option<Held>,
    /// some of the body is printed
    started: bool,
    sorter: Sorter,
    dedup: Dedup,
    /// the blank line before the current sort run
    run_blank: Option<Held>,
    quads: Vec<Quad>,
    versions: Vec<Held>,
    max_quads: u64,
    /// comments `canonicalize` dropped
    dropped: u64,
    /// the input lines, while the input may already be in canonical form
    canonical_input: Option<Vec<String>>,
    statements: u64,
    lines: u64,
}

impl<W: Write> Assembler<W> {
    /// An assembler writing to `w`; `cursor` is an input position (line index, byte in
    /// the line) whose output position [`Done::cursor`] reports.
    pub fn new(
        w: W,
        mode: Mode,
        lang: Language,
        cfg: &LinesConfig,
        cursor: Option<(u64, usize)>,
    ) -> Assembler<W> {
        Assembler {
            mode,
            lang,
            em: Emitter {
                w,
                next: 0,
                changed: false,
                bytes: 0,
                cursor: cursor.map(|(line, col)| CursorTrack {
                    line,
                    col,
                    exact: None,
                    after: None,
                }),
            },
            in_header: true,
            header: Vec::new(),
            block: Vec::new(),
            blank: None,
            started: false,
            sorter: Sorter::new(cfg.sort_memory, &cfg.spill_dir),
            dedup: Dedup::default(),
            run_blank: None,
            quads: Vec::new(),
            versions: Vec::new(),
            max_quads: cfg.max_canonicalize_quads,
            dropped: 0,
            canonical_input: Some(Vec::new()),
            statements: 0,
            lines: 0,
        }
    }

    /// The next chunk of the input.
    pub fn chunk(&mut self, mut c: Chunk) -> Result<(), FormatError> {
        let lines = std::mem::take(&mut c.lines);
        let mut cq = std::mem::take(&mut c.quads).into_iter();
        for (i, l) in lines.iter().enumerate() {
            let src = c.first_line + i as u64;
            if self.mode == Mode::Canon {
                self.track_canonical_input(&c, l);
            }
            match l.kind {
                LineKind::Blank => self.blank_line(Held {
                    text: String::new(),
                    src,
                    same: l.same,
                })?,
                LineKind::Comment => {
                    let h = Held {
                        text: c.comment(l).unwrap_or_default().to_string(),
                        src,
                        same: l.same,
                    };
                    match self.in_header {
                        true => self.header.push(h),
                        false => self.block.push(h),
                    }
                }
                LineKind::Statement | LineKind::Version => {
                    if l.kind == LineKind::Statement {
                        self.statements += 1;
                        if self.mode == Mode::Canon {
                            if self.quads.len() as u64 >= self.max_quads {
                                return Err(FormatError::TooLarge);
                            }
                            self.quads.push(cq.next().expect("one quad per statement"));
                        }
                    }
                    self.node(&c, l, src)?;
                }
            }
        }
        self.lines += lines.len() as u64;
        Ok(())
    }

    fn track_canonical_input(&mut self, c: &Chunk, l: &Line) {
        let keep = matches!(l.kind, LineKind::Statement | LineKind::Version)
            && l.comment.is_none()
            && l.same;
        match (&mut self.canonical_input, keep) {
            (Some(v), true) => v.push(c.out(l).to_string()),
            _ => self.canonical_input = None,
        }
    }

    fn blank_line(&mut self, h: Held) -> Result<(), FormatError> {
        if self.in_header {
            // blank lines between header comments, not before them; runs collapsed
            if self.header.last().is_some_and(|l| !l.text.is_empty()) {
                self.header.push(h);
            }
            return Ok(());
        }
        if !self.block.is_empty() {
            self.detached()?;
        }
        if self.blank.is_none() {
            self.blank = Some(h);
        }
        Ok(())
    }

    /// A comment block followed by a blank line (or the end of the input).
    fn detached(&mut self) -> Result<(), FormatError> {
        let block = std::mem::take(&mut self.block);
        let blank = self.blank.take();
        match self.mode {
            Mode::Canon => self.dropped += block.len() as u64,
            Mode::Plain | Mode::Sort => {
                self.flush_run()?;
                self.blank_before(blank).map_err(io_error)?;
                for h in &block {
                    self.em.held(h).map_err(io_error)?;
                }
                self.started = true;
            }
        }
        Ok(())
    }

    fn blank_before(&mut self, blank: Option<Held>) -> io::Result<()> {
        match blank {
            Some(b) if self.started => self.em.held(&b),
            _ => Ok(()),
        }
    }

    /// The header ends at the first statement or directive.
    fn end_header(&mut self) -> Result<(), FormatError> {
        self.in_header = false;
        let mut header = std::mem::take(&mut self.header);
        let pop_blank = |h: &mut Vec<Held>| match h.last() {
            Some(l) if l.text.is_empty() => h.pop(),
            _ => None,
        };
        let mut sep = pop_blank(&mut header);
        if sep.is_none()
            && header
                .last()
                .is_some_and(|h| crate::pragma::is_ignore(&h.text))
        {
            let start = header
                .iter()
                .rposition(|h| h.text.is_empty())
                .map_or(0, |i| i + 1);
            self.block = header.split_off(start);
            sep = pop_blank(&mut header);
        }
        if header.is_empty() {
            return Ok(());
        }
        if self.mode == Mode::Canon {
            self.dropped += header.iter().filter(|h| !h.text.is_empty()).count() as u64;
            return Ok(());
        }
        for h in &header {
            self.em.held(h).map_err(io_error)?;
        }
        match sep {
            Some(s) => self.em.held(&s),
            None => self.em.line("", None, false),
        }
        .map_err(io_error)
    }

    /// A statement or `VERSION` line, after its leading block.
    fn node(&mut self, c: &Chunk, l: &Line, src: u64) -> Result<(), FormatError> {
        if self.in_header {
            self.end_header()?;
        }
        let leading = std::mem::take(&mut self.block);
        let blank = self.blank.take();
        if self.mode == Mode::Canon {
            self.dropped += leading.len() as u64 + u64::from(l.comment.is_some());
            if l.kind == LineKind::Version {
                self.versions.push(Held {
                    text: c.key(l).to_string(),
                    src,
                    same: false,
                });
            }
            return Ok(());
        }
        let verbatim = leading
            .last()
            .is_some_and(|h| crate::pragma::is_ignore(&h.text));
        let line = match verbatim {
            true => c.verbatim(l),
            false => c.out(l),
        };
        let line_same = l.lf && line == c.src(l);
        if self.mode == Mode::Sort && l.kind == LineKind::Statement {
            if self.sorter.is_empty() {
                self.run_blank = blank;
            }
            let key = match verbatim {
                true => line,
                false => c.key(l),
            };
            let text = match (leading.is_empty(), line == key) {
                (true, true) => None,
                _ => {
                    let mut t = String::new();
                    for h in &leading {
                        t.push_str(&h.text);
                        t.push('\n');
                    }
                    t.push_str(line);
                    Some(t.into_boxed_str())
                }
            };
            let r = Record {
                key: key.into(),
                text,
                seq: leading.first().map_or(src, |h| h.src),
                lines: leading.len() as u32 + 1,
                same: line_same && leading.iter().all(|h| h.same),
            };
            return self.sorter.push(r).map_err(io_error);
        }
        // printed where it is (a VERSION line is a sort barrier)
        self.flush_run()?;
        self.blank_before(blank).map_err(io_error)?;
        for h in &leading {
            self.em.held(h).map_err(io_error)?;
        }
        self.em.line(line, Some(src), line_same).map_err(io_error)?;
        self.started = true;
        Ok(())
    }

    /// Print the sorted run, if any.
    fn flush_run(&mut self) -> Result<(), FormatError> {
        if self.mode != Mode::Sort || self.sorter.is_empty() {
            return Ok(());
        }
        let pushed = self.sorter.pushed;
        let blank = self.run_blank.take();
        self.blank_before(blank).map_err(io_error)?;
        self.dedup = Dedup::default();
        let Assembler {
            sorter,
            dedup,
            em,
            started,
            ..
        } = self;
        let mut first = !*started;
        sorter
            .finish(&mut |r| {
                // the first record is always kept: nothing sorts before it
                let (detach, plain) = match std::mem::take(&mut first) {
                    true => detached_leading(&r),
                    false => (false, r.text.is_none()),
                };
                match dedup.keep(&r, plain) {
                    true => em.record(&r, detach),
                    false => Ok(()),
                }
            })
            .map_err(io_error)?;
        // sorting only reorders lines and merges identical ones
        if self.dedup.unsorted || self.dedup.emitted + self.dedup.merged != pushed {
            return Err(FormatError::Unsafe {
                check: Check::Graph,
            });
        }
        self.started = true;
        Ok(())
    }

    /// The end of the input: print what is left; `bom` says whether the input started
    /// with a byte order mark.
    pub fn finish(mut self, bom: bool) -> Result<(Done, W), FormatError> {
        let mut warnings = Vec::new();
        if self.in_header {
            // comments only
            let mut header = std::mem::take(&mut self.header);
            while header.last().is_some_and(|h| h.text.is_empty()) {
                header.pop();
            }
            match self.mode {
                Mode::Canon => self.dropped += header.len() as u64,
                _ => {
                    for h in &header {
                        self.em.held(h).map_err(io_error)?;
                    }
                }
            }
        } else if self.mode == Mode::Canon {
            self.dropped += self.block.len() as u64;
            let quads = std::mem::take(&mut self.quads);
            let (lines, unstable) = canon::canonicalize(quads, self.lang)?;
            let mut out = Vec::new();
            for v in std::mem::take(&mut self.versions) {
                self.em.line(&v.text, None, false).map_err(io_error)?;
                out.push(v.text);
            }
            for l in &lines {
                self.em.line(l, None, false).map_err(io_error)?;
            }
            out.extend(lines);
            // the input was canonical already
            let same = self.canonical_input.as_ref() == Some(&out);
            self.em.changed = !same;
            self.em.next = if same { self.lines } else { 0 };
            if unstable {
                warnings.push(Warning {
                    code: "unstable-labels",
                    message: "canonicalize labeled blank nodes inside triple terms, which \
                              RDFC-1.0 does not define: the labels are stable only for this \
                              oxrdf version"
                        .to_string(),
                    line: 0,
                    column: 0,
                });
            }
        } else {
            self.flush_run()?;
            if !self.block.is_empty() {
                self.detached()?;
            }
        }
        if self.dropped > 0 {
            warnings.push(Warning {
                code: "comments-dropped",
                message: format!(
                    "canonicalize dropped {} comment{}",
                    self.dropped,
                    if self.dropped == 1 { "" } else { "s" }
                ),
                line: 0,
                column: 0,
            });
        }
        self.em.w.flush().map_err(io_error)?;
        let changed = self.em.changed || self.em.next != self.lines || bom;
        let cursor = self.em.cursor.and_then(|c| c.exact.or(c.after));
        Ok((
            Done {
                statements: self.statements,
                changed,
                warnings,
                cursor,
            },
            self.em.w,
        ))
    }
}

/// An I/O error of the input or the output. [`FormatError`] has no variant of its own
/// for them; the message says what happened.
pub fn io_error(e: io::Error) -> FormatError {
    FormatError::UnsupportedLanguage {
        language: String::new(),
        message: format!("reading or writing failed: {e}"),
    }
}
