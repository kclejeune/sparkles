//! The formatting pipeline and its safety checks: a reference parse of the input (its
//! errors are the user's syntax errors), the lossless parse and printing, then the
//! equivalence, comment and idempotence checks. Any failure after the reference parse
//! refuses the output.

pub mod algebra;
pub mod comments;
pub mod graph;
pub mod json;

use crate::doc::Printed;
use crate::lex::{LexMode, Token, TokenKind, lex};
use crate::sparql::Unit;
use crate::sparql::keywords::Kw;
use crate::tree::Tree;
use crate::trivia::{CommentRules, Comments};
use crate::{Check, FormatError, Formatted, Language, Options, Warning};
use spargebra::SparqlParser;
use std::collections::HashSet;
use std::time::Instant;

/// One language's part of the pipeline.
pub trait LangImpl {
    /// What the reference parser made of the input.
    type Reference;

    fn language(&self) -> Language;

    fn lex_mode(&self) -> LexMode;

    /// Parse the input with the reference parser.
    fn reference(&self, text: &str, tokens: &[Token]) -> Result<Self::Reference, FormatError>;

    /// Warnings of the reference parse (undeclared prefixes …).
    fn warnings(&self, r: &Self::Reference) -> Vec<Warning>;

    /// The lossless syntax tree; [`FormatError::Unsupported`] when the formatter's parser
    /// rejects what the reference parser accepted.
    fn cst<'s>(
        &self,
        text: &'s str,
        tokens: Vec<Token>,
        r: &Self::Reference,
    ) -> Result<Tree<'s>, FormatError>;

    fn rules(&self) -> &dyn CommentRules;

    /// Normalize and print the tree.
    fn print(
        &self,
        tree: &Tree<'_>,
        comments: &Comments,
        opts: &Options,
    ) -> Result<Printed, FormatError>;

    /// `Ok` when `output` means the same as the input ([`FormatError::Unsafe`] if not).
    fn equivalent(&self, r: &Self::Reference, output: &str) -> Result<(), FormatError>;
}

/// Format `text` with `lang`, checking the result.
pub fn run<L: LangImpl>(lang: &L, text: &str, opts: &Options) -> Result<Formatted, FormatError> {
    // a cursor inside a character means the start of that character
    let snapped = opts
        .cursor
        .map(|c| text.floor_char_boundary(c.min(text.len())));
    let opts = &Options {
        cursor: snapped,
        ..opts.clone()
    };
    let mode = lang.lex_mode();
    let tokens = lex(text, mode);
    // kept byte for byte, without even a reference parse
    if crate::pragma::ignore_file(text, &tokens) {
        return Ok(Formatted {
            text: text.to_string(),
            changed: false,
            cursor: opts.cursor.map(|c| c.min(text.len())),
            language: lang.language(),
            warnings: Vec::new(),
        });
    }
    let r = lang.reference(text, &tokens)?;
    let mut warnings = lang.warnings(&r);
    warnings.extend(crate::option_warnings(opts, lang.language()));
    deadline(opts.deadline)?;

    let (tree, printed, moved) = format_once(lang, text, tokens, &r, opts)?;
    warnings.extend(moved);
    let changed = printed.text != text;
    if changed {
        deadline(opts.deadline)?;
        lang.equivalent(&r, &printed.text)?;
        comments::same(text, &printed.text, mode)?;
        deadline(opts.deadline)?;
        // the output formats to itself
        let again =
            format_once(lang, &printed.text, lex(&printed.text, mode), &r, opts).map_err(|e| {
                match e {
                    FormatError::Timeout => FormatError::Timeout,
                    _ => FormatError::Unsafe {
                        check: Check::Idempotence,
                    },
                }
            })?;
        if again.1.text != printed.text {
            return Err(FormatError::Unsafe {
                check: Check::Idempotence,
            });
        }
    }
    let cursor = opts.cursor.map(|c| match changed {
        false => c.min(text.len()),
        true => crate::cursor::map(&tree, &printed, c),
    });
    Ok(Formatted {
        text: printed.text,
        changed,
        cursor,
        language: lang.language(),
        warnings,
    })
}

