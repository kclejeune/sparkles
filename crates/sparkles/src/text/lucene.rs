//! The query strings of `text:query`: Lucene's classic query syntax, which jena-text hands
//! to Lucene's `QueryParser`, turned into Tantivy queries over the text field.
//!
//! The parser follows Lucene's grammar and its rules for combining clauses, with OR as
//! the default operator, so a query string matches the documents it matches in Jena
//! (given the same tokens):
//!
//! * words, analyzed like the indexed text (a word the analyzer splits becomes an OR of
//!   its parts, as in Lucene), and `"phrases"` with an optional `~slop`;
//! * `+required`, `-excluded` and `!excluded`, `AND`/`&&`, `OR`/`||`, `NOT`, and
//!   parentheses;
//! * prefixes `al*`, wildcards `a?an` and `*ing` (Jena allows a leading wildcard),
//!   fuzzy terms `roam~`, `roam~1` and `roam~0.5` (at most two edits, and the 50 closest
//!   terms, as in Lucene), regular expressions `/al(an|len)/`, term ranges `[a TO c]` and
//!   `{a TO c}`, boosts `^2`, and `*` alone for every literal.
//!
//! Prefix, wildcard, fuzzy, regular expression and range terms are lowercased and
//! ASCII-folded like the indexed tokens, but not split. Sparkles reads `"ada lov"*` as a
//! phrase whose last word is a prefix, where Lucene reads the phrase or any document.
//! Field names (`name:ada`, `*:*`) are refused, since the call's predicates select what
//! is searched, and so is a query with no word to search for or with only excluded words,
//! which finds nothing in Jena.

use levenshtein_automata::{DFA, Distance, LevenshteinAutomatonBuilder};
use std::ops::Bound;
use std::sync::OnceLock;
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, Occur, PhrasePrefixQuery, PhraseQuery, Query, RangeQuery,
    RegexQuery, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption};
use tantivy::tokenizer::TextAnalyzer;
use tantivy::{Searcher, Term};

type Res<T> = std::result::Result<T, String>;

/// Terms a fuzzy term expands to at most (Lucene's `FuzzyQuery` default).
const FUZZY_EXPANSIONS: usize = 50;

/// The deepest nesting of parentheses a query string may have.
const MAX_DEPTH: usize = 64;

/// Parse a `text:query` string into a query over `field`. Fuzzy terms are expanded
/// against the terms of `searcher`.
pub(super) fn parse(src: &str, searcher: &Searcher, field: Field) -> Res<Box<dyn Query>> {
    let toks = lex(src)?;
    let mut p = Parser { toks, pos: 0 };
    let ast = p.query(0)?;
    if let Some(t) = p.toks.get(p.pos) {
        return Err(format!("unexpected {}", t.tok.describe()));
    }
    if ast.clauses.is_empty() {
        return Err("empty query".into());
    }
    let b = Builder {
        searcher,
        field,
        analyzer: searcher
            .index()
            .tokenizer_for_field(field)
            .map_err(|e| e.to_string())?,
    };
    if !ast.clauses.iter().any(|(o, _)| *o != Occur::MustNot) {
        return Err("the query needs a word that is not excluded".into());
    }
    match b.clauses(&ast)? {
        Some(q) => Ok(q),
        None => Err("the query has no word to search for".into()),
    }
}

// ------------------------------------------------------------------------- lexer --

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    And,
    Or,
    Not,
    Plus,
    Minus,
    LParen,
    RParen,
    Colon,
    /// `^n`
    Boost(f32),
    /// `~` or `~n`
    Tilde(Option<f32>),
    Quoted(String),
    /// a term's characters, each with whether it was escaped
    Term(Vec<(char, bool)>),
    Regex(String),
    Range {
        lo: Option<String>,
        hi: Option<String>,
        lo_incl: bool,
        hi_incl: bool,
    },
}

