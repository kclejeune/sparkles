//! The functional-style syntax reader (structural specification §3–§10, with the
//! grammar of its Appendix 1).
//!
//! [`Reader`] reads an ontology document one axiom at a time: [`Reader::new`] reads the
//! prefix declarations and the ontology header, and the reader is then an iterator over
//! the axioms, so a caller that maps each axiom to triples holds one axiom at a time.
//! [`parse`] collects a whole [`Document`]. [`triples`] maps a document to RDF with the
//! forward mapping of [`crate::write`].
//!
//! The prefixes `owl:`, `rdf:`, `rdfs:` and `xsd:` are predefined, and a document may
//! declare them again. A literal typed `rdf:PlainLiteral` is read as the plain or
//! language-tagged literal it stands for (§5.7). Every axiom is checked with
//! [`crate::validate::axiom`], so that what the reader returns can be written to triples.
//! Errors name the line and column of the token they are about.

use crate::model::*;
use crate::read::MAX_NESTING;
use crate::vocab;
use std::collections::BTreeSet;
use std::fmt;

/// A syntax error, with the 1-based line and column (in characters) where it was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for ParseError {}

/// A whole ontology document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Document {
    /// the prefix declarations, in document order (the predefined ones are not listed)
    pub prefixes: Vec<(String, String)>,
    pub header: Header,
    /// the axioms in document order; an axiom written twice is listed twice
    pub axioms: Vec<Axiom>,
}

/// Read a whole document.
pub fn parse(text: &str) -> Result<Document, ParseError> {
    let mut r = Reader::new(text)?;
    let mut axioms = Vec::new();
    for ax in r.by_ref() {
        axioms.push(ax?);
    }
    Ok(Document {
        prefixes: r.prefixes,
        header: r.header,
        axioms,
    })
}