/// Lossless parse, comments, printing: the tree, the output and the `comment-moved`
/// warnings.
fn format_once<'s, L: LangImpl>(
    lang: &L,
    text: &'s str,
    tokens: Vec<Token>,
    r: &L::Reference,
    opts: &Options,
) -> Result<(Tree<'s>, Printed, Vec<Warning>), FormatError> {
    let tree = lang.cst(text, tokens, r)?;
    let comments = Comments::attach(&tree, lang.rules());
    let printed = lang.print(&tree, &comments, opts)?;
    #[cfg(feature = "fault-injection")]
    let printed = fault::inject(&tree, printed);
    Ok((tree, printed, comments.warnings().to_vec()))
}

pub(crate) fn deadline(d: Option<Instant>) -> Result<(), FormatError> {
    match d {
        Some(d) if Instant::now() > d => Err(FormatError::Timeout),
        _ => Ok(()),
    }
}

#[cfg(feature = "fault-injection")]
mod fault {
    use crate::doc::Printed;
    use crate::lex::TokenKind;
    use crate::tree::Tree;

    /// With `SPARKLES_FMT_FAULT=drop-token`, drop the first variable, IRI, prefixed name
    /// or string of the output.
    pub(super) fn inject(tree: &Tree<'_>, mut printed: Printed) -> Printed {
        if std::env::var("SPARKLES_FMT_FAULT").as_deref() != Ok("drop-token") {
            return printed;
        }
        let victim = printed.tok_out.iter().position(|&(id, ..)| {
            let kind = tree.token_kind(id);
            kind.is_string()
                || matches!(
                    kind,
                    TokenKind::Var1 | TokenKind::Var2 | TokenKind::IriRef | TokenKind::PnameLn
                )
        });
        if let Some(i) = victim {
            let (_, start, len) = printed.tok_out.remove(i);
            printed
                .text
                .replace_range(start as usize..(start + len) as usize, "");
            for t in &mut printed.tok_out[i..] {
                t.1 -= len;
            }
        }
        printed
    }
}

// --------------------------------------------------------------------- SPARQL ------

/// The base IRI of both parses, so relative IRIs resolve (a `BASE` in the text wins).
pub const SPARQL_BASE: &str = "http://sparkles-fmt.invalid/base/";

/// The namespace an undeclared prefix gets in both parses. The label is percent-encoded
/// beyond ASCII letters, digits, `-`, `_` and `.`: a prefix label may hold characters an
/// IRI may not (U+FFF0 to U+FFFD).
pub fn undeclared_namespace(label: &str) -> String {
    let mut ns = String::from("http://sparkles-fmt.invalid/prefix/");
    for b in label.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => ns.push(b as char),
            _ => ns.push_str(&format!("%{b:02X}")),
        }
    }
    ns.push('/');
    ns
}

/// The algebra of a SPARQL document.
#[derive(Clone, Debug)]
pub enum Algebra {
    Query(Box<spargebra::Query>),
    Update(spargebra::Update),
}

/// The reference parse of a SPARQL document.
#[derive(Clone, Debug)]
pub struct SparqlReference {
    pub unit: Unit,
    pub algebra: Algebra,
    /// prefix labels used but never declared, registered for both parses
    pub undeclared: Vec<String>,
    /// the names (without sigil) of the input's variable tokens: any other variable in
    /// the algebra is one the parser made up
    pub vars: HashSet<String>,
    pub warnings: Vec<Warning>,
}

/// Parse a SPARQL query or update with spargebra: a query when the first keyword after
/// the prologue is a query form, an update for an update keyword (or nothing: a prologue
/// alone is an empty update request); otherwise whichever parse gets further.
pub fn sparql_reference(text: &str, tokens: &[Token]) -> Result<SparqlReference, FormatError> {
    let undeclared = undeclared_prefixes(text, tokens);
    let names: Vec<String> = undeclared.iter().map(|(l, _)| l.clone()).collect();
    let (unit, algebra) = match sparql_unit(text, tokens) {
        Some(unit) => (unit, sparql_parse(text, unit, &names)?),
        None => match sparql_parse(text, Unit::Query, &names) {
            Ok(a) => (Unit::Query, a),
            Err(q) => match sparql_parse(text, Unit::Update, &names) {
                Ok(a) => (Unit::Update, a),
                Err(u) => return Err(further(q, u)),
            },
        },
    };
    let warnings = undeclared
        .iter()
        .map(|(label, offset)| {
            let (line, column) = crate::line_col(text, *offset);
            Warning {
                code: "undeclared-prefix",
                message: format!(
                    "the prefix {label}: is not declared here; it is formatted as declared elsewhere"
                ),
                line,
                column,
            }
        })
        .collect();
    let vars = tokens
        .iter()
        .filter(|t| matches!(t.kind, TokenKind::Var1 | TokenKind::Var2))
        .map(|t| t.text(text)[1..].to_string())
        .collect();
    Ok(SparqlReference {
        unit,
        algebra,
        undeclared: names,
        vars,
        warnings,
    })
}