impl Tok {
    fn describe(&self) -> String {
        match self {
            Tok::And => "AND".into(),
            Tok::Or => "OR".into(),
            Tok::Not => "NOT".into(),
            Tok::Plus => "'+'".into(),
            Tok::Minus => "'-'".into(),
            Tok::LParen => "'('".into(),
            Tok::RParen => "')'".into(),
            Tok::Colon => "':'".into(),
            Tok::Boost(_) => "'^'".into(),
            Tok::Tilde(_) => "'~'".into(),
            Tok::Quoted(s) => format!("\"{s}\""),
            Tok::Term(t) => format!("'{}'", t.iter().map(|(c, _)| c).collect::<String>()),
            Tok::Regex(r) => format!("/{r}/"),
            Tok::Range { .. } => "range".into(),
        }
    }
}

#[derive(Debug)]
struct Token {
    tok: Tok,
    start: usize,
    end: usize,
}

/// Characters that end a term (Lucene's `_TERM_CHAR` excludes them); `+` and `-` only
/// start a clause, inside a term they belong to it.
fn ends_term(c: char) -> bool {
    c.is_whitespace() || "!():^[]\"{}~/".contains(c)
}

/// `n` or `n.m` at the start of `s`, and its length.
fn number(s: &str) -> Option<(f32, usize)> {
    let int = s.bytes().take_while(u8::is_ascii_digit).count();
    if int == 0 {
        return None;
    }
    let mut len = int;
    if s[len..].starts_with('.') {
        let frac = s[len + 1..].bytes().take_while(u8::is_ascii_digit).count();
        if frac > 0 {
            len += 1 + frac;
        }
    }
    Some((s[..len].parse().ok()?, len))
}

fn lex(src: &str) -> Res<Vec<Token>> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(c) = src[i..].chars().next() {
        let start = i;
        let rest = &src[i + c.len_utf8()..];
        let at_end = |s: &str| s.chars().next().is_none_or(char::is_whitespace);
        let tok = match c {
            c if c.is_whitespace() => {
                i += c.len_utf8();
                continue;
            }
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            ':' => Tok::Colon,
            // a lone operator character is a word that analyzes to nothing
            '+' | '-' | '!' if at_end(rest) => Tok::Term(vec![(c, false)]),
            '+' => Tok::Plus,
            '-' => Tok::Minus,
            '!' => Tok::Not,
            '^' => {
                let (n, len) = number(rest).ok_or("'^' needs a number")?;
                i += 1 + len;
                out.push(Token {
                    tok: Tok::Boost(n),
                    start,
                    end: i,
                });
                continue;
            }
            '~' => {
                let n = number(rest);
                i += 1 + n.map_or(0, |(_, len)| len);
                out.push(Token {
                    tok: Tok::Tilde(n.map(|(n, _)| n)),
                    start,
                    end: i,
                });
                continue;
            }
            '"' => {
                let mut s = String::new();
                let mut chars = rest.char_indices();
                let mut end = None;
                while let Some((j, c)) = chars.next() {
                    match c {
                        '\\' => match chars.next() {
                            Some((_, e)) => s.push(e),
                            None => break,
                        },
                        '"' => {
                            end = Some(j);
                            break;
                        }
                        c => s.push(c),
                    }
                }
                let end = end.ok_or("unterminated phrase")?;
                i += 1 + end + 1;
                out.push(Token {
                    tok: Tok::Quoted(s),
                    start,
                    end: i,
                });
                continue;
            }
            '/' => {
                let mut s = String::new();
                let mut chars = rest.char_indices();
                let mut end = None;
                while let Some((j, c)) = chars.next() {
                    match c {
                        '\\' => match chars.next() {
                            Some((_, '/')) => s.push('/'),
                            Some((_, e)) => {
                                s.push('\\');
                                s.push(e);
                            }
                            None => break,
                        },
                        '/' => {
                            end = Some(j);
                            break;
                        }
                        c => s.push(c),
                    }
                }
                let end = end.ok_or("unterminated regular expression")?;
                i += 1 + end + 1;
                out.push(Token {
                    tok: Tok::Regex(s),
                    start,
                    end: i,
                });
                continue;
            }
            '[' | '{' => {
                let (tok, len) = range(c == '[', rest)?;
                i += 1 + len;
                out.push(Token { tok, start, end: i });
                continue;
            }
            ']' | '}' => return Err(format!("unexpected '{c}'")),
            _ => {
                // a term: up to the next character that ends one
                let mut chars = Vec::new();
                let mut it = src[i..].char_indices().peekable();
                let mut len = src.len() - i;
                while let Some((j, c)) = it.next() {
                    if c == '\\' {
                        match it.next() {
                            Some((_, e)) => chars.push((e, true)),
                            None => return Err("a query cannot end with '\\'".into()),
                        }
                    } else if ends_term(c) {
                        len = j;
                        break;
                    } else {
                        chars.push((c, false));
                    }
                }
                i += len;
                let plain: Option<String> = chars.iter().map(|&(c, e)| (!e).then_some(c)).collect();
                let tok = match plain.as_deref() {
                    Some("AND" | "&&") => Tok::And,
                    Some("OR" | "||") => Tok::Or,
                    Some("NOT") => Tok::Not,
                    _ => Tok::Term(chars),
                };
                out.push(Token { tok, start, end: i });
                continue;
            }
        };
        i += c.len_utf8();
        out.push(Token { tok, start, end: i });
    }
    Ok(out)
}

