//! The document IR every language's printer builds, and the printer that lays it out:
//! Wadler's "prettier printer" in Lindig's strict variant, extended the way Prettier's doc
//! printer is (groups with ids, `IfBreak`, line suffixes, break propagation). There is no
//! `Fill`: a broken list has one item per line.
//!
//! The printer records where each source token lands in the output, for cursor mapping.

use crate::FormatError;
use crate::lex::Token;
use crate::tree::TokenId;
use std::ops::Range;
use std::time::Instant;

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
/// TODO: the real printer (fits, group modes, `IfBreak`, line suffixes, blank-line
/// collapsing, `unicode-width` measuring, trailing whitespace removal). This one prints
/// every group flat, which is enough for documents of `Verbatim` and `Concat`.
pub fn print(
    arena: &DocArena<'_>,
    root: DocId,
    src: &str,
    width: u16,
    indent: u8,
    deadline: Option<Instant>,
) -> Result<Printed, FormatError> {
    let _ = width;
    let mut p = Flat {
        arena,
        src,
        indent: indent as usize,
        level: 0,
        out: Printed::default(),
        suffix: Vec::new(),
        deadline,
        steps: 0,
    };
    p.doc(root)?;
    p.flush_suffix()?;
    Ok(p.out)
}

struct Flat<'a, 't> {
    arena: &'a DocArena<'t>,
    src: &'a str,
    indent: usize,
    level: usize,
    out: Printed,
    suffix: Vec<DocId>,
    deadline: Option<Instant>,
    steps: u32,
}

impl Flat<'_, '_> {
    fn doc(&mut self, id: DocId) -> Result<(), FormatError> {
        self.steps = self.steps.wrapping_add(1);
        if self.steps.is_multiple_of(4096) && self.deadline.is_some_and(|d| Instant::now() > d) {
            return Err(FormatError::Timeout);
        }
        match self.arena.get(id) {
            Doc::Nil | Doc::SoftLine | Doc::BreakParent => {}
            Doc::Text(s) => self.out.text.push_str(s),
            Doc::Token { id, printed } => {
                let tok = self.arena.tokens[id.0 as usize];
                let start = self.out.text.len() as u32;
                self.out
                    .text
                    .push_str(printed.as_deref().unwrap_or(tok.text(self.src)));
                let len = self.out.text.len() as u32 - start;
                self.out.tok_out.push((*id, start, len));
            }
            Doc::Line => self.out.text.push(' '),
            Doc::HardLine => self.newline()?,
            Doc::EmptyLine => {
                self.newline()?;
                self.newline()?;
            }
            Doc::Indent(d) => {
                self.level += 1;
                self.doc(*d)?;
                self.level -= 1;
            }
            Doc::Group { doc, .. } => self.doc(*doc)?,
            Doc::IfBreak { flat, .. } => self.doc(*flat)?,
            Doc::LineSuffix(d) => self.suffix.push(*d),
            Doc::Concat(ds) => {
                for d in ds {
                    self.doc(*d)?;
                }
            }
            Doc::Verbatim(r) => self.verbatim(r.clone()),
        }
        Ok(())
    }

    fn newline(&mut self) -> Result<(), FormatError> {
        self.flush_suffix()?;
        let trimmed = self.out.text.trim_end_matches([' ', '\t']).len();
        self.out.text.truncate(trimmed);
        self.out.text.push('\n');
        let n = self.level * self.indent;
        self.out.text.extend(std::iter::repeat_n(' ', n));
        Ok(())
    }

    fn flush_suffix(&mut self) -> Result<(), FormatError> {
        for d in std::mem::take(&mut self.suffix) {
            self.doc(d)?;
        }
        Ok(())
    }

    /// Copy a source range, recording the tokens inside it.
    fn verbatim(&mut self, r: Range<usize>) {
        let base = self.out.text.len() as u32;
        self.out.text.push_str(&self.src[r.clone()]);
        let tokens = self.arena.tokens;
        let first = tokens.partition_point(|t| (t.start as usize) < r.start);
        for (i, t) in tokens.iter().enumerate().skip(first) {
            if t.end() > r.end || t.kind == crate::lex::TokenKind::Eof {
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
        let sp = a.line();
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
    }
}