/// `Ok` when `output` parses (with the input's settings) to the input's algebra.
pub fn sparql_equivalent(r: &SparqlReference, output: &str) -> Result<(), FormatError> {
    let unsafe_ = FormatError::Unsafe {
        check: Check::Algebra,
    };
    let out = sparql_parse(output, r.unit, &r.undeclared).map_err(|_| unsafe_.clone())?;
    if algebra::equivalent(&r.algebra, &out, &r.vars) {
        Ok(())
    } else {
        Err(unsafe_)
    }
}

/// Parse `text` (a BOM dropped) as `unit`, with the synthetic base and the undeclared
/// prefixes.
pub fn sparql_parse(text: &str, unit: Unit, undeclared: &[String]) -> Result<Algebra, FormatError> {
    let bom = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let body = &text[bom..];
    let mut p = SparqlParser::new()
        .with_base_iri(SPARQL_BASE)
        .expect("a valid base IRI");
    for label in undeclared {
        p = p
            .with_prefix(label.as_str(), undeclared_namespace(label))
            .expect("a valid namespace IRI");
    }
    let parsed = match unit {
        Unit::Query => p.parse_query(body).map(|q| Algebra::Query(Box::new(q))),
        Unit::Update => p.parse_update(body).map(Algebra::Update),
    };
    parsed.map_err(|e| syntax_error(text, bom, &e.to_string()))
}

/// A [`FormatError::Syntax`] from spargebra's message, which reads
/// `error at LINE:COLUMN: expected …` (peg's 1-based line and character column, in the
/// text after the BOM). Other messages (a blank node shared by two data blocks) have no
/// position: 1:1.
fn syntax_error(text: &str, bom: usize, msg: &str) -> FormatError {
    let positioned = msg.strip_prefix("error at ").and_then(|rest| {
        let (line, rest) = rest.split_once(':')?;
        let (column, rest) = rest.split_once(':')?;
        Some((line.parse::<u32>().ok()?, column.parse::<u32>().ok()?, rest))
    });
    match positioned {
        Some((line, column, rest)) => FormatError::Syntax {
            message: rest.trim().to_string(),
            line,
            column,
            offset: crate::offset_of(text, line, column),
        },
        None => FormatError::Syntax {
            message: msg.to_string(),
            line: 1,
            column: 1,
            offset: bom,
        },
    }
}

/// Of two syntax errors, the one further into the text.
fn further(a: FormatError, b: FormatError) -> FormatError {
    let offset = |e: &FormatError| match e {
        FormatError::Syntax { offset, .. } => *offset,
        _ => 0,
    };
    if offset(&b) > offset(&a) { b } else { a }
}

/// Query or update, by the first keyword after the prologue; `None` when it is neither.
pub fn sparql_unit(text: &str, tokens: &[Token]) -> Option<Unit> {
    let mut sig = tokens.iter().filter(|t| !t.kind.is_trivia());
    loop {
        let t = sig.next()?;
        let kw = match t.kind {
            TokenKind::Eof => return Some(Unit::Update),
            TokenKind::Word => Kw::from_word(t.text(text))?,
            _ => return None,
        };
        match kw {
            Kw::Prefix => {
                sig.next();
                sig.next();
            }
            Kw::Base | Kw::Version => {
                sig.next();
            }
            Kw::Select | Kw::Construct | Kw::Describe | Kw::Ask => return Some(Unit::Query),
            Kw::Insert
            | Kw::Delete
            | Kw::With
            | Kw::Load
            | Kw::Clear
            | Kw::Drop
            | Kw::Create
            | Kw::Add
            | Kw::Move
            | Kw::Copy => return Some(Unit::Update),
            _ => return None,
        }
    }
}