/// A range's bounds and closing bracket after its opening bracket; returns the token
/// and the length it took.
fn range(lo_incl: bool, s: &str) -> Res<(Tok, usize)> {
    let mut i = 0;
    let skip_ws = |i: &mut usize| {
        *i += s[*i..]
            .chars()
            .take_while(|c| c.is_whitespace())
            .map(char::len_utf8)
            .sum::<usize>()
    };
    let bound = |i: &mut usize| -> Res<String> {
        let rest = &s[*i..];
        if let Some(q) = rest.strip_prefix('"') {
            let end = q.find('"').ok_or("unterminated range bound")?;
            *i += end + 2;
            return Ok(q[..end].to_string());
        }
        let len = rest
            .find(|c: char| c.is_whitespace() || c == ']' || c == '}')
            .unwrap_or(rest.len());
        *i += len;
        Ok(rest[..len].to_string())
    };
    skip_ws(&mut i);
    let lo = bound(&mut i)?;
    skip_ws(&mut i);
    if !s[i..].starts_with("TO") {
        return Err("a range is written [lower TO upper]".into());
    }
    i += 2;
    skip_ws(&mut i);
    let hi = bound(&mut i)?;
    skip_ws(&mut i);
    let hi_incl = match s[i..].chars().next() {
        Some(']') => true,
        Some('}') => false,
        _ => return Err("a range is written [lower TO upper]".into()),
    };
    let open = |b: String| (b != "*").then_some(b);
    Ok((
        Tok::Range {
            lo: open(lo),
            hi: open(hi),
            lo_incl,
            hi_incl,
        },
        i + 1,
    ))
}

// ------------------------------------------------------------------------ parser --

#[derive(Debug, PartialEq)]
enum Ast {
    Group(Clauses),
    Boost(Box<Ast>, f32),
    /// a word, analyzed
    Word(String),
    Phrase {
        text: String,
        slop: u32,
        /// `"…"*`: the last word is a prefix
        prefix: bool,
    },
    /// a term's characters (each with whether it is a literal) with `*` or `?`
    Wildcard(Vec<(char, bool)>),
    Fuzzy(String, f32),
    Regex(String),
    Range {
        lo: Option<String>,
        hi: Option<String>,
        lo_incl: bool,
        hi_incl: bool,
    },
    All,
}

