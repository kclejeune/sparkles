//! The document IR every language's printer builds, and the printer that lays it out:
//! Wadler's "prettier printer" in Lindig's strict variant, extended the way Prettier's doc
//! printer is (groups with ids, `IfBreak`, line suffixes, break propagation). There is no
//! `Fill`: a broken list has one item per line.
//!
//! The printer records where each source token lands in the output, for cursor mapping.

use crate::FormatError;
use crate::lex::{Token, TokenKind};
use crate::tree::TokenId;
use std::ops::Range;
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

/// A document in a [`DocArena`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DocId(pub u32);

/// The identity of a group, for [`DocArena::if_break`] on another group's mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GroupId(pub u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Doc {
    /// nothing
    Nil,
    /// text that is not a source token (a space, an inserted `.` or `WHERE`)
    Text(Box<str>),
    /// a source token: its own text, or `printed` when a normalization changed it
    Token {
        id: TokenId,
        printed: Option<Box<str>>,
    },
    /// a space when flat, a newline and the indentation when broken
    Line,
    /// nothing when flat, a newline and the indentation when broken
    SoftLine,
    /// always a newline; breaks every enclosing group
    HardLine,
    /// a blank line (collapses with an adjacent one)
    EmptyLine,
    /// one more indentation level for the lines inside
    Indent(DocId),
    /// flat when it fits in the rest of the line, else broken
    Group {
        doc: DocId,
        id: GroupId,
    },
    /// `broken` when the group (by default the enclosing one) is broken, else `flat`
    IfBreak {
        broken: DocId,
        flat: DocId,
        group: Option<GroupId>,
    },
    /// printed just before the next newline (trailing comments)
    LineSuffix(DocId),
    /// breaks the enclosing groups
    BreakParent,
    Concat(Vec<DocId>),
    /// a source range copied byte for byte (a pragma-ignored node, or an unfinished
    /// printer's node)
    Verbatim(Range<usize>),
}

/// The arena documents live in. It knows the tokens, so [`Doc::Token`] needs only an id.
#[derive(Clone, Debug)]
pub struct DocArena<'t> {
    tokens: &'t [Token],
    docs: Vec<Doc>,
    groups: u32,
}

impl<'t> DocArena<'t> {
    pub fn new(tokens: &'t [Token]) -> DocArena<'t> {
        DocArena {
            tokens,
            docs: Vec::new(),
            groups: 0,
        }
    }

    pub fn tokens(&self) -> &'t [Token] {
        self.tokens
    }

    pub fn get(&self, id: DocId) -> &Doc {
        &self.docs[id.0 as usize]
    }

    fn push(&mut self, d: Doc) -> DocId {
        self.docs.push(d);
        DocId(self.docs.len() as u32 - 1)
    }

    pub fn nil(&mut self) -> DocId {
        self.push(Doc::Nil)
    }

    pub fn text(&mut self, s: impl Into<Box<str>>) -> DocId {
        self.push(Doc::Text(s.into()))
    }

    pub fn token(&mut self, id: TokenId, printed: Option<Box<str>>) -> DocId {
        self.push(Doc::Token { id, printed })
    }

    pub fn line(&mut self) -> DocId {
        self.push(Doc::Line)
    }

    pub fn soft_line(&mut self) -> DocId {
        self.push(Doc::SoftLine)
    }

    pub fn hard_line(&mut self) -> DocId {
        self.push(Doc::HardLine)
    }

    pub fn empty_line(&mut self) -> DocId {
        self.push(Doc::EmptyLine)
    }

    pub fn indent(&mut self, d: DocId) -> DocId {
        self.push(Doc::Indent(d))
    }

    pub fn group(&mut self, d: DocId) -> (DocId, GroupId) {
        let id = GroupId(self.groups);
        self.groups += 1;
        (self.push(Doc::Group { doc: d, id }), id)
    }

    pub fn if_break(&mut self, broken: DocId, flat: DocId, group: Option<GroupId>) -> DocId {
        self.push(Doc::IfBreak {
            broken,
            flat,
            group,
        })
    }

    pub fn line_suffix(&mut self, d: DocId) -> DocId {
        self.push(Doc::LineSuffix(d))
    }

    pub fn break_parent(&mut self) -> DocId {
        self.push(Doc::BreakParent)
    }

    pub fn concat(&mut self, ds: impl IntoIterator<Item = DocId>) -> DocId {
        self.push(Doc::Concat(ds.into_iter().collect()))
    }