/// The prefix labels used but never declared (`PREFIX label:` anywhere), with the byte
/// offset of their first use, in order of first use.
pub fn undeclared_prefixes(text: &str, tokens: &[Token]) -> Vec<(String, usize)> {
    let sig: Vec<&Token> = tokens.iter().filter(|t| !t.kind.is_trivia()).collect();
    let mut declared = HashSet::new();
    let mut used: Vec<(String, usize)> = Vec::new();
    for (i, t) in sig.iter().enumerate() {
        if !matches!(t.kind, TokenKind::PnameNs | TokenKind::PnameLn) {
            continue;
        }
        let s = t.text(text);
        let label = &s[..s.find(':').expect("a prefixed name has a colon")];
        let after_prefix = i > 0
            && sig[i - 1].kind == TokenKind::Word
            && Kw::from_word(sig[i - 1].text(text)) == Some(Kw::Prefix);
        if after_prefix && t.kind == TokenKind::PnameNs {
            declared.insert(label.to_string());
        } else if !used.iter().any(|(l, _)| l == label) {
            used.push((label.to_string(), t.start as usize));
        }
    }
    used.retain(|(l, _)| !declared.contains(l));
    used
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(text: &str) -> Result<SparqlReference, FormatError> {
        sparql_reference(text, &lex(text, LexMode::Sparql))
    }

    #[test]
    fn query_or_update() {
        assert_eq!(reference("select * {}").unwrap().unit, Unit::Query);
        assert_eq!(
            reference("PREFIX ex: <http://e/> BASE <x> ASK {}")
                .unwrap()
                .unit,
            Unit::Query
        );
        assert_eq!(reference("CLEAR ALL").unwrap().unit, Unit::Update);
        assert_eq!(reference("").unwrap().unit, Unit::Update);
        assert_eq!(
            reference("# c\nPREFIX ex: <http://e/>").unwrap().unit,
            Unit::Update
        );
        let e = reference("FOO").unwrap_err();
        assert!(matches!(e, FormatError::Syntax { line: 1, .. }), "{e:?}");
    }

    #[test]
    fn syntax_positions() {
        let e = reference("select * {").unwrap_err();
        let FormatError::Syntax {
            line,
            column,
            offset,
            message,
        } = e
        else {
            panic!("{e:?}")
        };
        assert_eq!((line, column, offset), (1, 11, 10));
        assert!(message.starts_with("expected"), "{message}");

        // positions are in the text after the BOM, offsets in the text
        let text = "\u{feff}SELECT *\nWHERE { ?s ?p }";
        let e = reference(text).unwrap_err();
        let FormatError::Syntax {
            line,
            column,
            offset,
            ..
        } = e
        else {
            panic!("{e:?}")
        };
        assert_eq!(line, 2);
        assert!(column > 10, "{column}");
        assert_eq!(offset, "\u{feff}SELECT *\n".len() + column as usize - 1);

        // a shared blank node has no position
        let e = reference("INSERT DATA { _:a <p:a> 1 } ; INSERT DATA { _:a <p:b> 2 }").unwrap_err();
        assert!(
            matches!(
                e,
                FormatError::Syntax {
                    line: 1,
                    column: 1,
                    ..
                }
            ),
            "{e:?}"
        );
    }

    #[test]
    fn undeclared_prefixes_parse_and_warn() {
        let r =
            reference("PREFIX ex: <http://e/>\nSELECT * { ?s ex:p foaf:name ; :q dc: }").unwrap();
        assert_eq!(r.undeclared, ["foaf", "", "dc"]);
        assert_eq!(r.warnings.len(), 3);
        assert_eq!(r.warnings[0].code, "undeclared-prefix");
        assert_eq!((r.warnings[0].line, r.warnings[0].column), (2, 20));
        // a label declared anywhere is not registered (a use before it stays an error)
        assert!(
            reference("INSERT DATA { ex:a ex:b ex:c } ; PREFIX ex: <http://e/> CLEAR ALL").is_err()
        );
        assert!(r.vars.contains("s"));
    }

    #[test]
    fn relative_iris_resolve() {
        assert!(reference("SELECT * { <a> <b> <c> }").is_ok());
    }
}