#[derive(Debug, PartialEq)]
struct Clauses {
    clauses: Vec<(Occur, Ast)>,
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Conj {
    None,
    And,
    Or,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.tok)
    }

    fn next(&mut self) -> Option<&Token> {
        let t = self.toks.get(self.pos);
        self.pos += 1;
        t
    }

    /// Lucene's `Query`: clauses joined by conjunctions, each with a modifier, combined
    /// by `QueryParserBase.addClause` with OR as the default operator.
    fn query(&mut self, depth: usize) -> Res<Clauses> {
        if depth > MAX_DEPTH {
            return Err(format!("parentheses nested deeper than {MAX_DEPTH}"));
        }
        let mut clauses: Vec<(Occur, Ast)> = Vec::new();
        loop {
            let conj = match self.peek() {
                None | Some(Tok::RParen) => break,
                Some(Tok::And) | Some(Tok::Or) if clauses.is_empty() => {
                    return Err(format!(
                        "unexpected {} at the start of a query",
                        self.peek().unwrap().describe()
                    ));
                }
                Some(Tok::And) => {
                    self.pos += 1;
                    Conj::And
                }
                Some(Tok::Or) => {
                    self.pos += 1;
                    Conj::Or
                }
                _ => Conj::None,
            };
            let (required, prohibited) = match self.peek() {
                Some(Tok::Plus) => (true, false),
                Some(Tok::Minus) | Some(Tok::Not) => (false, true),
                _ => (false, false),
            };
            if required || prohibited {
                self.pos += 1;
            }
            let q = self.clause(depth)?;
            // a clause introduced by AND makes the one before it required, unless that
            // one is excluded
            if conj == Conj::And
                && let Some((o, _)) = clauses.last_mut()
                && *o != Occur::MustNot
            {
                *o = Occur::Must;
            }
            let occur = if prohibited {
                Occur::MustNot
            } else if required || conj == Conj::And {
                Occur::Must
            } else {
                Occur::Should
            };
            clauses.push((occur, q));
        }
        Ok(Clauses { clauses })
    }

    fn boost(&mut self, q: Ast) -> Ast {
        match self.peek() {
            Some(Tok::Boost(b)) => {
                let b = *b;
                self.pos += 1;
                Ast::Boost(Box::new(q), b)
            }
            _ => q,
        }
    }

    fn clause(&mut self, depth: usize) -> Res<Ast> {
        let field = matches!(
            self.toks.get(self.pos + 1).map(|t| &t.tok),
            Some(Tok::Colon)
        );
        if field {
            if self.peek() == Some(&Tok::Term(vec![('*', false)])) {
                // in Jena, *:* matches every document of the index, whatever the
                // predicates of the call
                return Err(
                    "*:* is not supported; * alone matches every literal of the call's predicates"
                        .into(),
                );
            }
            let name = self.peek().map(Tok::describe).unwrap_or_default();
            return Err(format!(
                "field names ({name}:) are not supported; the predicates of text:query select what is searched (write \\: for a literal colon)"
            ));
        }
        if self.peek() == Some(&Tok::LParen) {
            self.pos += 1;
            let inner = self.query(depth + 1)?;
            match self.next().map(|t| &t.tok) {
                Some(Tok::RParen) => {}
                _ => return Err("missing ')'".into()),
            }
            return Ok(self.boost(Ast::Group(inner)));
        }
        let Some(t) = self.next() else {
            return Err("the query ends where a word should follow".into());
        };
        let end = t.end;
        let q = match t.tok.clone() {
            Tok::Term(chars) => {
                // Lucene takes the fuzzy marker before or after a boost
                let fuzzy = self.tilde();
                let boost = match self.peek() {
                    Some(Tok::Boost(b)) => {
                        let b = *b;
                        self.pos += 1;
                        Some(b)
                    }
                    _ => None,
                };
                let fuzzy = fuzzy.or_else(|| self.tilde());
                let wild = chars.iter().any(|&(c, e)| !e && (c == '*' || c == '?'));
                let q = if chars == [('*', false)] {
                    Ast::All
                } else if wild {
                    // as in Lucene, a wildcard ignores a fuzzy marker
                    Ast::Wildcard(chars)
                } else {
                    let text: String = chars.iter().map(|(c, _)| c).collect();
                    match fuzzy {
                        Some(s) => Ast::Fuzzy(text, s.unwrap_or(2.0)),
                        None => Ast::Word(text),
                    }
                };
                match boost {
                    Some(b) => Ast::Boost(Box::new(q), b),
                    None => q,
                }
            }
            Tok::Quoted(text) => {
                let prefix = matches!(
                    self.toks.get(self.pos),
                    Some(Token { tok: Tok::Term(c), start, .. })
                        if *start == end && c.as_slice() == [('*', false)]
                );
                if prefix {
                    self.pos += 1;
                }
                let slop = match self.tilde() {
                    Some(Some(s)) if prefix => {
                        return Err(format!("a phrase prefix cannot have a slop (~{s})"));
                    }
                    Some(s) => s.unwrap_or(0.0) as u32,
                    None => 0,
                };
                self.boost(Ast::Phrase { text, slop, prefix })
            }
            Tok::Regex(r) => self.boost(Ast::Regex(r)),
            Tok::Range {
                lo,
                hi,
                lo_incl,
                hi_incl,
            } => self.boost(Ast::Range {
                lo,
                hi,
                lo_incl,
                hi_incl,
            }),
            other => return Err(format!("unexpected {}", other.describe())),
        };
        Ok(q)
    }

    /// A `~` or `~n` after a term or phrase.
    fn tilde(&mut self) -> Option<Option<f32>> {
        match self.peek() {
            Some(Tok::Tilde(n)) => {
                let n = *n;
                self.pos += 1;
                Some(n)
            }
            _ => None,
        }
    }
}