    pub fn verbatim(&mut self, r: Range<usize>) -> DocId {
        self.push(Doc::Verbatim(r))
    }
}

/// Printed text and where each source token went: `(token, output start, output
/// length)` in bytes, in output order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Printed {
    pub text: String,
    pub tok_out: Vec<(TokenId, u32, u32)>,
}

/// Lay out `root` within `width` columns, indenting by `indent` spaces per level.
///
/// - A group prints flat when it fits: its flat text, and what follows it up to the next
///   possible line break, fit in the rest of the line. A group holding a
///   [`Doc::HardLine`], [`Doc::EmptyLine`] or [`Doc::BreakParent`] always breaks, and so
///   does every group around it.
/// - Line suffixes wait for the next newline (or the end). Two of them never share a
///   line: a line comment would swallow the next one.
/// - Newlines leave no trailing spaces, indentation is written only before text (empty
///   lines stay empty), and at most one blank line is printed in a row.
/// - Comment tokens print without their trailing whitespace.
/// - Text holding a newline (a long string, a verbatim range) is copied as is: its later
///   lines are not re-indented.
///
/// Fails with [`FormatError::Timeout`] once `deadline` has passed.
pub fn print(
    arena: &DocArena<'_>,
    root: DocId,
    src: &str,
    width: u16,
    indent: u8,
    deadline: Option<Instant>,
) -> Result<Printed, FormatError> {
    let mut p = Printer {
        arena,
        src,
        width: i64::from(width),
        indent: u32::from(indent),
        hard: forced_breaks(arena),
        modes: vec![None; arena.groups as usize],
        out: Printed::default(),
        pos: 0,
        pending: None,
        suffix: Vec::new(),
        remeasure: false,
        deadline,
        steps: 0,
        scratch: Vec::new(),
    };
    p.run(root)?;
    Ok(p.out)
}

/// Per document: whether it holds a forced break, so every group around it breaks. A
/// document only refers to earlier ones, so one pass in id order sees children first.
/// An `IfBreak` forces a break only through its flat branch: a hard line in its broken
/// branch is printed only once the group broke for another reason.
fn forced_breaks(arena: &DocArena<'_>) -> Vec<bool> {
    let mut hard: Vec<bool> = Vec::with_capacity(arena.docs.len());
    for d in &arena.docs {
        let at = |x: &DocId| hard.get(x.0 as usize).copied().unwrap_or(false);
        let h = match d {
            Doc::HardLine | Doc::EmptyLine | Doc::BreakParent => true,
            Doc::Indent(x) | Doc::Group { doc: x, .. } => at(x),
            Doc::IfBreak { flat, .. } => at(flat),
            Doc::Concat(ds) => ds.iter().any(at),
            _ => false,
        };
        hard.push(h);
    }
    hard
}

/// A token's printed text: `printed`, else its source text (a comment without its
/// trailing whitespace).
fn token_text<'a>(
    tokens: &[Token],
    src: &'a str,
    id: TokenId,
    printed: Option<&'a str>,
) -> &'a str {
    if let Some(p) = printed {
        return p;
    }
    let tok = tokens[id.0 as usize];
    let text = tok.text(src);
    match tok.kind {
        TokenKind::Comment => text.trim_end(),
        _ => text,
    }
}