/// Read one axiom, such as `SubClassOf(ex:A ex:B)`, with `prefixes` and the predefined
/// ones (a prefix of `prefixes` wins over a predefined one of the same name).
pub fn parse_axiom(text: &str, prefixes: &[(String, String)]) -> Result<Axiom, ParseError> {
    let mut r = Reader {
        text,
        pos: 0,
        peeked: None,
        last: 0,
        names: crate::ofn::STANDARD_PREFIXES
            .iter()
            .map(|(p, n)| (p.to_string(), n.to_string()))
            .chain(prefixes.iter().cloned())
            .collect(),
        prefixes: Vec::new(),
        header: Header::default(),
        depth: 0,
        done: false,
    };
    let ax = r.axiom()?;
    match Reader::take(&mut r)? {
        Tok::End => Ok(ax),
        t => Err(r.error(format!(
            "expected the end of the axiom, found {}",
            t.describe()
        ))),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok<'a> {
    Open,
    Close,
    Equals,
    Carets,
    /// a full IRI, without its angle brackets
    Full(&'a str),
    /// a prefixed name or a keyword: a keyword has no colon
    Name(&'a str),
    /// a quoted string, unescaped
    Str(String),
    /// a language tag, without its `@`
    Lang(&'a str),
    /// a blank node label, without its `_:`
    Node(&'a str),
    Int(&'a str),
    End,
}

impl Tok<'_> {
    fn describe(&self) -> String {
        match self {
            Tok::Open => "'('".into(),
            Tok::Close => "')'".into(),
            Tok::Equals => "'='".into(),
            Tok::Carets => "'^^'".into(),
            Tok::Full(i) => format!("<{i}>"),
            Tok::Name(n) => format!("'{n}'"),
            Tok::Str(_) => "a quoted string".into(),
            Tok::Lang(l) => format!("'@{l}'"),
            Tok::Node(n) => format!("'_:{n}'"),
            Tok::Int(n) => format!("'{n}'"),
            Tok::End => "the end of the document".into(),
        }
    }
}

/// The characters that end a prefixed name, a keyword or a number.
fn ends_name(c: char) -> bool {
    c.is_whitespace() || "()=\"<>^#@".contains(c)
}

/// A reader of one ontology document.
pub struct Reader<'a> {
    text: &'a str,
    pos: usize,
    /// the token read ahead, and its start
    peeked: Option<(Tok<'a>, usize)>,
    /// the start of the last token taken
    last: usize,
    /// every prefix that resolves, the predefined ones first
    names: Vec<(String, String)>,
    prefixes: Vec<(String, String)>,
    header: Header,
    depth: usize,
    done: bool,
}

impl<'a> Reader<'a> {
    /// Read the prefix declarations and the ontology header, up to the first axiom.
    pub fn new(text: &'a str) -> Result<Reader<'a>, ParseError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut r = Reader {
            text,
            pos: 0,
            peeked: None,
            last: 0,
            names: crate::ofn::STANDARD_PREFIXES
                .iter()
                .map(|(p, n)| (p.to_string(), n.to_string()))
                .collect(),
            prefixes: Vec::new(),
            header: Header::default(),
            depth: 0,
            done: false,
        };
        r.preamble()?;
        Ok(r)
    }

    /// The prefix declarations, in document order.
    pub fn prefixes(&self) -> &[(String, String)] {
        &self.prefixes
    }

    /// The ontology header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    // ---- errors and positions

    fn error_at(&self, at: usize, message: impl Into<String>) -> ParseError {
        let before = &self.text[..at.min(self.text.len())];
        let line = before.matches('\n').count() + 1;
        let column = before
            .rsplit_once('\n')
            .map_or(before, |(_, l)| l)
            .chars()
            .count()
            + 1;
        ParseError {
            line,
            column,
            message: message.into(),
        }
    }

    fn error(&self, message: impl Into<String>) -> ParseError {
        self.error_at(self.last, message)
    }

    // ---- the lexer

    fn skip_space(&mut self) {
        let b = self.text.as_bytes();
        while self.pos < b.len() {
            match b[self.pos] {
                b' ' | b'\t' | b'\r' | b'\n' => self.pos += 1,
                b'#' => {
                    while self.pos < b.len() && b[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                }
                _ => {
                    // other Unicode white space
                    let c = self.text[self.pos..].chars().next().unwrap_or('x');
                    if c.is_whitespace() {
                        self.pos += c.len_utf8();
                    } else {
                        break;
                    }
                }
            }
        }
    }

    fn lex(&mut self) -> Result<(Tok<'a>, usize), ParseError> {
        self.skip_space();
        let start = self.pos;
        let rest = &self.text[self.pos..];
        let Some(c) = rest.chars().next() else {
            return Ok((Tok::End, start));
        };
        let tok = match c {
            '(' => {
                self.pos += 1;
                Tok::Open
            }
            ')' => {
                self.pos += 1;
                Tok::Close
            }
            '=' => {
                self.pos += 1;
                Tok::Equals
            }
            '^' => {
                if !rest.starts_with("^^") {
                    return Err(self.error_at(start, "'^' is not followed by '^'"));
                }
                self.pos += 2;
                Tok::Carets
            }
            '<' => {
                let Some(end) = rest.find('>') else {
                    return Err(self.error_at(start, "an IRI is not closed with '>'"));
                };
                let iri = &rest[1..end];
                if let Some(bad) = iri.chars().find(|c| c.is_whitespace() || *c == '<') {
                    return Err(self.error_at(
                        start,
                        format!("an IRI holds the character {bad:?}, or is not closed with '>'"),
                    ));
                }
                self.pos += end + 1;
                Tok::Full(iri)
            }
            '"' => {
                let mut s = String::new();
                let mut chars = rest.char_indices().skip(1);
                loop {
                    match chars.next() {
                        None => return Err(self.error_at(start, "a string is not closed")),
                        Some((i, '"')) => {
                            self.pos += i + 1;
                            break;
                        }
                        Some((_, '\\')) => match chars.next() {
                            Some((_, c @ ('"' | '\\'))) => s.push(c),
                            Some((i, c)) => {
                                return Err(self.error_at(
                                    start + i,
                                    format!(
                                        "'\\{c}' is not an escape of the syntax, which escapes only '\"' and '\\'"
                                    ),
                                ));
                            }
                            None => return Err(self.error_at(start, "a string is not closed")),
                        },
                        Some((_, c)) => s.push(c),
                    }
                }
                Tok::Str(s)
            }
            '@' => {
                let end = rest[1..]
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                    .map_or(rest.len(), |e| e + 1);
                if end == 1 {
                    return Err(self.error_at(start, "'@' is not followed by a language tag"));
                }
                self.pos += end;
                Tok::Lang(&rest[1..end])
            }
            _ if rest.starts_with("_:") => {
                let end = rest[2..].find(ends_name).map_or(rest.len(), |e| e + 2);
                if end == 2 {
                    return Err(self.error_at(start, "'_:' is not followed by a label"));
                }
                self.pos += end;
                Tok::Node(&rest[2..end])
            }
            _ => {
                let end = rest.find(ends_name).unwrap_or(rest.len());
                if end == 0 {
                    return Err(self.error_at(start, format!("unexpected character {c:?}")));
                }
                self.pos += end;
                let word = &rest[..end];
                if word.bytes().all(|b| b.is_ascii_digit()) {
                    Tok::Int(word)
                } else {
                    Tok::Name(word)
                }
            }
        };
        Ok((tok, start))
    }

    fn peek(&mut self) -> Result<&Tok<'a>, ParseError> {
        if self.peeked.is_none() {
            self.peeked = Some(self.lex()?);
        }
        Ok(&self.peeked.as_ref().expect("a token read ahead").0)
    }

    fn take(&mut self) -> Result<Tok<'a>, ParseError> {
        let (t, at) = match self.peeked.take() {
            Some(x) => x,
            None => self.lex()?,
        };
        self.last = at;
        Ok(t)
    }

    fn expect(&mut self, want: Tok<'static>) -> Result<(), ParseError> {
        let t = self.take()?;
        if t == want {
            Ok(())
        } else {
            Err(self.error(format!(
                "expected {}, found {}",
                want.describe(),
                t.describe()
            )))
        }
    }

    fn open(&mut self) -> Result<(), ParseError> {
        self.expect(Tok::Open)
    }

    fn close(&mut self) -> Result<(), ParseError> {
        self.expect(Tok::Close)
    }

    fn at_close(&mut self) -> Result<bool, ParseError> {
        Ok(matches!(self.peek()?, Tok::Close))
    }

    /// Whether the next token is the keyword `k`.
    fn at_keyword(&mut self, k: &str) -> Result<bool, ParseError> {
        Ok(matches!(self.peek()?, Tok::Name(n) if *n == k))
    }

    // ---- IRIs

    fn full_iri(&self, s: &str) -> Result<Iri, ParseError> {
        parse_iri(s).map_err(|e| self.error(e.0))
    }

    /// The IRI of a prefixed name.
    fn expand(&self, name: &str) -> Result<Iri, ParseError> {
        let Some((prefix, local)) = name.split_once(':') else {
            return Err(self.error(format!("expected an IRI, found the keyword '{name}'")));
        };
        let Some((_, ns)) = self.names.iter().rev().find(|(p, _)| p == prefix) else {
            return Err(self.error(format!("the prefix '{prefix}:' is not declared")));
        };
        let mut s = String::with_capacity(ns.len() + local.len());
        s.push_str(ns);
        // the escapes of a local name (PN_LOCAL_ESC) stand for the character
        let mut chars = local.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some(c) => s.push(c),
                    None => return Err(self.error("a local name ends with '\\'")),
                }
            } else {
                s.push(c);
            }
        }
        parse_iri(&s).map_err(|e| self.error(e.0))
    }

    fn iri(&mut self) -> Result<Iri, ParseError> {
        match self.take()? {
            Tok::Full(s) => self.full_iri(s),
            Tok::Name(n) => self.expand(n),
            t => Err(self.error(format!("expected an IRI, found {}", t.describe()))),
        }
    }

    /// Whether the next token is an IRI.
    fn at_iri(&mut self) -> Result<bool, ParseError> {
        Ok(match self.peek()? {
            Tok::Full(_) => true,
            Tok::Name(n) => n.contains(':'),
            _ => false,
        })
    }

    /// The keyword of a constructor, followed by its `(`.
    fn keyword(&mut self) -> Result<&'a str, ParseError> {
        match self.take()? {
            Tok::Name(n) if !n.contains(':') => {
                self.open()?;
                Ok(n)
            }
            t => Err(self.error(format!("expected a keyword, found {}", t.describe()))),
        }
    }

    fn nesting(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_NESTING {
            return Err(self.error(format!(
                "expressions nest more than {MAX_NESTING} levels deep"
            )));
        }
        Ok(())
    }

    // ---- the preamble

    fn preamble(&mut self) -> Result<(), ParseError> {
        while self.at_keyword("Prefix")? {
            self.take()?;
            self.open()?;
            let name = match self.take()? {
                Tok::Name(n) if n.ends_with(':') && n.matches(':').count() == 1 => {
                    n.trim_end_matches(':').to_string()
                }
                t => {
                    return Err(self.error(format!(
                        "expected a prefix name such as 'ex:' or ':', found {}",
                        t.describe()
                    )));
                }
            };
            self.expect(Tok::Equals)?;
            let ns = match self.take()? {
                Tok::Full(s) => self.full_iri(s)?.into_string(),
                t => {
                    return Err(self.error(format!(
                        "expected the prefix's IRI in angle brackets, found {}",
                        t.describe()
                    )));
                }
            };
            self.close()?;
            self.prefixes.push((name.clone(), ns.clone()));
            self.names.push((name, ns));
        }
        match self.take()? {
            Tok::Name("Ontology") => self.open()?,
            t => {
                return Err(self.error(format!(
                    "expected 'Ontology(' after the prefix declarations, found {}",
                    t.describe()
                )));
            }
        }
        if self.at_iri()? {
            self.header.iri = Some(self.iri()?);
            if self.at_iri()? {
                self.header.version_iri = Some(self.iri()?);
            }
        }
        while self.at_keyword("Import")? {
            self.take()?;
            self.open()?;
            let i = self.iri()?;
            self.close()?;
            self.header.imports.insert(i);
        }
        while self.at_keyword("Annotation")? {
            let a = self.annotation()?;
            self.header.annotations.insert(a);
        }
        Ok(())
    }

    // ---- terms

    fn literal(&mut self) -> Result<Literal, ParseError> {
        let value = match self.take()? {
            Tok::Str(s) => s,
            t => {
                return Err(self.error(format!("expected a literal, found {}", t.describe())));
            }
        };
        match self.peek()? {
            Tok::Carets => {
                self.take()?;
                let dt = self.iri()?;
                if dt.as_str() == vocab::RDF_PLAIN_LITERAL {
                    // "text@lang"^^rdf:PlainLiteral is "text"@lang, and "text@" is "text"
                    let Some((text, lang)) = value.rsplit_once('@') else {
                        return Err(self.error(
                            "an rdf:PlainLiteral's lexical form ends with '@' and its language tag",
                        ));
                    };
                    return if lang.is_empty() {
                        Ok(Literal::simple(text))
                    } else {
                        Literal::lang(text, lang).map_err(|e| self.error(e.0))
                    };
                }
                Ok(Literal::typed(value, dt.as_str()))
            }
            Tok::Lang(_) => {
                let Tok::Lang(l) = self.take()? else {
                    unreachable!()
                };
                Literal::lang(value, l).map_err(|e| self.error(e.0))
            }
            _ => Ok(Literal::simple(value)),
        }
    }

    fn individual(&mut self) -> Result<Individual, ParseError> {
        if let Tok::Node(_) = self.peek()? {
            let Tok::Node(n) = self.take()? else {
                unreachable!()
            };
            return Ok(Individual::Anonymous(AnonymousIndividual::new(n)));
        }
        Ok(Individual::Named(NamedIndividual(self.iri()?)))
    }

    fn individuals(&mut self) -> Result<BTreeSet<Individual>, ParseError> {
        let mut out = BTreeSet::new();
        while !self.at_close()? {
            out.insert(self.individual()?);
        }
        Ok(out)
    }

    fn ope(&mut self) -> Result<ObjectPropertyExpression, ParseError> {
        if self.at_keyword("ObjectInverseOf")? {
            self.take()?;
            self.open()?;
            let p = ObjectProperty(self.iri()?);
            self.close()?;
            return Ok(ObjectPropertyExpression::InverseOf(p));
        }
        Ok(ObjectPropertyExpression::Property(ObjectProperty(
            self.iri()?,
        )))
    }

    fn opes(&mut self) -> Result<BTreeSet<ObjectPropertyExpression>, ParseError> {
        let mut out = BTreeSet::new();
        while !self.at_close()? {
            out.insert(self.ope()?);
        }
        Ok(out)
    }

    fn data_property(&mut self) -> Result<DataProperty, ParseError> {
        Ok(DataProperty(self.iri()?))
    }

    fn data_properties(&mut self) -> Result<BTreeSet<DataProperty>, ParseError> {
        let mut out = BTreeSet::new();
        while !self.at_close()? {
            out.insert(self.data_property()?);
        }
        Ok(out)
    }

    fn annotation_property(&mut self) -> Result<AnnotationProperty, ParseError> {
        Ok(AnnotationProperty(self.iri()?))
    }

    fn cardinality(&mut self) -> Result<u32, ParseError> {
        match self.take()? {
            Tok::Int(n) => n
                .parse::<u32>()
                .map_err(|_| self.error(format!("the cardinality {n} is too large"))),
            t => Err(self.error(format!(
                "expected a non-negative integer, found {}",
                t.describe()
            ))),
        }
    }

    // ---- data ranges and class expressions

    fn data_range(&mut self) -> Result<DataRange, ParseError> {
        if self.at_iri()? {
            return Ok(DataRange::Datatype(Datatype(self.iri()?)));
        }
        self.nesting()?;
        let k = self.keyword()?;
        let r = match k {
            "DataIntersectionOf" | "DataUnionOf" => {
                let mut rs = BTreeSet::new();
                while !self.at_close()? {
                    rs.insert(self.data_range()?);
                }
                if k == "DataIntersectionOf" {
                    DataRange::IntersectionOf(rs)
                } else {
                    DataRange::UnionOf(rs)
                }
            }
            "DataComplementOf" => DataRange::ComplementOf(Box::new(self.data_range()?)),
            "DataOneOf" => {
                let mut ls = BTreeSet::new();
                while !self.at_close()? {
                    ls.insert(self.literal()?);
                }
                DataRange::OneOf(ls)
            }
            "DatatypeRestriction" => {
                let d = Datatype(self.iri()?);
                let mut fs = BTreeSet::new();
                while !self.at_close()? {
                    let facet = self.iri()?;
                    let value = self.literal()?;
                    fs.insert(FacetRestriction { facet, value });
                }
                DataRange::DatatypeRestriction(d, fs)
            }
            k => return Err(self.error(format!("'{k}' is not a data range"))),
        };
        self.close()?;
        self.depth -= 1;
        Ok(r)
    }

    fn class_expression(&mut self) -> Result<ClassExpression, ParseError> {
        use ClassExpression as C;
        if self.at_iri()? {
            return Ok(C::Class(Class(self.iri()?)));
        }
        self.nesting()?;
        let k = self.keyword()?;
        let boxed = |c: ClassExpression| Box::new(c);
        let c = match k {
            "ObjectIntersectionOf" | "ObjectUnionOf" => {
                let mut cs = BTreeSet::new();
                while !self.at_close()? {
                    cs.insert(self.class_expression()?);
                }
                if k == "ObjectIntersectionOf" {
                    C::ObjectIntersectionOf(cs)
                } else {
                    C::ObjectUnionOf(cs)
                }
            }
            "ObjectComplementOf" => C::ObjectComplementOf(boxed(self.class_expression()?)),
            "ObjectOneOf" => C::ObjectOneOf(self.individuals()?),
            "ObjectSomeValuesFrom" => {
                let p = self.ope()?;
                C::ObjectSomeValuesFrom(p, boxed(self.class_expression()?))
            }
            "ObjectAllValuesFrom" => {
                let p = self.ope()?;
                C::ObjectAllValuesFrom(p, boxed(self.class_expression()?))
            }
            "ObjectHasValue" => {
                let p = self.ope()?;
                C::ObjectHasValue(p, self.individual()?)
            }
            "ObjectHasSelf" => C::ObjectHasSelf(self.ope()?),
            "ObjectMinCardinality" | "ObjectMaxCardinality" | "ObjectExactCardinality" => {
                let n = self.cardinality()?;
                let p = self.ope()?;
                let f = if self.at_close()? {
                    None
                } else {
                    Some(boxed(self.class_expression()?))
                };
                match k {
                    "ObjectMinCardinality" => C::ObjectMinCardinality(n, p, f),
                    "ObjectMaxCardinality" => C::ObjectMaxCardinality(n, p, f),
                    _ => C::ObjectExactCardinality(n, p, f),
                }
            }
            "DataSomeValuesFrom" | "DataAllValuesFrom" => {
                // DataPropertyExpression { DataPropertyExpression } DataRange: every
                // operand before the last is a data property
                let mut ps = Vec::new();
                let mut last = None;
                while !self.at_close()? {
                    if let Some(prev) = last.take() {
                        match prev {
                            DataRange::Datatype(d) => ps.push(DataProperty(d.0)),
                            _ => {
                                return Err(self.error(format!(
                                    "{k} takes data properties before its data range"
                                )));
                            }
                        }
                    }
                    last = Some(self.data_range()?);
                }
                let Some(r) = last.filter(|_| !ps.is_empty()) else {
                    return Err(self.error(format!("{k} takes a data property and a data range")));
                };
                if k == "DataSomeValuesFrom" {
                    C::DataSomeValuesFrom(ps, r)
                } else {
                    C::DataAllValuesFrom(ps, r)
                }
            }
            "DataHasValue" => {
                let p = self.data_property()?;
                C::DataHasValue(p, self.literal()?)
            }
            "DataMinCardinality" | "DataMaxCardinality" | "DataExactCardinality" => {
                let n = self.cardinality()?;
                let p = self.data_property()?;
                let r = if self.at_close()? {
                    None
                } else {
                    Some(self.data_range()?)
                };
                match k {
                    "DataMinCardinality" => C::DataMinCardinality(n, p, r),
                    "DataMaxCardinality" => C::DataMaxCardinality(n, p, r),
                    _ => C::DataExactCardinality(n, p, r),
                }
            }
            k => return Err(self.error(format!("'{k}' is not a class expression"))),
        };
        self.close()?;
        self.depth -= 1;
        Ok(c)
    }

    fn class_expressions(&mut self) -> Result<BTreeSet<ClassExpression>, ParseError> {
        let mut out = BTreeSet::new();
        while !self.at_close()? {
            out.insert(self.class_expression()?);
        }
        Ok(out)
    }

    // ---- annotations and axioms

    fn annotation(&mut self) -> Result<Annotation, ParseError> {
        self.nesting()?;
        self.take()?; // Annotation
        self.open()?;
        let annotations = self.annotations()?;
        let property = self.annotation_property()?;
        let value = self.annotation_value()?;
        self.close()?;
        self.depth -= 1;
        Ok(Annotation {
            property,
            value,
            annotations,
        })
    }

    fn annotations(&mut self) -> Result<BTreeSet<Annotation>, ParseError> {
        let mut out = BTreeSet::new();
        while self.at_keyword("Annotation")? {
            out.insert(self.annotation()?);
        }
        Ok(out)
    }

    fn annotation_value(&mut self) -> Result<AnnotationValue, ParseError> {
        Ok(match self.peek()? {
            Tok::Node(_) => {
                let Tok::Node(n) = self.take()? else {
                    unreachable!()
                };
                AnnotationValue::Anonymous(AnonymousIndividual::new(n))
            }
            Tok::Str(_) => AnnotationValue::Literal(self.literal()?),
            _ => AnnotationValue::Iri(self.iri()?),
        })
    }

    fn annotation_subject(&mut self) -> Result<AnnotationSubject, ParseError> {
        if let Tok::Node(_) = self.peek()? {
            let Tok::Node(n) = self.take()? else {
                unreachable!()
            };
            return Ok(AnnotationSubject::Anonymous(AnonymousIndividual::new(n)));
        }
        Ok(AnnotationSubject::Iri(self.iri()?))
    }

    fn axiom(&mut self) -> Result<Axiom, ParseError> {
        use AxiomKind as K;
        let start = match &self.peeked {
            Some((_, at)) => *at,
            None => self.pos,
        };
        let k = self.keyword()?;
        let annotations = self.annotations()?;
        let kind = match k {
            "Declaration" => {
                let kw = self.keyword()?;
                let Some(kind) = EntityKind::parse(kw) else {
                    return Err(self.error(format!("'{kw}' is not an entity kind")));
                };
                let e = Entity::of_kind(kind, self.iri()?);
                self.close()?;
                K::Declaration(e)
            }
            "SubClassOf" => {
                let a = self.class_expression()?;
                K::SubClassOf(a, self.class_expression()?)
            }
            "EquivalentClasses" => K::EquivalentClasses(self.class_expressions()?),
            "DisjointClasses" => K::DisjointClasses(self.class_expressions()?),
            "DisjointUnion" => {
                let c = Class(self.iri()?);
                K::DisjointUnion(c, self.class_expressions()?)
            }
            "SubObjectPropertyOf" => {
                let sub = if self.at_keyword("ObjectPropertyChain")? {
                    self.take()?;
                    self.open()?;
                    let mut ps = Vec::new();
                    while !self.at_close()? {
                        ps.push(self.ope()?);
                    }
                    self.close()?;
                    SubObjectPropertyExpression::Chain(ps)
                } else {
                    SubObjectPropertyExpression::Expression(self.ope()?)
                };
                K::SubObjectPropertyOf(sub, self.ope()?)
            }
            "EquivalentObjectProperties" => K::EquivalentObjectProperties(self.opes()?),
            "DisjointObjectProperties" => K::DisjointObjectProperties(self.opes()?),
            "InverseObjectProperties" => {
                let a = self.ope()?;
                K::InverseObjectProperties(a, self.ope()?)
            }
            "ObjectPropertyDomain" => {
                let p = self.ope()?;
                K::ObjectPropertyDomain(p, self.class_expression()?)
            }
            "ObjectPropertyRange" => {
                let p = self.ope()?;
                K::ObjectPropertyRange(p, self.class_expression()?)
            }
            "FunctionalObjectProperty" => K::FunctionalObjectProperty(self.ope()?),
            "InverseFunctionalObjectProperty" => K::InverseFunctionalObjectProperty(self.ope()?),
            "ReflexiveObjectProperty" => K::ReflexiveObjectProperty(self.ope()?),
            "IrreflexiveObjectProperty" => K::IrreflexiveObjectProperty(self.ope()?),
            "SymmetricObjectProperty" => K::SymmetricObjectProperty(self.ope()?),
            "AsymmetricObjectProperty" => K::AsymmetricObjectProperty(self.ope()?),
            "TransitiveObjectProperty" => K::TransitiveObjectProperty(self.ope()?),
            "SubDataPropertyOf" => {
                let a = self.data_property()?;
                K::SubDataPropertyOf(a, self.data_property()?)
            }
            "EquivalentDataProperties" => K::EquivalentDataProperties(self.data_properties()?),
            "DisjointDataProperties" => K::DisjointDataProperties(self.data_properties()?),
            "DataPropertyDomain" => {
                let p = self.data_property()?;
                K::DataPropertyDomain(p, self.class_expression()?)
            }
            "DataPropertyRange" => {
                let p = self.data_property()?;
                K::DataPropertyRange(p, self.data_range()?)
            }
            "FunctionalDataProperty" => K::FunctionalDataProperty(self.data_property()?),
            "DatatypeDefinition" => {
                let d = Datatype(self.iri()?);
                K::DatatypeDefinition(d, self.data_range()?)
            }
            "HasKey" => {
                let c = self.class_expression()?;
                self.open()?;
                let ops = self.opes()?;
                self.close()?;
                self.open()?;
                let dps = self.data_properties()?;
                self.close()?;
                K::HasKey(c, ops, dps)
            }
            "SameIndividual" => K::SameIndividual(self.individuals()?),
            "DifferentIndividuals" => K::DifferentIndividuals(self.individuals()?),
            "ClassAssertion" => {
                let c = self.class_expression()?;
                K::ClassAssertion(c, self.individual()?)
            }
            "ObjectPropertyAssertion" | "NegativeObjectPropertyAssertion" => {
                let p = self.ope()?;
                let a = self.individual()?;
                let b = self.individual()?;
                if k == "ObjectPropertyAssertion" {
                    K::ObjectPropertyAssertion(p, a, b)
                } else {
                    K::NegativeObjectPropertyAssertion(p, a, b)
                }
            }
            "DataPropertyAssertion" | "NegativeDataPropertyAssertion" => {
                let p = self.data_property()?;
                let a = self.individual()?;
                let l = self.literal()?;
                if k == "DataPropertyAssertion" {
                    K::DataPropertyAssertion(p, a, l)
                } else {
                    K::NegativeDataPropertyAssertion(p, a, l)
                }
            }
            "AnnotationAssertion" => {
                let p = self.annotation_property()?;
                let s = self.annotation_subject()?;
                K::AnnotationAssertion(p, s, self.annotation_value()?)
            }
            "SubAnnotationPropertyOf" => {
                let a = self.annotation_property()?;
                K::SubAnnotationPropertyOf(a, self.annotation_property()?)
            }
            "AnnotationPropertyDomain" => {
                let p = self.annotation_property()?;
                K::AnnotationPropertyDomain(p, self.iri()?)
            }
            "AnnotationPropertyRange" => {
                let p = self.annotation_property()?;
                K::AnnotationPropertyRange(p, self.iri()?)
            }
            k => return Err(self.error(format!("'{k}' is not an axiom"))),
        };
        self.close()?;
        let ax = Axiom { kind, annotations };
        crate::validate::axiom(&ax).map_err(|e| self.error_at(start, e.0))?;
        Ok(ax)
    }

    /// The next axiom, or `None` at the ontology's closing parenthesis.
    fn next_axiom(&mut self) -> Result<Option<Axiom>, ParseError> {
        if self.at_close()? {
            self.take()?;
            match self.take()? {
                Tok::End => return Ok(None),
                t => {
                    return Err(self.error(format!(
                        "expected the end of the document after the ontology, found {}",
                        t.describe()
                    )));
                }
            }
        }
        if let Tok::End = self.peek()? {
            self.take()?;
            return Err(self.error("the ontology is not closed with ')'"));
        }
        self.depth = 0;
        self.axiom().map(Some)
    }
}

impl Iterator for Reader<'_> {
    type Item = Result<Axiom, ParseError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.next_axiom() {
            Ok(Some(ax)) => Some(Ok(ax)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

/// The RDF triples of a document, by the forward mapping, with fresh blank nodes from
/// `fresh`. An anonymous individual is the blank node its label names.
pub fn triples(doc: &Document, fresh: &mut dyn FnMut() -> oxrdf::BlankNode) -> Vec<oxrdf::Triple> {
    crate::write::ontology_triples(&doc.header, doc.axioms.iter(), fresh)
}