// ----------------------------------------------------------------------- builder --

struct Builder<'a> {
    searcher: &'a Searcher,
    field: Field,
    analyzer: TextAnalyzer,
}

impl Builder<'_> {
    fn term(&self, t: &str) -> Term {
        Term::from_field_text(self.field, t)
    }

    fn term_query(&self, t: &str) -> Box<dyn Query> {
        Box::new(TermQuery::new(
            self.term(t),
            IndexRecordOption::WithFreqsAndPositions,
        ))
    }

    /// The tokens of `text` with their positions.
    fn tokens(&self, text: &str) -> Vec<(usize, String)> {
        let mut a = self.analyzer.clone();
        let mut stream = a.token_stream(text);
        let mut out = Vec::new();
        while let Some(t) = stream.next() {
            out.push((t.position, t.text.clone()));
        }
        out
    }

    /// The clauses of a query or group; `None` when none of them has a word.
    fn clauses(&self, c: &Clauses) -> Res<Option<Box<dyn Query>>> {
        let mut out: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for (o, ast) in &c.clauses {
            if let Some(q) = self.build(ast)? {
                out.push((*o, q));
            }
        }
        if out.is_empty() {
            return Ok(None);
        }
        if out.len() == 1 && out[0].0 != Occur::MustNot {
            return Ok(out.pop().map(|(_, q)| q));
        }
        // Lucene: without a required clause, one of the optional ones must match (a
        // group of excluded clauses alone matches nothing)
        let must = out.iter().any(|(o, _)| *o == Occur::Must);
        let should = out.iter().any(|(o, _)| *o == Occur::Should);
        let min = usize::from(!must && should);
        Ok(Some(Box::new(BooleanQuery::with_minimum_required_clauses(
            out, min,
        ))))
    }

    fn build(&self, ast: &Ast) -> Res<Option<Box<dyn Query>>> {
        Ok(match ast {
            Ast::Group(c) => self.clauses(c)?,
            Ast::Boost(q, b) => self
                .build(q)?
                .map(|q| Box::new(BoostQuery::new(q, *b)) as Box<dyn Query>),
            Ast::All => Some(Box::new(AllQuery)),
            Ast::Word(w) => {
                let toks = self.tokens(w);
                match toks.len() {
                    0 => None,
                    1 => Some(self.term_query(&toks[0].1)),
                    // Lucene ORs the parts of a word the analyzer splits
                    _ => Some(Box::new(BooleanQuery::new(
                        toks.iter()
                            .map(|(_, t)| (Occur::Should, self.term_query(t)))
                            .collect(),
                    ))),
                }
            }
            Ast::Phrase { text, slop, prefix } => {
                let toks = self.tokens(text);
                let first = toks.first().map_or(0, |(p, _)| *p);
                let terms: Vec<(usize, Term)> = toks
                    .iter()
                    .map(|(p, t)| (p - first, self.term(t)))
                    .collect();
                match (terms.len(), prefix) {
                    (0, _) => None,
                    (1, false) => Some(self.term_query(&toks[0].1)),
                    (1, true) => Some(self.regex(&format!("{}.*", regex::escape(&toks[0].1)))?),
                    (_, true) => Some(Box::new(PhrasePrefixQuery::new_with_offset(terms))),
                    (_, false) if *slop == 0 => Some(Box::new(PhraseQuery::new_with_offset(terms))),
                    (_, false) => Some(Box::new(super::sloppy::SloppyPhraseQuery::new(
                        terms, *slop,
                    )?)),
                }
            }
            Ast::Wildcard(chars) => {
                let mut re = String::new();
                let mut lit = String::new();
                let flush = |lit: &mut String, re: &mut String| {
                    re.push_str(&regex::escape(&normalize(lit)));
                    lit.clear();
                };
                for &(c, escaped) in chars {
                    match (c, escaped) {
                        ('*', false) => {
                            flush(&mut lit, &mut re);
                            re.push_str(".*");
                        }
                        ('?', false) => {
                            flush(&mut lit, &mut re);
                            re.push('.');
                        }
                        (c, _) => lit.push(c),
                    }
                }
                flush(&mut lit, &mut re);
                Some(self.regex(&re)?)
            }
            Ast::Fuzzy(w, sim) => Some(self.fuzzy(&normalize(w), *sim)?),
            Ast::Regex(r) => {
                lucene_regex(r)?;
                Some(self.regex(&normalize(r))?)
            }
            Ast::Range {
                lo,
                hi,
                lo_incl,
                hi_incl,
            } => {
                let bound = |b: &Option<String>, incl: bool| match b {
                    None => Bound::Unbounded,
                    Some(b) if incl => Bound::Included(self.term(&normalize(b))),
                    Some(b) => Bound::Excluded(self.term(&normalize(b))),
                };
                Some(Box::new(RangeQuery::new(
                    bound(lo, *lo_incl),
                    bound(hi, *hi_incl),
                )))
            }
        })
    }

    fn regex(&self, re: &str) -> Res<Box<dyn Query>> {
        Ok(Box::new(RegexQuery::from_pattern(re, self.field).map_err(
            |e| format!("invalid regular expression /{re}/: {e}"),
        )?))
    }

    /// Lucene's `FuzzyQuery`: the terms of the index within the edit distance (with
    /// transpositions), at most [`FUZZY_EXPANSIONS`] of them, the most similar first.
    fn fuzzy(&self, w: &str, sim: f32) -> Res<Box<dyn Query>> {
        let len = w.chars().count();
        // Lucene's FuzzyQuery.floatToEdits
        let edits = if sim >= 1.0 {
            if sim.fract() != 0.0 {
                return Err(format!(
                    "fractional edit distances are not allowed (~{sim})"
                ));
            }
            sim.min(2.0) as u8
        } else if sim == 0.0 {
            0
        } else {
            ((1.0 - f64::from(sim)) * len as f64).min(2.0) as u8
        };
        if edits == 0 {
            return Ok(self.term_query(w));
        }
        let dfa = automaton(edits).build_dfa(w);
        let mut found: Vec<Vec<u8>> = Vec::new();
        for seg in self.searcher.segment_readers() {
            let inv = seg.inverted_index(self.field).map_err(|e| e.to_string())?;
            let mut s = inv
                .terms()
                .search(Dfa(&dfa))
                .into_stream()
                .map_err(|e| e.to_string())?;
            while s.advance() {
                found.push(s.key().to_vec());
            }
        }
        found.sort_unstable();
        found.dedup();
        // Lucene's similarity: 1 - edits / the shorter length
        let mut scored: Vec<(f32, &str)> = found
            .iter()
            .filter_map(|t| {
                let t = std::str::from_utf8(t).ok()?;
                let Distance::Exact(d) = dfa.eval(t) else {
                    return None;
                };
                let sim = if d == 0 {
                    1.0
                } else {
                    1.0 - f32::from(d) / t.chars().count().min(len) as f32
                };
                Some((sim, t))
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(b.1)));
        scored.truncate(FUZZY_EXPANSIONS);
        Ok(Box::new(BooleanQuery::new(
            scored
                .iter()
                .map(|(_, t)| (Occur::Should, self.term_query(t)))
                .collect(),
        )))
    }
}