/// The display width of `s` up to its first line break, and whether it has one.
fn measure(s: &str) -> (i64, bool) {
    match s.find(['\n', '\r']) {
        Some(i) => (s[..i].width() as i64, true),
        None => (s.width() as i64, false),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

#[derive(Clone, Debug)]
enum Work {
    Doc(DocId),
    /// the rest of a verbatim range, after line suffixes were flushed into it
    Verbatim(Range<usize>),
    /// a newline between two line suffixes
    Newline,
}

#[derive(Clone, Debug)]
struct Cmd {
    /// indentation in columns
    ind: u32,
    mode: Mode,
    work: Work,
}

struct Printer<'a, 't> {
    arena: &'a DocArena<'t>,
    src: &'a str,
    width: i64,
    indent: u32,
    hard: Vec<bool>,
    /// the mode each group printed in, for `IfBreak` on a group id
    modes: Vec<Option<Mode>>,
    out: Printed,
    /// the display width of the current line, pending indentation included
    pos: i64,
    /// indentation not written yet: it goes before the line's first text
    pending: Option<u32>,
    suffix: Vec<Cmd>,
    /// a newline was printed inside flat content, so the next group measures again
    remeasure: bool,
    deadline: Option<Instant>,
    steps: u32,
    scratch: Vec<(Mode, Work)>,
}

impl<'a> Printer<'a, '_> {
    fn run(&mut self, root: DocId) -> Result<(), FormatError> {
        let arena = self.arena;
        let mut stack = vec![Cmd {
            ind: 0,
            mode: Mode::Break,
            work: Work::Doc(root),
        }];
        let push = |stack: &mut Vec<Cmd>, ind: u32, mode: Mode, x: DocId| {
            stack.push(Cmd {
                ind,
                mode,
                work: Work::Doc(x),
            });
        };
        loop {
            let Some(Cmd { ind, mode, work }) = stack.pop() else {
                if self.suffix.is_empty() {
                    return Ok(());
                }
                self.flush_suffix(&mut stack);
                continue;
            };
            self.tick()?;
            let d = match work {
                Work::Doc(d) => d,
                Work::Verbatim(r) => {
                    self.verbatim(r, ind, mode, &mut stack);
                    continue;
                }
                Work::Newline => {
                    self.newline(ind, false);
                    continue;
                }
            };
            match arena.get(d) {
                Doc::Nil | Doc::BreakParent => {}
                Doc::Text(s) => {
                    self.write(s, mode);
                }
                Doc::Token { id, printed } => {
                    let text = token_text(arena.tokens, self.src, *id, printed.as_deref());
                    let start = self.write(text, mode);
                    self.out
                        .tok_out
                        .push((*id, start as u32, text.len() as u32));
                }
                Doc::Verbatim(r) => self.verbatim(r.clone(), ind, mode, &mut stack),
                Doc::Concat(ds) => {
                    for &x in ds.iter().rev() {
                        push(&mut stack, ind, mode, x);
                    }
                }
                Doc::Indent(x) => push(&mut stack, ind + self.indent, mode, *x),
                Doc::Group { doc, id } => {
                    let m = if self.hard[d.0 as usize] {
                        Mode::Break
                    } else if mode == Mode::Flat && !self.remeasure {
                        Mode::Flat
                    } else {
                        self.remeasure = false;
                        if let Some(m) = self.modes.get_mut(id.0 as usize) {
                            *m = None;
                        }
                        let next = Cmd {
                            ind,
                            mode: Mode::Flat,
                            work: Work::Doc(*doc),
                        };
                        match self.fits(&next, &stack)? {
                            true => Mode::Flat,
                            false => Mode::Break,
                        }
                    };
                    if let Some(slot) = self.modes.get_mut(id.0 as usize) {
                        *slot = Some(m);
                    }
                    push(&mut stack, ind, m, *doc);
                }
                Doc::IfBreak {
                    broken,
                    flat,
                    group,
                } => {
                    let x = match self.if_break_mode(*group, mode) {
                        Mode::Break => *broken,
                        Mode::Flat => *flat,
                    };
                    push(&mut stack, ind, mode, x);
                }
                Doc::Line if mode == Mode::Flat => {
                    self.write(" ", mode);
                }
                Doc::SoftLine if mode == Mode::Flat => {}
                Doc::Line | Doc::SoftLine | Doc::HardLine | Doc::EmptyLine => {
                    if !self.suffix.is_empty() {
                        // the line suffixes first, then this line break again
                        push(&mut stack, ind, mode, d);
                        self.flush_suffix(&mut stack);
                        continue;
                    }
                    if mode == Mode::Flat {
                        self.remeasure = true;
                    }
                    self.newline(ind, matches!(arena.get(d), Doc::EmptyLine));
                }
                Doc::LineSuffix(x) => self.suffix.push(Cmd {
                    ind,
                    mode,
                    work: Work::Doc(*x),
                }),
            }
        }
    }

    /// The mode an `IfBreak` follows: its group's, or the enclosing one's. A group not
    /// printed yet counts as flat.
    fn if_break_mode(&self, group: Option<GroupId>, mode: Mode) -> Mode {
        match group {
            Some(g) => self
                .modes
                .get(g.0 as usize)
                .copied()
                .flatten()
                .unwrap_or(Mode::Flat),
            None => mode,
        }
    }

    /// Whether `next` printed flat, and the rest of the stream up to its next possible
    /// line break, fit in the rest of the line.
    fn fits(&mut self, next: &Cmd, rest: &[Cmd]) -> Result<bool, FormatError> {
        let (arena, src) = (self.arena, self.src);
        let mut rem = self.width - self.pos;
        let mut rest_i = rest.len();
        let mut cmds = std::mem::take(&mut self.scratch);
        cmds.clear();
        cmds.push((next.mode, next.work.clone()));
        let fits = loop {
            if rem < 0 {
                break false;
            }
            let (mode, work) = match cmds.pop() {
                Some(c) => c,
                None if rest_i == 0 => break true,
                None => {
                    rest_i -= 1;
                    (rest[rest_i].mode, rest[rest_i].work.clone())
                }
            };
            self.tick()?;
            let d = match work {
                Work::Doc(d) => d,
                Work::Verbatim(r) => match measure(&src[r]) {
                    (w, true) => break rem - w >= 0,
                    (w, false) => {
                        rem -= w;
                        continue;
                    }
                },
                Work::Newline => break true,
            };
            let text = match arena.get(d) {
                Doc::Nil | Doc::BreakParent | Doc::LineSuffix(_) => continue,
                Doc::Text(s) => &**s,
                Doc::Token { id, printed } => {
                    token_text(arena.tokens, src, *id, printed.as_deref())
                }
                Doc::Verbatim(r) => &src[r.clone()],
                Doc::Concat(ds) => {
                    cmds.extend(ds.iter().rev().map(|&x| (mode, Work::Doc(x))));
                    continue;
                }
                Doc::Indent(x) => {
                    cmds.push((mode, Work::Doc(*x)));
                    continue;
                }
                Doc::Group { doc, .. } => {
                    let m = match self.hard[d.0 as usize] {
                        true => Mode::Break,
                        false => mode,
                    };
                    cmds.push((m, Work::Doc(*doc)));
                    continue;
                }
                Doc::IfBreak {
                    broken,
                    flat,
                    group,
                } => {
                    let x = match self.if_break_mode(*group, mode) {
                        Mode::Break => *broken,
                        Mode::Flat => *flat,
                    };
                    cmds.push((mode, Work::Doc(x)));
                    continue;
                }
                Doc::Line | Doc::SoftLine if mode == Mode::Break => break true,
                Doc::Line => {
                    rem -= 1;
                    continue;
                }
                Doc::SoftLine => continue,
                Doc::HardLine | Doc::EmptyLine => break true,
            };
            match measure(text) {
                (w, true) => break rem - w >= 0,
                (w, false) => rem -= w,
            }
        };
        self.scratch = cmds;
        Ok(fits)
    }

    fn tick(&mut self) -> Result<(), FormatError> {
        self.steps = self.steps.wrapping_add(1);
        if self.steps & 0xfff == 1 && self.deadline.is_some_and(|d| Instant::now() > d) {
            return Err(FormatError::Timeout);
        }
        Ok(())
    }

    /// Append `s` after the line's pending indentation; returns where `s` starts.
    fn write(&mut self, s: &str, mode: Mode) -> usize {
        if s.is_empty() {
            return self.out.text.len();
        }
        if let Some(n) = self.pending.take() {
            self.out.text.extend(std::iter::repeat_n(' ', n as usize));
        }
        let start = self.out.text.len();
        self.out.text.push_str(s);
        match s.rfind('\n') {
            Some(i) => {
                self.pos = s[i + 1..].width() as i64;
                if mode == Mode::Flat {
                    self.remeasure = true;
                }
            }
            None => self.pos += s.width() as i64,
        }
        start
    }

    /// End the line (with a blank line after it when `blank`) and indent the next one by
    /// `ind`. Never more than one blank line in a row, and none at the very start.
    fn newline(&mut self, ind: u32, blank: bool) {
        self.trim_end();
        let text = &mut self.out.text;
        let newlines = text.len() - text.trim_end_matches('\n').len();
        if blank {
            if !text.is_empty() {
                for _ in newlines.min(2)..2 {
                    text.push('\n');
                }
            }
        } else if newlines < 2 {
            text.push('\n');
        }
        self.pending = Some(ind);
        self.pos = i64::from(ind);
    }

    /// Drop trailing spaces and tabs (a token's recorded length shrinks with them).
    fn trim_end(&mut self) {
        let len = self.out.text.trim_end_matches([' ', '\t']).len();
        if len == self.out.text.len() {
            return;
        }
        self.out.text.truncate(len);
        for e in self.out.tok_out.iter_mut().rev() {
            if (e.1 + e.2) as usize <= len {
                break;
            }
            e.2 = (len as u32).saturating_sub(e.1);
        }
    }

    /// Schedule the pending line suffixes, each after the first on a line of its own.
    fn flush_suffix(&mut self, stack: &mut Vec<Cmd>) {
        let suffix = std::mem::take(&mut self.suffix);
        for (i, c) in suffix.into_iter().enumerate().rev() {
            let ind = c.ind;
            stack.push(c);
            if i > 0 {
                stack.push(Cmd {
                    ind,
                    mode: Mode::Break,
                    work: Work::Newline,
                });
            }
        }
    }

    /// Copy a source range. Pending line suffixes go before its first line break that is
    /// whitespace (never inside a long string).
    fn verbatim(&mut self, r: Range<usize>, ind: u32, mode: Mode, stack: &mut Vec<Cmd>) {
        if !self.suffix.is_empty()
            && let Some((before, at)) = self.first_break(&r)
        {
            stack.push(Cmd {
                ind,
                mode,
                work: Work::Verbatim(at..r.end),
            });
            self.copy(r.start..before, mode);
            self.flush_suffix(stack);
            return;
        }
        self.copy(r, mode);
    }

    /// In `r`, the first whitespace token holding a line break: its start, and where the
    /// break is.
    fn first_break(&self, r: &Range<usize>) -> Option<(usize, usize)> {
        let tokens = self.arena.tokens;
        let first = tokens.partition_point(|t| (t.start as usize) < r.start);
        tokens[first..]
            .iter()
            .take_while(|t| t.end() <= r.end && t.kind != TokenKind::Eof)
            .filter(|t| t.kind == TokenKind::Whitespace)
            .find_map(|t| {
                let at = t.text(self.src).find(['\n', '\r'])?;
                Some((t.start as usize, t.start as usize + at))
            })
    }

    /// Copy a source range, recording the tokens inside it.
    fn copy(&mut self, r: Range<usize>, mode: Mode) {
        let src: &'a str = self.src;
        let base = self.write(&src[r.clone()], mode) as u32;
        let tokens = self.arena.tokens;
        let first = tokens.partition_point(|t| (t.start as usize) < r.start);
        for (i, t) in tokens.iter().enumerate().skip(first) {
            if t.end() > r.end || t.kind == TokenKind::Eof {
                break;
            }
            let start = base + t.start - r.start as u32;
            self.out.tok_out.push((TokenId(i as u32), start, t.len));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::{LexMode, lex};

    fn show(a: &DocArena<'_>, d: DocId, width: u16) -> String {
        print(a, d, "", width, 2, None).unwrap().text
    }

    /// `[a, b, c]` as a group: flat `[a, b, c]`, broken one item per line.
    fn list(a: &mut DocArena<'_>, items: &[&str]) -> (DocId, GroupId) {
        let mut parts = vec![a.soft_line()];
        for (i, s) in items.iter().enumerate() {
            if i > 0 {
                let comma = a.text(",");
                let line = a.line();
                parts.extend([comma, line]);
            }
            parts.push(a.text(*s));
        }
        let open = a.text("[");
        let body = a.concat(parts);
        let body = a.indent(body);
        let sl = a.soft_line();
        let close = a.text("]");
        let all = a.concat([open, body, sl, close]);
        a.group(all)
    }

    #[test]
    fn verbatim_and_tokens() {
        let src = "SELECT * { ?s ?p ?o }";
        let toks = lex(src, LexMode::Sparql);
        let mut a = DocArena::new(&toks);
        let v = a.verbatim(0..src.len());
        let p = print(&a, v, src, 100, 2, None).unwrap();
        assert_eq!(p.text, src);
        // every token but the empty end of input
        assert_eq!(p.tok_out.len(), toks.len() - 1);
        assert!(p.tok_out.iter().all(|&(id, start, len)| {
            let t = toks[id.0 as usize];
            t.start == start && t.len == len
        }));

        let mut a = DocArena::new(&toks);
        let sel = a.token(TokenId(0), Some("select".into()));
        let sp = a.text(" ");
        let star = a.token(TokenId(2), None);
        let hl = a.hard_line();
        let rest = a.verbatim(9..src.len());
        let inner = a.concat([hl, rest]);
        let ind = a.indent(inner);
        let all = a.concat([sel, sp, star, ind]);
        let p = print(&a, all, src, 100, 2, None).unwrap();
        assert_eq!(p.text, "select *\n  { ?s ?p ?o }");
        assert_eq!(p.tok_out[0], (TokenId(0), 0, 6));
        assert_eq!(p.tok_out[1], (TokenId(2), 7, 1));
        assert_eq!(p.tok_out[2].1, 11);
    }

    #[test]
    fn groups_fit_or_break() {
        let mut a = DocArena::new(&[]);
        let (g, _) = list(&mut a, &["aaa", "bbb", "ccc"]);
        assert_eq!(show(&a, g, 15), "[aaa, bbb, ccc]");
        assert_eq!(show(&a, g, 14), "[\n  aaa,\n  bbb,\n  ccc\n]");

        // the outer group breaks, the inner ones fit on their lines
        let mut a = DocArena::new(&[]);
        let (x, _) = list(&mut a, &["1", "2"]);
        let (y, _) = list(&mut a, &["3", "4"]);
        let open = a.text("f(");
        let sl = a.soft_line();
        let comma = a.text(",");
        let line = a.line();
        let body = a.concat([sl, x, comma, line, y]);
        let body = a.indent(body);
        let sl2 = a.soft_line();
        let close = a.text(")");
        let all = a.concat([open, body, sl2, close]);
        let (g, _) = a.group(all);
        assert_eq!(show(&a, g, 80), "f([1, 2], [3, 4])");
        assert_eq!(show(&a, g, 12), "f(\n  [1, 2],\n  [3, 4]\n)");
        assert_eq!(
            show(&a, g, 5),
            "f(\n  [\n    1,\n    2\n  ],\n  [\n    3,\n    4\n  ]\n)"
        );
    }

    #[test]
    fn fitting_counts_the_rest_of_the_line() {
        // the group itself fits, but not with the text after it before the next line
        let mut a = DocArena::new(&[]);
        let (g, _) = list(&mut a, &["a", "b"]);
        let tail = a.text(" + something");
        let hl = a.hard_line();
        let after = a.text("next");
        let all = a.concat([g, tail, hl, after]);
        assert_eq!(show(&a, all, 18), "[a, b] + something\nnext");
        assert_eq!(show(&a, all, 17), "[\n  a,\n  b\n] + something\nnext");
    }

    #[test]
    fn break_parent_breaks_every_enclosing_group() {
        let mut a = DocArena::new(&[]);
        let x = a.text("x");
        let bp = a.break_parent();
        let line = a.line();
        let y = a.text("y");
        let c = a.concat([x, bp, line, y]);
        let (inner, _) = a.group(c);
        let open = a.text("(");
        let line = a.line();
        let close = a.text(")");
        let body = a.concat([line, inner]);
        let body = a.indent(body);
        let c = a.concat([open, body, close]);
        let (outer, _) = a.group(c);
        assert_eq!(show(&a, outer, 80), "(\n  x\n  y)");

        // a hard line breaks the enclosing groups too, but not a sibling group
        let mut a = DocArena::new(&[]);
        let (sibling, _) = list(&mut a, &["1", "2"]);
        let hl = a.hard_line();
        let z = a.text("z");
        let line = a.line();
        let c = a.concat([sibling, line, z, hl, z]);
        let (g, _) = a.group(c);
        assert_eq!(show(&a, g, 80), "[1, 2]\nz\nz");
    }

    #[test]
    fn if_break_follows_its_group() {
        // a note after a list only when the list broke
        let mut a = DocArena::new(&[]);
        let (g, id) = list(&mut a, &["aaaa", "bbbb"]);
        let yes = a.text(" // broken");
        let no = a.nil();
        let note = a.if_break(yes, no, Some(id));
        let all = a.concat([g, note]);
        assert_eq!(show(&a, all, 80), "[aaaa, bbbb]");
        assert_eq!(show(&a, all, 11), "[\n  aaaa,\n  bbbb\n] // broken");

        // without a group id it follows the enclosing group: a trailing `,` when broken
        let mut a = DocArena::new(&[]);
        let x = a.text("xxxx");
        let comma = a.text(",");
        let none = a.nil();
        let trailing = a.if_break(comma, none, None);
        let sl = a.soft_line();
        let body = a.concat([sl, x, trailing]);
        let body = a.indent(body);
        let sl2 = a.soft_line();
        let open = a.text("(");
        let close = a.text(")");
        let all = a.concat([open, body, sl2, close]);
        let (g, _) = a.group(all);
        assert_eq!(show(&a, g, 80), "(xxxx)");
        assert_eq!(show(&a, g, 5), "(\n  xxxx,\n)");
    }

    #[test]
    fn line_suffixes_wait_for_the_newline() {
        let mut a = DocArena::new(&[]);
        let x = a.text("x");
        let c = a.text(" # c");
        let suffix = a.line_suffix(c);
        let comma = a.text(",");
        let hl = a.hard_line();
        let y = a.text("y");
        let all = a.concat([x, suffix, comma, hl, y]);
        assert_eq!(show(&a, all, 80), "x, # c\ny");

        // at the end of the document
        let all = a.concat([y, suffix]);
        assert_eq!(show(&a, all, 80), "y # c");

        // two suffixes on one line never run together
        let d = a.text(" # d");
        let suffix2 = a.line_suffix(d);
        let all = a.concat([x, suffix, y, suffix2, hl, x]);
        assert_eq!(show(&a, all, 80), "xy # c\n # d\nx");

        // a suffix with a break parent breaks its group
        let bp = a.break_parent();
        let line = a.line();
        let c = a.concat([x, suffix, bp, line, y]);
        let (g, _) = a.group(c);
        assert_eq!(show(&a, g, 80), "x # c\ny");
    }

    #[test]
    fn wide_characters_take_two_columns() {
        let mut a = DocArena::new(&[]);
        // "日本語" is 9 bytes and 6 columns
        let (g, _) = list(&mut a, &["日本語", "ab"]);
        assert_eq!(show(&a, g, 12), "[日本語, ab]");
        assert_eq!(show(&a, g, 11), "[\n  日本語,\n  ab\n]");
    }

    #[test]
    fn blank_lines_and_trailing_whitespace() {
        let mut a = DocArena::new(&[]);
        let x = a.text("x  ");
        let el = a.empty_line();
        let hl = a.hard_line();
        let y = a.text("y");
        // blank lines collapse, and carry no indentation
        let body = a.concat([el, hl, el, hl, x, el, el, y]);
        let body = a.indent(body);
        let top = a.concat([y, body]);
        assert_eq!(show(&a, top, 80), "y\n\n  x\n\n  y");
        // no blank line at the very start; trailing spaces go at the next newline
        let all = a.concat([el, x]);
        assert_eq!(show(&a, all, 80), "x  ");
    }

    #[test]
    fn comments_print_without_trailing_whitespace() {
        let src = "?x # c  \n";
        let toks = lex(src, LexMode::Sparql);
        let c = toks
            .iter()
            .position(|t| t.kind == TokenKind::Comment)
            .unwrap() as u32;
        let mut a = DocArena::new(&toks);
        let x = a.token(TokenId(0), None);
        let sp = a.text(" ");
        let cm = a.token(TokenId(c), None);
        let all = a.concat([x, sp, cm]);
        let p = print(&a, all, src, 80, 2, None).unwrap();
        assert_eq!(p.text, "?x # c");
        assert_eq!(p.tok_out[1], (TokenId(c), 3, 3));
    }

    #[test]
    fn verbatim_lines_are_copied_and_take_pending_suffixes() {
        let src = "{ ?s ?p \"\"\"a\nb\"\"\" .  \n    ?s ?q ?o }";
        let toks = lex(src, LexMode::Sparql);
        let mut a = DocArena::new(&toks);
        let x = a.text("x");
        let c = a.text(" # c");
        let suffix = a.line_suffix(c);
        let sp = a.text(" ");
        let v = a.verbatim(0..src.len());
        let body = a.concat([x, suffix, sp, v]);
        let all = a.indent(body);
        let p = print(&a, all, src, 80, 2, None).unwrap();
        // the suffix waits for the first line break in whitespace, not the string's
        assert_eq!(p.text, "x { ?s ?p \"\"\"a\nb\"\"\" . # c\n    ?s ?q ?o }");
    }

    #[test]
    fn a_passed_deadline_times_out() {
        let mut a = DocArena::new(&[]);
        let x = a.text("x");
        let all = a.concat(std::iter::repeat_n(x, 10));
        let past = Instant::now() - std::time::Duration::from_secs(1);
        assert!(matches!(
            print(&a, all, "", 80, 2, Some(past)),
            Err(FormatError::Timeout)
        ));
        assert!(print(&a, all, "", 80, 2, None).is_ok());
    }
}