/// Lowercase and ASCII-fold like the indexed tokens, without splitting.
fn normalize(s: &str) -> String {
    let mut a = super::imp::normalizer();
    let mut stream = a.token_stream(s);
    let mut out = String::new();
    while let Some(t) = stream.next() {
        out.push_str(&t.text);
    }
    out
}

/// Refuse the operators of Lucene's regular expressions that Tantivy's lack or read
/// differently: `@` (any string), `#` (the empty language), `<n-m>` (numeric
/// intervals), `&` (intersection) and `~` (complement), and anchors.
fn lucene_regex(r: &str) -> Res<()> {
    let mut class = false;
    let mut chars = r.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '[' => class = true,
            ']' => class = false,
            '@' | '#' | '<' | '>' | '&' | '~' | '$' if !class => {
                return Err(format!(
                    "the regular expression operator '{c}' is not supported (escape it to match it)"
                ));
            }
            '^' if !class => {
                return Err("regular expressions match whole terms and take no '^' anchor".into());
            }
            _ => {}
        }
    }
    Ok(())
}

/// Levenshtein automata with transpositions for 1 and 2 edits (expensive to build).
fn automaton(edits: u8) -> &'static LevenshteinAutomatonBuilder {
    static B: [OnceLock<LevenshteinAutomatonBuilder>; 2] = [OnceLock::new(), OnceLock::new()];
    B[usize::from(edits.clamp(1, 2) - 1)]
        .get_or_init(|| LevenshteinAutomatonBuilder::new(edits.clamp(1, 2), true))
}

/// A Levenshtein automaton over the term dictionary.
struct Dfa<'a>(&'a DFA);

impl tantivy_fst::Automaton for Dfa<'_> {
    type State = u32;

    fn start(&self) -> u32 {
        self.0.initial_state()
    }

    fn is_match(&self, state: &u32) -> bool {
        matches!(self.0.distance(*state), Distance::Exact(_))
    }

    fn can_match(&self, state: &u32) -> bool {
        *state != levenshtein_automata::SINK_STATE
    }

    fn accept(&self, state: &u32, byte: u8) -> u32 {
        self.0.transition(*state, byte)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ast(s: &str) -> Clauses {
        let toks = lex(s).unwrap();
        let mut p = Parser { toks, pos: 0 };
        let q = p.query(0).unwrap();
        assert_eq!(p.pos, p.toks.len(), "{s}");
        q
    }

    fn occurs(s: &str) -> Vec<Occur> {
        ast(s).clauses.iter().map(|(o, _)| *o).collect()
    }

    fn word(w: &str) -> Ast {
        Ast::Word(w.into())
    }

    #[test]
    fn combines_clauses_like_lucene() {
        use Occur::*;
        assert_eq!(occurs("a b"), [Should, Should]);
        assert_eq!(occurs("a AND b"), [Must, Must]);
        assert_eq!(occurs("a && b"), [Must, Must]);
        assert_eq!(occurs("a OR b AND c"), [Should, Must, Must]);
        assert_eq!(occurs("a AND b OR c"), [Must, Must, Should]);
        assert_eq!(occurs("+a -b !c"), [Must, MustNot, MustNot]);
        assert_eq!(occurs("a AND NOT b"), [Must, MustNot]);
        assert_eq!(occurs("-a AND b"), [MustNot, Must]);
        assert_eq!(occurs("a NOT b"), [Should, MustNot]);
        assert_eq!(occurs("a || b"), [Should, Should]);
    }

    #[test]
    fn reads_terms() {
        assert_eq!(ast("foo-bar").clauses[0].1, word("foo-bar"));
        assert_eq!(ast("ANDY").clauses[0].1, word("ANDY"));
        assert_eq!(ast("a\\:b").clauses[0].1, word("a:b"));
        assert_eq!(
            ast("al*").clauses[0].1,
            Ast::Wildcard(vec![('a', false), ('l', false), ('*', false)])
        );
        assert_eq!(
            ast("a\\*").clauses[0].1,
            Ast::Word("a*".into()),
            "an escaped * is a literal"
        );
        assert_eq!(ast("roam~").clauses[0].1, Ast::Fuzzy("roam".into(), 2.0));
        assert_eq!(ast("roam~1").clauses[0].1, Ast::Fuzzy("roam".into(), 1.0));
        assert_eq!(ast("roam~0.5").clauses[0].1, Ast::Fuzzy("roam".into(), 0.5));
        assert_eq!(
            ast("roam^2~1").clauses[0].1,
            Ast::Boost(Box::new(Ast::Fuzzy("roam".into(), 1.0)), 2.0)
        );
        assert_eq!(
            ast("\"a b\"~3").clauses[0].1,
            Ast::Phrase {
                text: "a b".into(),
                slop: 3,
                prefix: false
            }
        );
        assert_eq!(
            ast("\"a b\"*").clauses[0].1,
            Ast::Phrase {
                text: "a b".into(),
                slop: 0,
                prefix: true
            }
        );
        assert_eq!(ast("*").clauses[0].1, Ast::All);
        assert_eq!(ast("/a.c/").clauses[0].1, Ast::Regex("a.c".into()));
        assert_eq!(
            ast("[a TO *}").clauses[0].1,
            Ast::Range {
                lo: Some("a".into()),
                hi: None,
                lo_incl: true,
                hi_incl: false
            }
        );
        let Ast::Group(g) = &ast("(a OR b)^2").clauses[0].1 else {
            // a boosted group
            let Ast::Boost(inner, b) = &ast("(a OR b)^2").clauses[0].1 else {
                panic!()
            };
            assert_eq!(*b, 2.0);
            assert!(matches!(**inner, Ast::Group(_)));
            return;
        };
        panic!("{g:?}");
    }

    #[test]
    fn refuses_malformed_queries() {
        for q in [
            "(a", "a)", "\"a", "AND a", "a AND", "name:ada", "a^", "[a b]", "/a", "a\\",
        ] {
            let r = lex(q).and_then(|toks| {
                let mut p = Parser { toks, pos: 0 };
                let a = p.query(0)?;
                match p.toks.get(p.pos) {
                    Some(t) => Err(format!("unexpected {}", t.tok.describe())),
                    None => Ok(a),
                }
            });
            assert!(r.is_err(), "{q}: {r:?}");
        }
        assert!(lucene_regex("a@").is_err());
        assert!(lucene_regex("^a").is_err());
        assert!(lucene_regex("[^a]b").is_ok());
        assert!(lucene_regex("a\\@").is_ok());
    }

    #[test]
    fn normalizes_like_the_index() {
        assert_eq!(normalize("Café"), "cafe");
        assert_eq!(normalize("AL*"), "al*");
    }
}
