#![allow(clippy::ignored_unit_patterns)]

use crate::algebra::*;
use crate::query::*;
use crate::term::*;
use crate::update::*;
use oxilangtag::LanguageTag;
use oxiri::{Iri, IriParseError};
use oxrdf::vocab::{rdf, xsd};
use peg::parser;
use peg::str::LineCol;
use rand::random;
use std::borrow::Cow;
#[cfg(test)]
use std::cell::Cell;
use std::char;
use std::collections::hash_map::RandomState;
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::mem::take;
#[cfg(feature = "standard-unicode-escaping")]
use std::str::Chars;
use std::str::FromStr;
use std::sync::Arc;

/// The IRI of the ARQ aggregate `local` (`median`, `stdev`, …).
fn arq_aggregate(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{ARQ_AGGREGATE_NAMESPACE}{local}"))
}

/// A SPARQL parser
///
/// ```
/// use spargebra::SparqlParser;
///
/// let query_str = "SELECT ?s ?p ?o WHERE { ?s ?p ?o . }";
/// let query = SparqlParser::new().parse_query(query_str)?;
/// assert_eq!(query.to_string(), query_str);
/// # Ok::<_, spargebra::SparqlSyntaxError>(())
/// ```
#[must_use]
#[derive(Clone, Default)]
pub struct SparqlParser {
    base_iri: Option<Iri<String>>,
    prefixes: HashMap<String, String>,
    /// shared, so that a parser with many custom aggregates is cheap to clone
    custom_aggregate_functions: Arc<HashSet<NamedNode>>,
    /// Prefix names registered as synthetic placeholders (see [`SparqlParser::with_placeholder_prefix`]).
    placeholder_prefixes: HashSet<String>,
    /// Optional upper bound on cumulative resolved IRI bytes materialized during parsing.
    max_resolved_iri_bytes: Option<usize>,
    /// Reject Jena ARQ's syntax extensions (see [`SparqlParser::with_arq_syntax`]).
    sparql_only: bool,
}

impl SparqlParser {
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Provides an IRI that could be used to resolve the operation relative IRIs.
    ///
    /// ```
    /// use spargebra::SparqlParser;
    ///
    /// let query = SparqlParser::new().with_base_iri("http://example.com/")?.parse_query("SELECT * WHERE { <s> <p> <o> }")?;
    /// assert_eq!(query.to_string(), "BASE <http://example.com/>\nSELECT * WHERE { <http://example.com/s> <http://example.com/p> <http://example.com/o> . }");
    /// # Result::<_, Box<dyn std::error::Error>>::Ok(())
    /// ```
    #[inline]
    pub fn with_base_iri(mut self, base_iri: impl Into<String>) -> Result<Self, IriParseError> {
        self.base_iri = Some(Iri::parse(base_iri.into())?);
        Ok(self)
    }

    /// Set a default IRI prefix used during parsing.
    ///
    /// ```
    /// use spargebra::SparqlParser;
    ///
    /// let query = SparqlParser::new()
    ///     .with_prefix("ex", "http://example.com/")?
    ///     .parse_query("SELECT * WHERE { ex:s ex:p ex:o }")?;
    /// assert_eq!(
    ///     query.to_string(),
    ///     "SELECT * WHERE { <http://example.com/s> <http://example.com/p> <http://example.com/o> . }"
    /// );
    /// # Result::<_, Box<dyn std::error::Error>>::Ok(())
    /// ```
    #[inline]
    pub fn with_prefix(
        mut self,
        prefix_name: impl Into<String>,
        prefix_iri: impl Into<String>,
    ) -> Result<Self, IriParseError> {
        self.prefixes.insert(
            prefix_name.into(),
            Iri::parse(prefix_iri.into())?.into_inner(),
        );
        Ok(self)
    }

    /// Adds a new function to be parsed as a custom aggregate function and not as a regular custom function.
    ///
    /// ```
    /// use oxrdf::NamedNode;
    /// use spargebra::SparqlParser;
    ///
    /// SparqlParser::new()
    ///     .with_custom_aggregate_function(NamedNode::new("http://example.com/concat")?)
    ///     .parse_query(
    ///         "PREFIX ex: <http://example.com/> SELECT (ex:concat(?o) AS ?concat) WHERE { ex:s ex:p ex:o }",
    ///     )?;
    /// # Result::<_, Box<dyn std::error::Error>>::Ok(())
    /// ```
    #[inline]
    pub fn with_custom_aggregate_function(mut self, name: impl Into<NamedNode>) -> Self {
        Arc::make_mut(&mut self.custom_aggregate_functions).insert(name.into());
        self
    }

    /// Adds a set of custom aggregate functions at once. A parser that has none yet shares
    /// the set rather than copying it, so a set built once costs nothing per parser.
    #[inline]
    pub fn with_custom_aggregate_functions(mut self, names: &Arc<HashSet<NamedNode>>) -> Self {
        if self.custom_aggregate_functions.is_empty() {
            self.custom_aggregate_functions = Arc::clone(names);
        } else {
            Arc::make_mut(&mut self.custom_aggregate_functions).extend(names.iter().cloned());
        }
        self
    }

    /// Registers a synthetic placeholder prefix used for standalone fragment validation
    /// when real prefix declarations are not yet known. Single-argument calls written with
    /// this prefixed name (and never explicit `<IRI>` references or prefixes redeclared in
    /// the query prologue) are admitted as potential custom aggregates in grouped `SELECT`
    /// projections, with or without `DISTINCT`.
    #[inline]
    pub fn with_placeholder_prefix(
        mut self,
        prefix_name: impl Into<String>,
        placeholder_iri: impl Into<String>,
    ) -> Result<Self, IriParseError> {
        let name = prefix_name.into();
        let iri = Iri::parse(placeholder_iri.into())?.into_inner();
        self.placeholder_prefixes.insert(name.clone());
        self.prefixes.insert(name, iri);
        Ok(self)
    }

    /// Sets an upper bound on cumulative resolved IRI bytes materialized during parsing
    /// (`BASE` / `PREFIX` declarations, `IRIREF`s, and `PrefixedName` expansions). When
    /// exceeded, parsing fails before allocating the over-budget IRI (including escaped
    /// `IRIREF`s, whose decoded length is measured without allocation first; an admitted
    /// escaped relative `IRIREF` allocates only its already-admitted decoded scratch buffer
    /// before resolution) with a structured error reportable via
    /// [`SparqlSyntaxError::is_resolved_iri_limit_exceeded`].
    #[inline]
    pub fn with_max_resolved_iri_bytes(mut self, max_bytes: usize) -> Self {
        self.max_resolved_iri_bytes = Some(max_bytes);
        self
    }

    /// Whether Jena ARQ's extensions of the SPARQL syntax are accepted (the default) or
    /// are syntax errors: `LATERAL`, `LET`, `UNFOLD`, `SEMIJOIN`, `ANTIJOIN`, path ranges (`p{2}`,
    /// `p{1,3}`, `p{2,}`, `p{,3}`, `p{*}`, `p{+}`) and `distinct(…)`, `multi(…)` and
    /// `shortest(…)` paths, `GRAPH` blocks in CONSTRUCT
    /// templates and ARQ's aggregates (`MEDIAN`, `MODE`, `STDEV`, …, `FOLD`,
    /// `AGG <iri>(…)`).
    ///
    /// ```
    /// use spargebra::SparqlParser;
    ///
    /// let q = "SELECT * WHERE { ?s <http://example.com/p>{2} ?o }";
    /// assert!(SparqlParser::new().parse_query(q).is_ok());
    /// assert!(SparqlParser::new().with_arq_syntax(false).parse_query(q).is_err());
    /// ```
    #[inline]
    pub fn with_arq_syntax(mut self, enabled: bool) -> Self {
        self.sparql_only = !enabled;
        self
    }

    /// Parse the given query string using the already set options.
    ///
    /// ```
    /// use spargebra::SparqlParser;
    ///
    /// let query_str = "SELECT ?s ?p ?o WHERE { ?s ?p ?o . }";
    /// let query = SparqlParser::new().parse_query(query_str)?;
    /// assert_eq!(query.to_string(), query_str);
    /// # Ok::<_, spargebra::SparqlSyntaxError>(())
    /// ```
    #[cfg_attr(
        not(feature = "standard-unicode-escaping"),
        expect(clippy::needless_borrow)
    )]
    pub fn parse_query(self, query: &str) -> Result<Query, SparqlSyntaxError> {
        let mut state = ParserState::new(
            self.base_iri,
            self.prefixes,
            self.custom_aggregate_functions,
        );
        state.placeholder_prefixes = self.placeholder_prefixes;
        state.max_resolved_iri_bytes = self.max_resolved_iri_bytes;
        state.arq = !self.sparql_only;
        #[cfg(feature = "standard-unicode-escaping")]
        let query = unescape_unicode_codepoints(query);
        too_deep(&query)?;
        let parsed = parser::QueryUnit(&query, &mut state);
        if let Some(limit) = state.resolved_iri_limit_exceeded {
            return Err(SparqlSyntaxErrorKind::ResolvedIriLimitExceeded { limit }.into());
        }
        Ok(parsed.map_err(SparqlSyntaxErrorKind::Syntax)?)
    }

    /// Parse the given update string using the already set options.
    ///
    /// ```
    /// use spargebra::SparqlParser;
    ///
    /// let update_str = "CLEAR ALL ;";
    /// let update = SparqlParser::new().parse_update(update_str)?;
    /// assert_eq!(update.to_string().trim(), update_str);
    /// # Ok::<_, spargebra::SparqlSyntaxError>(())
    /// ```
    #[cfg_attr(
        not(feature = "standard-unicode-escaping"),
        expect(clippy::needless_borrow)
    )]
    pub fn parse_update(self, update: &str) -> Result<Update, SparqlSyntaxError> {
        let mut state = ParserState::new(
            self.base_iri,
            self.prefixes,
            self.custom_aggregate_functions,
        );
        state.max_resolved_iri_bytes = self.max_resolved_iri_bytes;
        state.arq = !self.sparql_only;
        #[cfg(feature = "standard-unicode-escaping")]
        let update = unescape_unicode_codepoints(update);
        too_deep(&update)?;
        let parsed = parser::UpdateInit(&update, &mut state);
        if let Some(limit) = state.resolved_iri_limit_exceeded {
            return Err(SparqlSyntaxErrorKind::ResolvedIriLimitExceeded { limit }.into());
        }
        let operations = parsed.map_err(SparqlSyntaxErrorKind::Syntax)?;
        check_if_insert_data_are_sharing_blank_nodes(&operations)?;
        Ok(Update {
            operations,
            base_iri: state.base_iri,
        })
    }
}

/// Error returned during SPARQL parsing.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct SparqlSyntaxError {
    #[from]
    kind: SparqlSyntaxErrorKind,
}

impl SparqlSyntaxError {
    pub(crate) fn from_bad_base_iri(e: IriParseError) -> Self {
        SparqlSyntaxErrorKind::InvalidBaseIri(e).into()
    }

    /// Whether this error refused the query or update for exceeding the nesting or depth limit.
    pub fn is_too_deep(&self) -> bool {
        matches!(self.kind, SparqlSyntaxErrorKind::TooDeep { .. })
    }

    /// Whether this error refused the query or update for exceeding the resolved-IRI byte budget.
    pub fn is_resolved_iri_limit_exceeded(&self) -> bool {
        matches!(
            self.kind,
            SparqlSyntaxErrorKind::ResolvedIriLimitExceeded { .. }
        )
    }
}

#[derive(Debug, thiserror::Error)]
enum SparqlSyntaxErrorKind {
    #[error("Invalid SPARQL base IRI provided: {0}")]
    InvalidBaseIri(#[from] IriParseError),
    #[error(transparent)]
    Syntax(#[from] peg::error::ParseError<LineCol>),
    #[error("The blank node {0} cannot be shared by multiple blocks")]
    SharedBlankNode(BlankNode),
    /// Refused before parsing (see [`crate::nesting`]); written as peg writes its errors.
    #[error("error at {line}:{column}: {message}")]
    TooDeep {
        line: usize,
        column: usize,
        message: String,
    },
    /// Refused during parsing because cumulative resolved IRI bytes exceeded the configured limit.
    #[error("query exceeds maximum resolved IRI size ({limit} bytes)")]
    ResolvedIriLimitExceeded { limit: usize },
}

/// Refuse a text that nests too deeply for the parser and the algebra it builds.
fn too_deep(text: &str) -> Result<(), SparqlSyntaxError> {
    crate::nesting::check(text).map_err(|e| {
        let before = &text[..e.offset];
        SparqlSyntaxErrorKind::TooDeep {
            line: before.matches('\n').count() + 1,
            column: before.chars().rev().take_while(|&c| c != '\n').count() + 1,
            message: e.message,
        }
        .into()
    })
}

#[cfg(feature = "standard-unicode-escaping")]
fn unescape_unicode_codepoints(input: &str) -> Cow<'_, str> {
    if needs_unescape_unicode_codepoints(input) {
        UnescapeUnicodeCharIterator::new(input).collect()
    } else {
        input.into()
    }
}

#[cfg(feature = "standard-unicode-escaping")]
fn needs_unescape_unicode_codepoints(input: &str) -> bool {
    let bytes = input.as_bytes();
    for i in 1..bytes.len() {
        if (bytes[i] == b'u' || bytes[i] == b'U') && bytes[i - 1] == b'\\' {
            return true;
        }
    }
    false
}

#[cfg(feature = "standard-unicode-escaping")]
struct UnescapeUnicodeCharIterator<'a> {
    iter: Chars<'a>,
    buffer: String,
}

#[cfg(feature = "standard-unicode-escaping")]
impl<'a> UnescapeUnicodeCharIterator<'a> {
    fn new(string: &'a str) -> Self {
        Self {
            iter: string.chars(),
            buffer: String::with_capacity(9),
        }
    }
}

#[cfg(feature = "standard-unicode-escaping")]
impl Iterator for UnescapeUnicodeCharIterator<'_> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        let c = if self.buffer.is_empty() {
            self.iter.next()?
        } else {
            self.buffer.remove(0)
        };
        match c {
            '\\' => match self.iter.next() {
                Some('u') => {
                    self.buffer.push('u');
                    for _ in 0..4 {
                        if let Some(c) = self.iter.next() {
                            self.buffer.push(c);
                        } else {
                            return Some('\\');
                        }
                    }
                    if let Some(c) = u32::from_str_radix(&self.buffer[1..], 16)
                        .ok()
                        .and_then(char::from_u32)
                    {
                        self.buffer.clear();
                        Some(c)
                    } else {
                        Some('\\')
                    }
                }
                Some('U') => {
                    self.buffer.push('U');
                    for _ in 0..8 {
                        if let Some(c) = self.iter.next() {
                            self.buffer.push(c);
                        } else {
                            return Some('\\');
                        }
                    }
                    if let Some(c) = u32::from_str_radix(&self.buffer[1..], 16)
                        .ok()
                        .and_then(char::from_u32)
                    {
                        self.buffer.clear();
                        Some(c)
                    } else {
                        Some('\\')
                    }
                }
                Some(c) => {
                    self.buffer.push(c);
                    Some('\\')
                }
                None => Some('\\'),
            },
            _ => Some(c),
        }
    }
}

struct ReifiedTerm {
    term: TermPattern,
    reifiers: Vec<TermPattern>,
}

#[derive(Default)]
struct FocusedTriplePattern<F> {
    focus: F,
    patterns: Vec<TriplePattern>,
}

impl<F> FocusedTriplePattern<F> {
    fn new(focus: impl Into<F>) -> Self {
        Self {
            focus: focus.into(),
            patterns: Vec::new(),
        }
    }
}

impl<F> From<FocusedTriplePattern<F>> for FocusedTriplePattern<Vec<F>> {
    fn from(input: FocusedTriplePattern<F>) -> Self {
        Self {
            focus: vec![input.focus],
            patterns: input.patterns,
        }
    }
}

#[derive(Clone, Debug)]
enum VariableOrPropertyPath {
    Variable(Variable),
    PropertyPath(PropertyPathExpression),
}

impl From<Variable> for VariableOrPropertyPath {
    fn from(var: Variable) -> Self {
        Self::Variable(var)
    }
}

impl From<NamedNodePattern> for VariableOrPropertyPath {
    fn from(pattern: NamedNodePattern) -> Self {
        match pattern {
            NamedNodePattern::NamedNode(node) => PropertyPathExpression::from(node).into(),
            NamedNodePattern::Variable(v) => v.into(),
        }
    }
}

impl From<PropertyPathExpression> for VariableOrPropertyPath {
    fn from(path: PropertyPathExpression) -> Self {
        Self::PropertyPath(path)
    }
}

#[cfg_attr(feature = "sparql-12", expect(clippy::unnecessary_wraps))]
fn add_to_triple_patterns(
    subject: TermPattern,
    predicate: NamedNodePattern,
    object: ReifiedTerm,
    patterns: &mut Vec<TriplePattern>,
) -> Result<(), &'static str> {
    let triple = TriplePattern::new(subject, predicate, object.term);
    #[cfg(feature = "sparql-12")]
    for reifier in object.reifiers {
        patterns.push(TriplePattern {
            subject: reifier.clone(),
            predicate: rdf::REIFIES.into_owned().into(),
            object: triple.clone().into(),
        });
    }
    #[cfg(not(feature = "sparql-12"))]
    if !object.reifiers.is_empty() {
        return Err("Triple terms are only available in SPARQL 1.2");
    }
    patterns.push(triple);
    Ok(())
}

fn add_to_triple_or_path_patterns(
    subject: TermPattern,
    predicate: impl Into<VariableOrPropertyPath>,
    object: ReifiedTerm,
    patterns: &mut Vec<TripleOrPathPattern>,
) -> Result<(), &'static str> {
    match predicate.into() {
        VariableOrPropertyPath::Variable(p) => {
            add_triple_to_triple_or_path_patterns(subject, p, object, patterns)?;
        }
        VariableOrPropertyPath::PropertyPath(p) => match p {
            PropertyPathExpression::NamedNode(p) => {
                add_triple_to_triple_or_path_patterns(subject, p, object, patterns)?;
            }
            PropertyPathExpression::Reverse(p) => add_to_triple_or_path_patterns(
                object.term,
                *p,
                ReifiedTerm {
                    term: subject,
                    reifiers: object.reifiers,
                },
                patterns,
            )?,
            PropertyPathExpression::Sequence(a, b) => {
                if !object.reifiers.is_empty() {
                    return Err("Reifiers are not allowed on property paths");
                }
                let middle = BlankNode::default();
                add_to_triple_or_path_patterns(
                    subject,
                    *a,
                    ReifiedTerm {
                        term: middle.clone().into(),
                        reifiers: Vec::new(),
                    },
                    patterns,
                )?;
                add_to_triple_or_path_patterns(
                    middle.into(),
                    *b,
                    ReifiedTerm {
                        term: object.term,
                        reifiers: Vec::new(),
                    },
                    patterns,
                )?;
            }
            path => {
                if !object.reifiers.is_empty() {
                    return Err("Reifiers are not allowed on property paths");
                }
                patterns.push(TripleOrPathPattern::Path {
                    subject,
                    path,
                    object: object.term,
                })
            }
        },
    }
    Ok(())
}

#[cfg_attr(feature = "sparql-12", expect(clippy::unnecessary_wraps))]
fn add_triple_to_triple_or_path_patterns(
    subject: TermPattern,
    predicate: impl Into<NamedNodePattern>,
    object: ReifiedTerm,
    patterns: &mut Vec<TripleOrPathPattern>,
) -> Result<(), &'static str> {
    let triple = TriplePattern::new(subject, predicate, object.term);
    #[cfg(feature = "sparql-12")]
    for reifier in object.reifiers {
        patterns.push(
            TriplePattern {
                subject: reifier.clone(),
                predicate: rdf::REIFIES.into_owned().into(),
                object: triple.clone().into(),
            }
            .into(),
        );
    }
    #[cfg(not(feature = "sparql-12"))]
    if !object.reifiers.is_empty() {
        return Err("Triple terms are only available in SPARQL 1.2");
    }
    patterns.push(triple.into());
    Ok(())
}

fn build_bgp(patterns: Vec<TripleOrPathPattern>) -> GraphPattern {
    let mut bgp = Vec::new();
    let mut elements = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        match pattern {
            TripleOrPathPattern::Triple(t) => bgp.push(t),
            TripleOrPathPattern::Path {
                subject,
                path,
                object,
            } => {
                if !bgp.is_empty() {
                    elements.push(GraphPattern::Bgp {
                        patterns: take(&mut bgp),
                    });
                }
                elements.push(GraphPattern::Path {
                    subject,
                    path,
                    object,
                })
            }
        }
    }
    if !bgp.is_empty() {
        elements.push(GraphPattern::Bgp { patterns: bgp });
    }
    elements.into_iter().reduce(new_join).unwrap_or_default()
}

#[derive(Debug)]
enum TripleOrPathPattern {
    Triple(TriplePattern),
    Path {
        subject: TermPattern,
        path: PropertyPathExpression,
        object: TermPattern,
    },
}

impl From<TriplePattern> for TripleOrPathPattern {
    fn from(tp: TriplePattern) -> Self {
        Self::Triple(tp)
    }
}

#[derive(Debug, Default)]
struct FocusedTripleOrPathPattern<F> {
    focus: F,
    patterns: Vec<TripleOrPathPattern>,
}

impl<F> FocusedTripleOrPathPattern<F> {
    fn new(focus: impl Into<F>) -> Self {
        Self {
            focus: focus.into(),
            patterns: Vec::new(),
        }
    }
}

impl<F> From<FocusedTripleOrPathPattern<F>> for FocusedTripleOrPathPattern<Vec<F>> {
    fn from(input: FocusedTripleOrPathPattern<F>) -> Self {
        Self {
            focus: vec![input.focus],
            patterns: input.patterns,
        }
    }
}

impl<F, T: From<F>> From<FocusedTriplePattern<F>> for FocusedTripleOrPathPattern<T> {
    fn from(input: FocusedTriplePattern<F>) -> Self {
        Self {
            focus: input.focus.into(),
            patterns: input.patterns.into_iter().map(Into::into).collect(),
        }
    }
}

/// The modifier after a path element.
enum PathMod {
    ZeroOrOne,
    ZeroOrMore,
    OneOrMore,
    /// ARQ's `{n,m}` forms (see [`PropertyPathExpression::Range`])
    Range(u64, Option<u64>),
}

#[derive(Eq, PartialEq, Debug, Clone, Hash)]
enum PartialGraphPattern {
    Optional(GraphPattern, Option<Expression>),
    #[cfg(feature = "sep-0006")]
    Lateral(GraphPattern),
    Minus(GraphPattern),
    /// ARQ's `SEMIJOIN { … }` and `ANTIJOIN { … }`
    SemiJoin(GraphPattern),
    AntiJoin(GraphPattern),
    Bind(Expression, Variable),
    /// ARQ's `LET (?v := expr)`
    Assign(Expression, Variable),
    /// ARQ's `UNFOLD(expr AS ?v1, ?v2)`
    Unfold(Expression, Variable, Option<Variable>),
    Filter(Expression),
    Other(GraphPattern),
}

fn new_join(l: GraphPattern, r: GraphPattern) -> GraphPattern {
    // Avoid to output empty BGPs
    if let GraphPattern::Bgp { patterns: pl } = &l {
        if pl.is_empty() {
            return r;
        }
    }
    if let GraphPattern::Bgp { patterns: pr } = &r {
        if pr.is_empty() {
            return l;
        }
    }

    match (l, r) {
        (GraphPattern::Bgp { patterns: mut pl }, GraphPattern::Bgp { patterns: pr }) => {
            pl.extend(pr);
            GraphPattern::Bgp { patterns: pl }
        }
        (GraphPattern::Bgp { patterns }, other) | (other, GraphPattern::Bgp { patterns })
            if patterns.is_empty() =>
        {
            other
        }
        (l, r) => GraphPattern::Join {
            left: Box::new(l),
            right: Box::new(r),
        },
    }
}

fn not_empty_fold<T>(
    iter: impl Iterator<Item = T>,
    combine: impl Fn(T, T) -> T,
) -> Result<T, &'static str> {
    iter.fold(None, |a, b| match a {
        Some(av) => Some(combine(av, b)),
        None => Some(b),
    })
    .ok_or("The iterator should not be empty")
}

#[derive(Default)]
enum SelectionOption {
    Distinct,
    Reduced,
    #[default]
    Default,
}

enum SelectionMember {
    Variable(Variable),
    Expression(Expression, Variable),
}

#[derive(Default)]
enum SelectionVariables {
    Explicit(Vec<SelectionMember>),
    #[default]
    Star,
}

#[derive(Default)]
struct Selection {
    pub start: usize,
    pub option: SelectionOption,
    pub variables: SelectionVariables,
}

/// Add `v` to the variables `pv` unless it is one of them already, and say whether it was
/// added. A query can select thousands of variables, so past a few dozen a set of them,
/// `seen`, answers whether one is there.
fn add_var(pv: &mut Vec<Variable>, seen: &mut Option<HashSet<Variable>>, v: &Variable) -> bool {
    let new = match seen {
        Some(seen) => seen.insert(v.clone()),
        None if pv.len() < 32 => !pv.contains(v),
        None => {
            let mut set: HashSet<Variable> = pv.iter().cloned().collect();
            let new = set.insert(v.clone());
            *seen = Some(set);
            new
        }
    };
    if new {
        pv.push(v.clone());
    }
    new
}

fn build_select(
    select: Selection,
    r#where: GraphPattern,
    mut group: Option<(Vec<Variable>, Vec<(Expression, Variable)>)>,
    having: Option<Expression>,
    order_by: Option<Vec<OrderExpression>>,
    offset_limit: Option<(usize, Option<usize>)>,
    values: Option<GraphPattern>,
    state: &mut ParserState,
) -> Result<GraphPattern, &'static str> {
    // GROUP BY
    let aggregates = state.aggregates.pop().unwrap_or_default();
    let placeholder_calls = state.placeholder_calls.pop().unwrap_or_default();
    let _ = state.aggregate_arg_depth.pop();
    let select_start = select.start;
    let record_forest = !placeholder_calls.is_empty()
        && !state.synthetic_calls.has_recorded_select(select_start);
    let checkpoint = record_forest.then(|| state.synthetic_calls.checkpoint());

    let result: Result<GraphPattern, &'static str> = (|| {
        let mut p = r#where;
        let mut with_aggregate = false;
        let agg_vars: HashSet<Variable> = if record_forest && !aggregates.is_empty() {
            aggregates.iter().map(|(v, _)| v.clone()).collect()
        } else {
            HashSet::new()
        };
        if group.is_none() && !aggregates.is_empty() {
            group = Some((vec![], vec![]));
        }

        if let Some((clauses, binds)) = group {
            for (expression, variable) in binds {
                p = GraphPattern::Extend {
                    inner: Box::new(p),
                    variable,
                    expression,
                };
            }
            p = GraphPattern::Group {
                inner: Box::new(p),
                variables: clauses,
                aggregates,
            };
            with_aggregate = true;
        }

        // HAVING
        if let Some(expr) = having {
            if record_forest {
                state.synthetic_calls.record_placeholder_forest(
                    &expr,
                    &placeholder_calls,
                    &agg_vars,
                    None,
                )?;
            }
            let expr = if placeholder_calls.is_empty() {
                expr
            } else {
                restore_placeholder_calls(expr, &placeholder_calls)
            };
            p = GraphPattern::Filter {
                expr,
                inner: Box::new(p),
            };
        }

        // VALUES
        if let Some(data) = values {
            p = new_join(p, data);
        }

        // SELECT
        let mut pv = Vec::new();
        let mut in_pv = None;
        let with_project = match select.variables {
            SelectionVariables::Explicit(sel_items) => {
                let mut visible = HashSet::new();
                p.on_in_scope_variable(|v| {
                    visible.insert(v.clone());
                });
                for sel_item in sel_items {
                    let v = match sel_item {
                        SelectionMember::Variable(v) => {
                            if with_aggregate && !visible.contains(&v) {
                                // We validate projection variables if there is an aggregate
                                return Err("The SELECT contains a variable that is unbound");
                            }
                            v
                        }
                        SelectionMember::Expression(expression, variable) => {
                            if visible.contains(&variable) {
                                // We disallow to override an existing variable with an expression
                                return Err(
                                    "The SELECT overrides an existing variable using an expression",
                                );
                            }
                            if with_aggregate
                                && !are_variables_bound(&expression, &visible, &placeholder_calls)
                            {
                                // We validate projection variables if there is an aggregate
                                return Err(
                                    "The SELECT contains an expression with a variable that is unbound",
                                );
                            }
                            if record_forest {
                                state.synthetic_calls.record_placeholder_forest(
                                    &expression,
                                    &placeholder_calls,
                                    &agg_vars,
                                    None,
                                )?;
                            }
                            let expression = if placeholder_calls.is_empty() {
                                expression
                            } else {
                                restore_placeholder_calls(expression, &placeholder_calls)
                            };
                            p = GraphPattern::Extend {
                                inner: Box::new(p),
                                variable: variable.clone(),
                                expression,
                            };
                            // SPARQL 1.2 §16.1.2: "The scoping for (expr AS v) applies
                            // immediately": later SELECT expressions may use v (and it may
                            // not be assigned again), as the Extend chain of §18.3.4.4 implies.
                            visible.insert(variable.clone());
                            variable
                        }
                    };
                    if !add_var(&mut pv, &mut in_pv, &v) {
                        return Err("Duplicated variable name in SELECT");
                    }
                }
                true
            }
            SelectionVariables::Star => {
                if with_aggregate {
                    return Err("SELECT * is not authorized with GROUP BY");
                }
                // TODO: is it really useful to do a projection?
                p.on_in_scope_variable(|v| {
                    add_var(&mut pv, &mut in_pv, v);
                });
                pv.sort();
                true
            }
        };

        let mut m = p;

        // ORDER BY
        if let Some(order_exprs) = order_by {
            let mut expression = Vec::with_capacity(order_exprs.len());
            for order_expr in order_exprs {
                let e = match &order_expr {
                    OrderExpression::Asc(e) | OrderExpression::Desc(e) => e,
                };
                if record_forest {
                    state.synthetic_calls.record_placeholder_forest(
                        e,
                        &placeholder_calls,
                        &agg_vars,
                        None,
                    )?;
                }
                expression.push(if placeholder_calls.is_empty() {
                    order_expr
                } else {
                    match order_expr {
                        OrderExpression::Asc(e) => {
                            OrderExpression::Asc(restore_placeholder_calls(e, &placeholder_calls))
                        }
                        OrderExpression::Desc(e) => {
                            OrderExpression::Desc(restore_placeholder_calls(e, &placeholder_calls))
                        }
                    }
                });
            }
            m = GraphPattern::OrderBy {
                inner: Box::new(m),
                expression,
            };
        }

        // PROJECT
        if with_project {
            m = GraphPattern::Project {
                inner: Box::new(m),
                variables: pv,
            };
        }
        match select.option {
            SelectionOption::Distinct => m = GraphPattern::Distinct { inner: Box::new(m) },
            SelectionOption::Reduced => m = GraphPattern::Reduced { inner: Box::new(m) },
            SelectionOption::Default => (),
        }

        // OFFSET LIMIT
        if let Some((start, length)) = offset_limit {
            m = GraphPattern::Slice {
                inner: Box::new(m),
                start,
                length,
            }
        }
        Ok(m)
    })();

    match result {
        Ok(pattern) => {
            if let Some(checkpoint) = checkpoint {
                state
                    .synthetic_calls
                    .commit_select(select_start, checkpoint);
            }
            Ok(pattern)
        }
        Err(err) => {
            if let Some(checkpoint) = checkpoint {
                state.synthetic_calls.rollback(checkpoint);
            }
            Err(err)
        }
    }
}

fn are_variables_bound(
    expression: &Expression,
    variables: &HashSet<Variable>,
    placeholder_calls: &HashMap<Variable, (NamedNode, Vec<Expression>, bool)>,
) -> bool {
    match expression {
        Expression::NamedNode(_)
        | Expression::Literal(_)
        | Expression::Bound(_)
        | Expression::Coalesce(_)
        | Expression::Exists(_) => true,
        Expression::Variable(var) => {
            variables.contains(var)
                || placeholder_calls
                    .get(var)
                    .is_some_and(|(_, args, allow_unbound)| {
                        *allow_unbound
                            || args
                                .iter()
                                .all(|arg| are_variables_bound(arg, variables, placeholder_calls))
                    })
        }
        Expression::UnaryPlus(e) | Expression::UnaryMinus(e) | Expression::Not(e) => {
            are_variables_bound(e, variables, placeholder_calls)
        }
        Expression::Or(a, b)
        | Expression::And(a, b)
        | Expression::Equal(a, b)
        | Expression::SameTerm(a, b)
        | Expression::Greater(a, b)
        | Expression::GreaterOrEqual(a, b)
        | Expression::Less(a, b)
        | Expression::LessOrEqual(a, b)
        | Expression::Add(a, b)
        | Expression::Subtract(a, b)
        | Expression::Multiply(a, b)
        | Expression::Divide(a, b) => {
            are_variables_bound(a, variables, placeholder_calls)
                && are_variables_bound(b, variables, placeholder_calls)
        }
        Expression::In(a, b) => {
            are_variables_bound(a, variables, placeholder_calls)
                && b.iter()
                    .all(|b| are_variables_bound(b, variables, placeholder_calls))
        }
        Expression::FunctionCall(_, parameters) => parameters
            .iter()
            .all(|p| are_variables_bound(p, variables, placeholder_calls)),
        Expression::If(a, b, c) => {
            are_variables_bound(a, variables, placeholder_calls)
                && are_variables_bound(b, variables, placeholder_calls)
                && are_variables_bound(c, variables, placeholder_calls)
        }
    }
}

const NO_SYNTHETIC_CALL_NODE: u32 = u32::MAX;

struct InternedIri {
    iri: NamedNode,
    hash: u64,
    next_in_bucket: u32,
}

/// Interns distinct synthetic call IRIs once and caches each key's 64-bit hash so table
/// growth never rehashes previously interned IRI strings.
struct IriInterner {
    hasher: RandomState,
    entries: Vec<InternedIri>,
    buckets: Vec<u32>,
    #[cfg(test)]
    hashed_bytes: Cell<usize>,
}

impl IriInterner {
    fn new() -> Self {
        Self {
            hasher: RandomState::new(),
            entries: Vec::new(),
            buckets: Vec::new(),
            #[cfg(test)]
            hashed_bytes: Cell::new(0),
        }
    }

    fn hash_iri(&self, iri: &NamedNode) -> u64 {
        let s = iri.as_str();
        #[cfg(test)]
        self.hashed_bytes.set(self.hashed_bytes.get() + s.len());
        self.hasher.hash_one(s)
    }

    fn bucket_index(hash: u64, bucket_len: usize) -> usize {
        debug_assert!(bucket_len.is_power_of_two());
        let mask = u64::try_from(bucket_len - 1).unwrap_or(u64::MAX);
        usize::try_from(hash & mask).unwrap_or(0)
    }

    fn get(&self, iri: &NamedNode) -> Option<u32> {
        if self.buckets.is_empty() {
            return None;
        }
        let hash = self.hash_iri(iri);
        let mut idx = self.buckets[Self::bucket_index(hash, self.buckets.len())];
        while idx != NO_SYNTHETIC_CALL_NODE {
            let entry = &self.entries[idx as usize];
            if entry.hash == hash && entry.iri == *iri {
                return Some(idx);
            }
            idx = entry.next_in_bucket;
        }
        None
    }

    fn intern(&mut self, iri: &NamedNode) -> Result<(u32, bool), &'static str> {
        let hash = self.hash_iri(iri);
        if !self.buckets.is_empty() {
            let mut idx = self.buckets[Self::bucket_index(hash, self.buckets.len())];
            while idx != NO_SYNTHETIC_CALL_NODE {
                let entry = &self.entries[idx as usize];
                if entry.hash == hash && entry.iri == *iri {
                    return Ok((idx, false));
                }
                idx = entry.next_in_bucket;
            }
        }
        let id = u32::try_from(self.entries.len())
            .ok()
            .filter(|&id| id != NO_SYNTHETIC_CALL_NODE)
            .ok_or("Too many distinct synthetic function calls")?;
        if self.buckets.is_empty()
            || self.entries.len().saturating_add(1).saturating_mul(4)
                > self.buckets.len().saturating_mul(3)
        {
            let new_cap = if self.buckets.is_empty() {
                8
            } else {
                self.buckets
                    .len()
                    .checked_mul(2)
                    .ok_or("Too many distinct synthetic function calls")?
            };
            let mut new_buckets = vec![NO_SYNTHETIC_CALL_NODE; new_cap];
            let mut entry_id: u32 = 0;
            for entry in &mut self.entries {
                let bucket = Self::bucket_index(entry.hash, new_cap);
                entry.next_in_bucket = new_buckets[bucket];
                new_buckets[bucket] = entry_id;
                entry_id += 1;
            }
            self.buckets = new_buckets;
        }
        let bucket = Self::bucket_index(hash, self.buckets.len());
        let next_in_bucket = self.buckets[bucket];
        self.entries.push(InternedIri {
            iri: iri.clone(),
            hash,
            next_in_bucket,
        });
        self.buckets[bucket] = id;
        Ok((id, true))
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Clone, Copy)]
struct ForestCheckpoint {
    nodes_len: usize,
    roots_head: u32,
}

#[derive(Clone, Copy)]
struct SyntheticCallNode {
    iri: u32,
    wraps_aggregate: bool,
    first_child: u32,
    next_sibling: u32,
}

/// Tracks synthetic placeholder call nesting in O(occurrences) memory and validates
/// against inferred `DISTINCT` synthetic aggregates once all inferences are known.
struct SyntheticCallTracker {
    iris: IriInterner,
    inferred_aggregates: Vec<bool>,
    in_aggregates: Vec<bool>,
    has_inferred_aggregates: bool,
    nodes: Vec<SyntheticCallNode>,
    roots_head: u32,
    recorded_selects: HashSet<usize>,
}

impl SyntheticCallTracker {
    fn new() -> Self {
        Self {
            iris: IriInterner::new(),
            inferred_aggregates: Vec::new(),
            in_aggregates: Vec::new(),
            has_inferred_aggregates: false,
            nodes: Vec::new(),
            roots_head: NO_SYNTHETIC_CALL_NODE,
            recorded_selects: HashSet::new(),
        }
    }

    fn has_recorded_select(&self, select_start: usize) -> bool {
        self.recorded_selects.contains(&select_start)
    }

    fn checkpoint(&self) -> ForestCheckpoint {
        ForestCheckpoint {
            nodes_len: self.nodes.len(),
            roots_head: self.roots_head,
        }
    }

    fn commit_select(&mut self, select_start: usize, checkpoint: ForestCheckpoint) {
        if self.nodes.len() > checkpoint.nodes_len {
            self.recorded_selects.insert(select_start);
        }
    }

    fn rollback(&mut self, checkpoint: ForestCheckpoint) {
        self.nodes.truncate(checkpoint.nodes_len);
        self.roots_head = checkpoint.roots_head;
    }

    fn is_inferred_aggregate(&self, iri: &NamedNode) -> bool {
        self.has_inferred_aggregates
            && self
                .iris
                .get(iri)
                .is_some_and(|id| self.inferred_aggregates[id as usize])
    }

    fn intern_iri(&mut self, iri: &NamedNode) -> Result<u32, &'static str> {
        let (id, inserted) = self.iris.intern(iri)?;
        if inserted {
            self.inferred_aggregates.push(false);
            self.in_aggregates.push(false);
        }
        Ok(id)
    }

    fn mark_inferred_aggregate(&mut self, iri: &NamedNode) -> Result<(), &'static str> {
        let id = self.intern_iri(iri)?;
        self.inferred_aggregates[id as usize] = true;
        self.has_inferred_aggregates = true;
        if self.in_aggregates[id as usize] {
            return Err("Aggregate functions cannot be nested");
        }
        Ok(())
    }

    fn push_node(&mut self, iri: u32, parent: Option<u32>) -> Result<u32, &'static str> {
        let idx = u32::try_from(self.nodes.len())
            .ok()
            .filter(|&idx| idx != NO_SYNTHETIC_CALL_NODE)
            .ok_or("Too many synthetic function calls")?;
        let next_sibling = match parent {
            Some(parent_idx) => self.nodes[parent_idx as usize].first_child,
            None => self.roots_head,
        };
        self.nodes.push(SyntheticCallNode {
            iri,
            wraps_aggregate: false,
            first_child: NO_SYNTHETIC_CALL_NODE,
            next_sibling,
        });
        match parent {
            Some(parent_idx) => self.nodes[parent_idx as usize].first_child = idx,
            None => self.roots_head = idx,
        }
        Ok(idx)
    }

    fn record_in_aggregate(
        &mut self,
        expression: &Expression,
        placeholder_calls: &HashMap<Variable, (NamedNode, Vec<Expression>, bool)>,
    ) -> Result<(), &'static str> {
        match expression {
            Expression::NamedNode(_)
            | Expression::Literal(_)
            | Expression::Bound(_)
            | Expression::Exists(_) => Ok(()),
            Expression::Variable(var) => {
                if let Some((iri, args, _)) = placeholder_calls.get(var) {
                    let id = self.intern_iri(iri)?;
                    self.in_aggregates[id as usize] = true;
                    for arg in args {
                        self.record_in_aggregate(arg, placeholder_calls)?;
                    }
                }
                Ok(())
            }
            Expression::UnaryPlus(e) | Expression::UnaryMinus(e) | Expression::Not(e) => {
                self.record_in_aggregate(e, placeholder_calls)
            }
            Expression::Or(a, b)
            | Expression::And(a, b)
            | Expression::Equal(a, b)
            | Expression::SameTerm(a, b)
            | Expression::Greater(a, b)
            | Expression::GreaterOrEqual(a, b)
            | Expression::Less(a, b)
            | Expression::LessOrEqual(a, b)
            | Expression::Add(a, b)
            | Expression::Subtract(a, b)
            | Expression::Multiply(a, b)
            | Expression::Divide(a, b) => {
                self.record_in_aggregate(a, placeholder_calls)?;
                self.record_in_aggregate(b, placeholder_calls)
            }
            Expression::In(a, b) => {
                self.record_in_aggregate(a, placeholder_calls)?;
                for e in b {
                    self.record_in_aggregate(e, placeholder_calls)?;
                }
                Ok(())
            }
            Expression::FunctionCall(_, parameters) | Expression::Coalesce(parameters) => {
                for p in parameters {
                    self.record_in_aggregate(p, placeholder_calls)?;
                }
                Ok(())
            }
            Expression::If(a, b, c) => {
                self.record_in_aggregate(a, placeholder_calls)?;
                self.record_in_aggregate(b, placeholder_calls)?;
                self.record_in_aggregate(c, placeholder_calls)
            }
        }
    }

    fn record_placeholder_forest(
        &mut self,
        expression: &Expression,
        placeholder_calls: &HashMap<Variable, (NamedNode, Vec<Expression>, bool)>,
        agg_vars: &HashSet<Variable>,
        parent: Option<u32>,
    ) -> Result<bool, &'static str> {
        match expression {
            Expression::NamedNode(_) | Expression::Literal(_) | Expression::Exists(_) => Ok(false),
            Expression::Bound(var) => Ok(agg_vars.contains(var)),
            Expression::Variable(var) => {
                if agg_vars.contains(var) {
                    return Ok(true);
                }
                if let Some((iri, args, _)) = placeholder_calls.get(var) {
                    let iri_id = self.intern_iri(iri)?;
                    let node_idx = self.push_node(iri_id, parent)?;
                    let mut child_has_agg = false;
                    for arg in args {
                        child_has_agg |= self.record_placeholder_forest(
                            arg,
                            placeholder_calls,
                            agg_vars,
                            Some(node_idx),
                        )?;
                    }
                    self.nodes[node_idx as usize].wraps_aggregate = child_has_agg;
                    return Ok(child_has_agg);
                }
                Ok(false)
            }
            Expression::UnaryPlus(e) | Expression::UnaryMinus(e) | Expression::Not(e) => {
                self.record_placeholder_forest(e, placeholder_calls, agg_vars, parent)
            }
            Expression::Or(a, b)
            | Expression::And(a, b)
            | Expression::Equal(a, b)
            | Expression::SameTerm(a, b)
            | Expression::Greater(a, b)
            | Expression::GreaterOrEqual(a, b)
            | Expression::Less(a, b)
            | Expression::LessOrEqual(a, b)
            | Expression::Add(a, b)
            | Expression::Subtract(a, b)
            | Expression::Multiply(a, b)
            | Expression::Divide(a, b) => {
                let lhs = self.record_placeholder_forest(a, placeholder_calls, agg_vars, parent)?;
                let rhs = self.record_placeholder_forest(b, placeholder_calls, agg_vars, parent)?;
                Ok(lhs || rhs)
            }
            Expression::In(a, b) => {
                let mut has_agg =
                    self.record_placeholder_forest(a, placeholder_calls, agg_vars, parent)?;
                for e in b {
                    has_agg |=
                        self.record_placeholder_forest(e, placeholder_calls, agg_vars, parent)?;
                }
                Ok(has_agg)
            }
            Expression::FunctionCall(_, parameters) | Expression::Coalesce(parameters) => {
                let mut has_agg = false;
                for p in parameters {
                    has_agg |=
                        self.record_placeholder_forest(p, placeholder_calls, agg_vars, parent)?;
                }
                Ok(has_agg)
            }
            Expression::If(a, b, c) => {
                let ha = self.record_placeholder_forest(a, placeholder_calls, agg_vars, parent)?;
                let hb = self.record_placeholder_forest(b, placeholder_calls, agg_vars, parent)?;
                let hc = self.record_placeholder_forest(c, placeholder_calls, agg_vars, parent)?;
                Ok(ha || hb || hc)
            }
        }
    }

    fn validate_no_nested_aggregates(&self) -> Result<(), &'static str> {
        if !self.has_inferred_aggregates {
            return Ok(());
        }
        for (&inferred, &in_agg) in self
            .inferred_aggregates
            .iter()
            .zip(self.in_aggregates.iter())
        {
            if inferred && in_agg {
                return Err("Aggregate functions cannot be nested");
            }
        }
        if self.roots_head == NO_SYNTHETIC_CALL_NODE {
            return Ok(());
        }
        let mut stack = Vec::new();
        let mut cur = (self.roots_head, false);
        loop {
            let node = self.nodes[cur.0 as usize];
            let is_inferred = self.inferred_aggregates[node.iri as usize];
            if is_inferred && (cur.1 || node.wraps_aggregate) {
                return Err("Aggregate functions cannot be nested");
            }
            let child_ancestor_inferred = cur.1 || is_inferred;
            if node.first_child != NO_SYNTHETIC_CALL_NODE {
                if node.next_sibling != NO_SYNTHETIC_CALL_NODE {
                    stack.push((node.next_sibling, cur.1));
                }
                cur = (node.first_child, child_ancestor_inferred);
            } else if node.next_sibling != NO_SYNTHETIC_CALL_NODE {
                cur = (node.next_sibling, cur.1);
            } else if let Some(next) = stack.pop() {
                cur = next;
            } else {
                break;
            }
        }
        Ok(())
    }
}

fn restore_placeholder_calls(
    expression: Expression,
    placeholder_calls: &HashMap<Variable, (NamedNode, Vec<Expression>, bool)>,
) -> Expression {
    match expression {
        Expression::Variable(var) => {
            if let Some((iri, args, _)) = placeholder_calls.get(&var) {
                Expression::FunctionCall(
                    Function::Custom(iri.clone()),
                    args.iter()
                        .cloned()
                        .map(|arg| restore_placeholder_calls(arg, placeholder_calls))
                        .collect(),
                )
            } else {
                Expression::Variable(var)
            }
        }
        Expression::NamedNode(_)
        | Expression::Literal(_)
        | Expression::Bound(_)
        | Expression::Exists(_) => expression,
        Expression::UnaryPlus(e) => {
            Expression::UnaryPlus(Box::new(restore_placeholder_calls(*e, placeholder_calls)))
        }
        Expression::UnaryMinus(e) => {
            Expression::UnaryMinus(Box::new(restore_placeholder_calls(*e, placeholder_calls)))
        }
        Expression::Not(e) => {
            Expression::Not(Box::new(restore_placeholder_calls(*e, placeholder_calls)))
        }
        Expression::Or(a, b) => Expression::Or(
            Box::new(restore_placeholder_calls(*a, placeholder_calls)),
            Box::new(restore_placeholder_calls(*b, placeholder_calls)),
        ),
        Expression::And(a, b) => Expression::And(
            Box::new(restore_placeholder_calls(*a, placeholder_calls)),
            Box::new(restore_placeholder_calls(*b, placeholder_calls)),
        ),
        Expression::Equal(a, b) => Expression::Equal(
            Box::new(restore_placeholder_calls(*a, placeholder_calls)),
            Box::new(restore_placeholder_calls(*b, placeholder_calls)),
        ),
        Expression::SameTerm(a, b) => Expression::SameTerm(
            Box::new(restore_placeholder_calls(*a, placeholder_calls)),
            Box::new(restore_placeholder_calls(*b, placeholder_calls)),
        ),
        Expression::Greater(a, b) => Expression::Greater(
            Box::new(restore_placeholder_calls(*a, placeholder_calls)),
            Box::new(restore_placeholder_calls(*b, placeholder_calls)),
        ),
        Expression::GreaterOrEqual(a, b) => Expression::GreaterOrEqual(
            Box::new(restore_placeholder_calls(*a, placeholder_calls)),
            Box::new(restore_placeholder_calls(*b, placeholder_calls)),
        ),
        Expression::Less(a, b) => Expression::Less(
            Box::new(restore_placeholder_calls(*a, placeholder_calls)),
            Box::new(restore_placeholder_calls(*b, placeholder_calls)),
        ),
        Expression::LessOrEqual(a, b) => Expression::LessOrEqual(
            Box::new(restore_placeholder_calls(*a, placeholder_calls)),
            Box::new(restore_placeholder_calls(*b, placeholder_calls)),
        ),
        Expression::Add(a, b) => Expression::Add(
            Box::new(restore_placeholder_calls(*a, placeholder_calls)),
            Box::new(restore_placeholder_calls(*b, placeholder_calls)),
        ),
        Expression::Subtract(a, b) => Expression::Subtract(
            Box::new(restore_placeholder_calls(*a, placeholder_calls)),
            Box::new(restore_placeholder_calls(*b, placeholder_calls)),
        ),
        Expression::Multiply(a, b) => Expression::Multiply(
            Box::new(restore_placeholder_calls(*a, placeholder_calls)),
            Box::new(restore_placeholder_calls(*b, placeholder_calls)),
        ),
        Expression::Divide(a, b) => Expression::Divide(
            Box::new(restore_placeholder_calls(*a, placeholder_calls)),
            Box::new(restore_placeholder_calls(*b, placeholder_calls)),
        ),
        Expression::In(a, b) => Expression::In(
            Box::new(restore_placeholder_calls(*a, placeholder_calls)),
            b.into_iter()
                .map(|e| restore_placeholder_calls(e, placeholder_calls))
                .collect(),
        ),
        Expression::FunctionCall(function, parameters) => Expression::FunctionCall(
            function,
            parameters
                .into_iter()
                .map(|p| restore_placeholder_calls(p, placeholder_calls))
                .collect(),
        ),
        Expression::Coalesce(parameters) => Expression::Coalesce(
            parameters
                .into_iter()
                .map(|p| restore_placeholder_calls(p, placeholder_calls))
                .collect(),
        ),
        Expression::If(a, b, c) => Expression::If(
            Box::new(restore_placeholder_calls(*a, placeholder_calls)),
            Box::new(restore_placeholder_calls(*b, placeholder_calls)),
            Box::new(restore_placeholder_calls(*c, placeholder_calls)),
        ),
    }
}

/// Whether `expression` mentions `variable`, not looking inside EXISTS patterns
/// (which are their own group graph patterns, not part of the expression).
fn expression_uses_variable(expression: &Expression, variable: &Variable) -> bool {
    match expression {
        Expression::NamedNode(_) | Expression::Literal(_) | Expression::Exists(_) => false,
        Expression::Variable(v) | Expression::Bound(v) => v == variable,
        Expression::UnaryPlus(e) | Expression::UnaryMinus(e) | Expression::Not(e) => {
            expression_uses_variable(e, variable)
        }
        Expression::Or(a, b)
        | Expression::And(a, b)
        | Expression::Equal(a, b)
        | Expression::SameTerm(a, b)
        | Expression::Greater(a, b)
        | Expression::GreaterOrEqual(a, b)
        | Expression::Less(a, b)
        | Expression::LessOrEqual(a, b)
        | Expression::Add(a, b)
        | Expression::Subtract(a, b)
        | Expression::Multiply(a, b)
        | Expression::Divide(a, b) => {
            expression_uses_variable(a, variable) || expression_uses_variable(b, variable)
        }
        Expression::In(a, b) => {
            expression_uses_variable(a, variable)
                || b.iter().any(|b| expression_uses_variable(b, variable))
        }
        Expression::FunctionCall(_, parameters) | Expression::Coalesce(parameters) => parameters
            .iter()
            .any(|p| expression_uses_variable(p, variable)),
        Expression::If(a, b, c) => {
            expression_uses_variable(a, variable)
                || expression_uses_variable(b, variable)
                || expression_uses_variable(c, variable)
        }
    }
}

/// Called on every variable defined using "AS" or "VALUES"
#[cfg(feature = "sep-0006")]
fn add_defined_variables<'a>(pattern: &'a GraphPattern, set: &mut HashSet<&'a Variable>) {
    match pattern {
        GraphPattern::Bgp { .. } | GraphPattern::Path { .. } => {}
        GraphPattern::Join { left, right }
        | GraphPattern::LeftJoin { left, right, .. }
        | GraphPattern::Lateral { left, right }
        | GraphPattern::Union { left, right }
        | GraphPattern::Minus { left, right } => {
            add_defined_variables(left, set);
            add_defined_variables(right, set);
        }
        GraphPattern::SemiJoin { left, .. } | GraphPattern::AntiJoin { left, .. } => {
            add_defined_variables(left, set);
        }
        GraphPattern::Graph { inner, .. } => {
            add_defined_variables(inner, set);
        }
        GraphPattern::Extend {
            inner, variable, ..
        }
        | GraphPattern::Assign {
            inner, variable, ..
        } => {
            set.insert(variable);
            add_defined_variables(inner, set);
        }
        GraphPattern::Unfold {
            inner,
            variable,
            second,
            ..
        } => {
            set.insert(variable);
            if let Some(second) = second {
                set.insert(second);
            }
            add_defined_variables(inner, set);
        }
        GraphPattern::Group {
            variables,
            aggregates,
            inner,
        } => {
            for (v, _) in aggregates {
                set.insert(v);
            }
            let mut inner_variables = HashSet::new();
            add_defined_variables(inner, &mut inner_variables);
            for v in inner_variables {
                if variables.contains(v) {
                    set.insert(v);
                }
            }
        }
        GraphPattern::Values { variables, .. } => {
            for v in variables {
                set.insert(v);
            }
        }
        GraphPattern::Project { variables, inner } => {
            let mut inner_variables = HashSet::new();
            add_defined_variables(inner, &mut inner_variables);
            for v in inner_variables {
                if variables.contains(v) {
                    set.insert(v);
                }
            }
        }
        GraphPattern::Service { inner, .. }
        | GraphPattern::Filter { inner, .. }
        | GraphPattern::OrderBy { inner, .. }
        | GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. } => add_defined_variables(inner, set),
    }
}

fn copy_graph(from: impl Into<GraphName>, to: impl Into<GraphNamePattern>) -> GraphUpdateOperation {
    let bgp = GraphPattern::Bgp {
        patterns: vec![TriplePattern::new(
            Variable::new_unchecked("s"),
            Variable::new_unchecked("p"),
            Variable::new_unchecked("o"),
        )],
    };
    GraphUpdateOperation::DeleteInsert {
        delete: Vec::new(),
        insert: vec![QuadPattern::new(
            Variable::new_unchecked("s"),
            Variable::new_unchecked("p"),
            Variable::new_unchecked("o"),
            to,
        )],
        using: None,
        pattern: Box::new(match from.into() {
            GraphName::NamedNode(from) => GraphPattern::Graph {
                name: from.into(),
                inner: Box::new(bgp),
            },
            GraphName::DefaultGraph => bgp,
        }),
    }
}

fn check_if_insert_data_are_sharing_blank_nodes(
    update: &[GraphUpdateOperation],
) -> Result<(), SparqlSyntaxError> {
    #[cfg(feature = "sparql-12")]
    fn add_triple_blank_nodes<'a>(triple: &'a Triple, bnodes: &mut HashSet<&'a BlankNode>) {
        if let NamedOrBlankNode::BlankNode(bnode) = &triple.subject {
            bnodes.insert(bnode);
        }
        if let Term::BlankNode(bnode) = &triple.object {
            bnodes.insert(bnode);
        } else if let Term::Triple(triple) = &triple.object {
            add_triple_blank_nodes(triple, bnodes);
        }
    }

    if update
        .iter()
        .filter(|op| matches!(op, GraphUpdateOperation::InsertData { .. }))
        .count()
        < 2
    {
        // Fast path, no need to validate
        return Ok(());
    }

    let mut existing_blank_nodes = HashSet::new();
    for operation in update {
        if let GraphUpdateOperation::InsertData { data } = operation {
            let mut new_blank_nodes = HashSet::new();
            for quad in data {
                if let NamedOrBlankNode::BlankNode(bnode) = &quad.subject {
                    new_blank_nodes.insert(bnode);
                }
                if let Term::BlankNode(bnode) = &quad.object {
                    new_blank_nodes.insert(bnode);
                }
                #[cfg(feature = "sparql-12")]
                if let Term::Triple(triple) = &quad.object {
                    add_triple_blank_nodes(triple, &mut new_blank_nodes);
                }
            }
            if let Some(error) = existing_blank_nodes.intersection(&new_blank_nodes).next() {
                return Err(SparqlSyntaxErrorKind::SharedBlankNode((**error).clone()).into());
            }
            existing_blank_nodes.extend(new_blank_nodes);
        }
    }
    Ok(())
}

enum Either<L, R> {
    Left(L),
    Right(R),
}

fn has_iri_scheme(s: &str) -> bool {
    let mut bytes = s.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    for b in bytes {
        if b == b':' {
            return true;
        }
        if !(b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')) {
            return false;
        }
    }
    false
}

#[derive(Clone, Copy)]
struct IriRefHead {
    len: usize,
    has_scheme: bool,
    first: Option<u8>,
    second: Option<u8>,
}

impl IriRefHead {
    fn from_plain(raw: &str) -> Self {
        let bytes = raw.as_bytes();
        Self {
            len: raw.len(),
            has_scheme: has_iri_scheme(raw),
            first: bytes.first().copied(),
            second: bytes.get(1).copied(),
        }
    }
}

fn measure_unescaped_iriref(mut input: &str) -> Result<IriRefHead, &'static str> {
    #[derive(Clone, Copy)]
    enum SchemeState {
        Start,
        InScheme,
        HasScheme,
        NoScheme,
    }

    struct Scanner {
        len: usize,
        scheme_state: SchemeState,
        first: Option<u8>,
        second: Option<u8>,
    }

    impl Scanner {
        fn feed_bytes(&mut self, bytes: &[u8]) {
            self.len = self.len.saturating_add(bytes.len());
            if self.second.is_some()
                && matches!(
                    self.scheme_state,
                    SchemeState::HasScheme | SchemeState::NoScheme
                )
            {
                return;
            }
            for &b in bytes {
                if self.first.is_none() {
                    self.first = Some(b);
                } else if self.second.is_none() {
                    self.second = Some(b);
                }
                self.scheme_state = match self.scheme_state {
                    SchemeState::Start => {
                        if b.is_ascii_alphabetic() {
                            SchemeState::InScheme
                        } else {
                            SchemeState::NoScheme
                        }
                    }
                    SchemeState::InScheme => {
                        if b == b':' {
                            SchemeState::HasScheme
                        } else if b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.') {
                            SchemeState::InScheme
                        } else {
                            SchemeState::NoScheme
                        }
                    }
                    other => other,
                };
                if self.second.is_some()
                    && matches!(
                        self.scheme_state,
                        SchemeState::HasScheme | SchemeState::NoScheme
                    )
                {
                    break;
                }
            }
        }
    }

    let mut scanner = Scanner {
        len: 0,
        scheme_state: SchemeState::Start,
        first: None,
        second: None,
    };
    let mut utf8_buf = [0u8; 4];
    while let Some((before, after)) = input.split_once('\\') {
        scanner.feed_bytes(before.as_bytes());
        let mut after = after.chars();
        let (escape, rest) = match after.next() {
            Some('u') => read_hex_char::<4>(after.as_str())?,
            Some('U') => read_hex_char::<8>(after.as_str())?,
            Some(_) => {
                return Err(
                    "IRIs are only allowed to contain escape sequences \\uXXXX and \\UXXXXXXXX",
                );
            }
            None => return Err("IRIs are not allowed to end with a '\\'"),
        };
        let encoded = escape.encode_utf8(&mut utf8_buf);
        scanner.feed_bytes(encoded.as_bytes());
        input = rest;
    }
    scanner.feed_bytes(input.as_bytes());
    Ok(IriRefHead {
        len: scanner.len,
        has_scheme: matches!(scanner.scheme_state, SchemeState::HasScheme),
        first: scanner.first,
        second: scanner.second,
    })
}

fn relative_iri_resolution_bound(base_iri: &Iri<String>, relative: IriRefHead) -> usize {
    let scheme_end = base_iri.scheme().len().saturating_add(1);
    if relative.first == Some(b'/') && relative.second == Some(b'/') {
        return scheme_end.saturating_add(relative.len);
    }
    let authority_end = scheme_end.saturating_add(
        base_iri
            .authority()
            .map_or(0, |a| a.len().saturating_add(2)),
    );
    if relative.first == Some(b'/') {
        return authority_end.saturating_add(relative.len);
    }
    let path_end = authority_end.saturating_add(base_iri.path().len());
    if relative.len == 0 || relative.first == Some(b'#') {
        let query_end = path_end.saturating_add(
            base_iri
                .query()
                .map_or(0, |q| q.len().saturating_add(1)),
        );
        return query_end.saturating_add(relative.len);
    }
    if relative.first == Some(b'?') {
        return path_end.saturating_add(relative.len);
    }
    path_end.saturating_add(1).saturating_add(relative.len)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PlaceholderScope {
    None,
    SelectProjection,
    SolutionModifier,
}

pub struct ParserState {
    base_iri: Option<Iri<String>>,
    prefixes: HashMap<String, String>,
    custom_aggregate_functions: Arc<HashSet<NamedNode>>,
    placeholder_prefixes: HashSet<String>,
    placeholder_calls: Vec<HashMap<Variable, (NamedNode, Vec<Expression>, bool)>>,
    placeholder_scope: Vec<PlaceholderScope>,
    aggregate_arg_depth: Vec<usize>,
    synthetic_calls: SyntheticCallTracker,
    max_resolved_iri_bytes: Option<usize>,
    resolved_iri_bytes: usize,
    resolved_iri_limit_exceeded: Option<usize>,
    used_bnodes: HashSet<BlankNode>,
    currently_used_bnodes: HashSet<BlankNode>,
    aggregates: Vec<Vec<(Variable, AggregateExpression)>>,
    /// Jena ARQ's syntax extensions are accepted (the `arq()` rule)
    arq: bool,
}

impl ParserState {
    pub(crate) fn new(
        base_iri: Option<Iri<String>>,
        prefixes: HashMap<String, String>,
        custom_aggregate_functions: Arc<HashSet<NamedNode>>,
    ) -> Self {
        Self {
            base_iri,
            prefixes,
            custom_aggregate_functions,
            placeholder_prefixes: HashSet::new(),
            placeholder_calls: Vec::new(),
            placeholder_scope: Vec::new(),
            aggregate_arg_depth: Vec::new(),
            synthetic_calls: SyntheticCallTracker::new(),
            max_resolved_iri_bytes: None,
            resolved_iri_bytes: 0,
            resolved_iri_limit_exceeded: None,
            used_bnodes: HashSet::new(),
            currently_used_bnodes: HashSet::new(),
            aggregates: Vec::new(),
            arq: true,
        }
    }

    fn charge_resolved_iri_bytes(&mut self, bytes: usize) -> Result<(), &'static str> {
        if self.resolved_iri_limit_exceeded.is_some() {
            return Err("query exceeds maximum resolved IRI size");
        }
        if let Some(limit) = self.max_resolved_iri_bytes {
            let next = self.resolved_iri_bytes.saturating_add(bytes);
            if next > limit {
                self.resolved_iri_limit_exceeded = Some(limit);
                return Err("query exceeds maximum resolved IRI size");
            }
            self.resolved_iri_bytes = next;
        }
        Ok(())
    }

    fn parse_iriref(&mut self, raw: &str) -> Result<Iri<String>, &'static str> {
        if self.resolved_iri_limit_exceeded.is_some() {
            return Err("query exceeds maximum resolved IRI size");
        }
        let has_escape = raw.contains('\\');
        if self.max_resolved_iri_bytes.is_some() {
            let head = if has_escape {
                measure_unescaped_iriref(raw)?
            } else {
                IriRefHead::from_plain(raw)
            };
            if let Some(base_iri) = &self.base_iri {
                if head.has_scheme {
                    self.charge_resolved_iri_bytes(head.len)?;
                    let iri_str = if has_escape {
                        unescape_iriref_with_capacity(raw, head.len)?
                    } else {
                        raw.to_owned()
                    };
                    return Iri::parse(iri_str).map_err(|_| "IRI parsing failed");
                }
                let bound = relative_iri_resolution_bound(base_iri, head);
                self.charge_resolved_iri_bytes(bound)?;
                let unescaped = if has_escape {
                    Cow::Owned(unescape_iriref_with_capacity(raw, head.len)?)
                } else {
                    Cow::Borrowed(raw)
                };
                let mut resolved = String::new();
                self.base_iri
                    .as_ref()
                    .expect("checked")
                    .resolve_into(&unescaped, &mut resolved)
                    .map_err(|_| "IRI parsing failed")?;
                if resolved.len() > bound {
                    self.charge_resolved_iri_bytes(resolved.len() - bound)?;
                }
                return Ok(Iri::parse_unchecked(resolved));
            }
            self.charge_resolved_iri_bytes(head.len)?;
            let iri_str = if has_escape {
                unescape_iriref_with_capacity(raw, head.len)?
            } else {
                raw.to_owned()
            };
            return Iri::parse(iri_str).map_err(|_| "IRI parsing failed");
        }

        let unescaped = if has_escape {
            Cow::Owned(unescape_iriref(raw)?)
        } else {
            Cow::Borrowed(raw)
        };
        if let Some(base_iri) = &self.base_iri {
            if has_iri_scheme(&unescaped) {
                return Iri::parse(unescaped.into_owned()).map_err(|_| "IRI parsing failed");
            }
            let mut resolved = String::new();
            base_iri
                .resolve_into(&unescaped, &mut resolved)
                .map_err(|_| "IRI parsing failed")?;
            Ok(Iri::parse_unchecked(resolved))
        } else {
            Iri::parse(unescaped.into_owned()).map_err(|_| "IRI parsing failed")
        }
    }

    fn build_custom_function_call(
        &mut self,
        name: NamedNode,
        is_synth: bool,
        args: Vec<Expression>,
    ) -> Result<Expression, &'static str> {
        if self.custom_aggregate_functions.contains(&name)
            || (is_synth && self.synthetic_calls.is_inferred_aggregate(&name))
        {
            return Err(
                "This custom function is an aggregate function and not a regular function",
            );
        }
        let scope = self
            .placeholder_scope
            .last()
            .copied()
            .unwrap_or(PlaceholderScope::None);
        let allow_unbound =
            is_synth && args.len() == 1 && scope == PlaceholderScope::SelectProjection;
        let track_for_nesting = is_synth
            && (scope != PlaceholderScope::None
                || self.aggregate_arg_depth.last().copied().unwrap_or(0) > 0);
        if (allow_unbound || track_for_nesting)
            && let Some(calls) = self.placeholder_calls.last_mut()
        {
            let var = variable();
            calls.insert(var.clone(), (name, args, allow_unbound));
            Ok(Expression::Variable(var))
        } else {
            Ok(Expression::FunctionCall(Function::Custom(name), args))
        }
    }

    fn new_aggregation(
        &mut self,
        agg: AggregateExpression,
        inferred_synth_agg: Option<NamedNode>,
    ) -> Result<Variable, &'static str> {
        let agg = if let Some(calls) = self.placeholder_calls.last()
            && !calls.is_empty()
        {
            match &agg {
                AggregateExpression::CountSolutions { .. } => {}
                AggregateExpression::FunctionCall { expr, .. } => {
                    self.synthetic_calls.record_in_aggregate(expr, calls)?;
                }
                AggregateExpression::Fold {
                    expr, value, order, ..
                } => {
                    self.synthetic_calls.record_in_aggregate(expr, calls)?;
                    if let Some(v) = value {
                        self.synthetic_calls.record_in_aggregate(v, calls)?;
                    }
                    for o in order {
                        let e = match o {
                            OrderExpression::Asc(e) | OrderExpression::Desc(e) => e,
                        };
                        self.synthetic_calls.record_in_aggregate(e, calls)?;
                    }
                }
            }
            match agg {
                AggregateExpression::CountSolutions { distinct } => {
                    AggregateExpression::CountSolutions { distinct }
                }
                AggregateExpression::FunctionCall {
                    name,
                    expr,
                    distinct,
                } => AggregateExpression::FunctionCall {
                    name,
                    expr: restore_placeholder_calls(expr, calls),
                    distinct,
                },
                AggregateExpression::Fold {
                    expr,
                    value,
                    distinct,
                    order,
                } => AggregateExpression::Fold {
                    expr: restore_placeholder_calls(expr, calls),
                    value: value.map(|v| restore_placeholder_calls(v, calls)),
                    distinct,
                    order: order
                        .into_iter()
                        .map(|o| match o {
                            OrderExpression::Asc(e) => {
                                OrderExpression::Asc(restore_placeholder_calls(e, calls))
                            }
                            OrderExpression::Desc(e) => {
                                OrderExpression::Desc(restore_placeholder_calls(e, calls))
                            }
                        })
                        .collect(),
                },
            }
        } else {
            agg
        };
        if let Some(name) = inferred_synth_agg {
            self.synthetic_calls.mark_inferred_aggregate(&name)?;
        }
        let aggregates = self.aggregates.last_mut().ok_or("Unexpected aggregate")?;
        // SPARQL 1.2 §19.7: "The expression argument of an aggregate function cannot
        // contain an aggregate function." An aggregate call parsed inside `expr` has
        // already been replaced by the fresh variable registered for it at this query
        // level, so a nested aggregate shows up as one of those variables.
        let args: Vec<&Expression> = match &agg {
            AggregateExpression::CountSolutions { .. } => Vec::new(),
            AggregateExpression::FunctionCall { expr, .. } => vec![expr],
            AggregateExpression::Fold {
                expr, value, order, ..
            } => [expr]
                .into_iter()
                .chain(value)
                .chain(order.iter().map(|o| match o {
                    OrderExpression::Asc(e) | OrderExpression::Desc(e) => e,
                }))
                .collect(),
        };
        if args.iter().any(|expr| {
            aggregates
                .iter()
                .any(|(v, _)| expression_uses_variable(expr, v))
        }) {
            return Err("Aggregate functions cannot be nested");
        }
        Ok(aggregates
            .iter()
            .find_map(|(v, a)| (a == &agg).then_some(v))
            .cloned()
            .unwrap_or_else(|| {
                let new_var = variable();
                aggregates.push((new_var.clone(), agg));
                new_var
            }))
    }
}

fn unescape_iriref(input: &str) -> Result<String, &'static str> {
    unescape_iriref_with_capacity(input, input.len())
}

fn unescape_iriref_with_capacity(mut input: &str, capacity: usize) -> Result<String, &'static str> {
    let mut output = String::with_capacity(capacity);
    while let Some((before, after)) = input.split_once('\\') {
        output.push_str(before);
        let mut after = after.chars();
        let (escape, after) = match after.next() {
            Some('u') => read_hex_char::<4>(after.as_str())?,
            Some('U') => read_hex_char::<8>(after.as_str())?,
            Some(_) => {
                return Err(
                    "IRIs are only allowed to contain escape sequences \\uXXXX and \\UXXXXXXXX",
                );
            }
            None => return Err("IRIs are not allowed to end with a '\\'"),
        };
        output.push(escape);
        input = after;
    }
    output.push_str(input);
    Ok(output)
}

fn unescape_string(mut input: &str) -> Result<String, &'static str> {
    let mut output = String::with_capacity(input.len());
    while let Some((before, after)) = input.split_once('\\') {
        output.push_str(before);
        let mut after = after.chars();
        let (escape, after) = match after.next() {
            Some('t') => ('\u{0009}', after.as_str()),
            Some('b') => ('\u{0008}', after.as_str()),
            Some('n') => ('\u{000A}', after.as_str()),
            Some('r') => ('\u{000D}', after.as_str()),
            Some('f') => ('\u{000C}', after.as_str()),
            Some('"') => ('\u{0022}', after.as_str()),
            Some('\'') => ('\u{0027}', after.as_str()),
            Some('\\') => ('\u{005C}', after.as_str()),
            Some('u') => read_hex_char::<4>(after.as_str())?,
            Some('U') => read_hex_char::<8>(after.as_str())?,
            Some(_) => return Err("The character that can be escaped in strings are tbnrf\"'\\"),
            None => return Err("strings are not allowed to end with a '\'"),
        };
        output.push(escape);
        input = after;
    }
    output.push_str(input);
    Ok(output)
}

fn read_hex_char<const SIZE: usize>(input: &str) -> Result<(char, &str), &'static str> {
    if let Some(escape) = input.get(..SIZE) {
        if let Some(char) = u32::from_str_radix(escape, 16)
            .ok()
            .and_then(char::from_u32)
        {
            Ok((char, &input[SIZE..]))
        } else {
            Err("\\u escape sequence should be followed by hexadecimal digits")
        }
    } else {
        Err("\\u escape sequence should be followed by hexadecimal digits")
    }
}

fn variable() -> Variable {
    Variable::new_unchecked(format!("{:x}", random::<u128>()))
}

parser! {
    //See https://www.w3.org/TR/turtle/#sec-grammar
    grammar parser(state: &mut ParserState) for str {
        pub rule QueryUnit() -> Query = Query()

        rule Query() -> Query = _ Prologue() _ q:(SelectQuery() / ConstructQuery() / DescribeQuery() / AskQuery()) _ {?
            state.synthetic_calls.validate_no_nested_aggregates()?;
            Ok(q)
        }

        pub rule UpdateInit() -> Vec<GraphUpdateOperation> = Update()

        rule Prologue() = (BaseDecl() _ / PrefixDecl() _ / VersionDecl() _)* {}

        rule BaseDecl() = i("BASE") _ i:IRIREF() {
            state.base_iri = Some(i)
        }

        rule PrefixDecl() = i("PREFIX") _ ns:PNAME_NS() _ i:IRIREF() {
            state.placeholder_prefixes.remove(ns);
            state.prefixes.insert(ns.into(), i.into_inner());
        }

        rule VersionDecl() = i("VERSION") _ VersionSpecifier() {?
            if cfg!(feature = "sparql-12") {
                Ok(())
            } else {
                Err("The VERSION declaration is only supported in SPARQL 1.2")
            }
        }

        rule VersionSpecifier() = STRING_LITERAL1() / STRING_LITERAL2() {}

        rule SelectQuery() -> Query = s:SelectClause() _ d:DatasetClauses() _ w:WhereClause() _ g:GroupClause()? _ h:HavingClause()? _ o:OrderClause()? _ l:LimitOffsetClauses()? _ v:ValuesClause() {?
            Ok(Query::Select {
                dataset: d,
                pattern: build_select(s, w, g, h, o, l, v, state)?,
                base_iri: state.base_iri.clone()
            })
        }

        rule SubSelect() -> GraphPattern = s:SelectClause() _ rest:SubSelect_rest()? {?
            if let Some((w, g, h, o, l, v)) = rest {
                build_select(s, w, g, h, o, l, v, state)
            } else {
                state.aggregates.pop();
                state.placeholder_calls.pop();
                state.aggregate_arg_depth.pop();
                Err("Expected subselect body")
            }
        }
        rule SubSelect_rest() -> (GraphPattern, Option<(Vec<Variable>, Vec<(Expression, Variable)>)>, Option<Expression>, Option<Vec<OrderExpression>>, Option<(usize, Option<usize>)>, Option<GraphPattern>) =
            w:WhereClause() _ g:GroupClause()? _ h:HavingClause()? _ o:OrderClause()? _ l:LimitOffsetClauses()? _ v:ValuesClause() {
                (w, g, h, o, l, v)
            }

        rule SelectClause() -> Selection = start:position!() i("SELECT") _ Selection_init() res:SelectClause_body()? {?
            state.placeholder_scope.pop();
            if let Some((option, variables)) = res {
                Ok(Selection { start, option, variables })
            } else {
                state.aggregates.pop();
                state.placeholder_calls.pop();
                state.aggregate_arg_depth.pop();
                Err("Expected SELECT clause variables")
            }
        }
        rule SelectClause_body() -> (SelectionOption, SelectionVariables) =
            o:SelectClause_option() _ v:SelectClause_variables() { (o, v) }
        rule Selection_init() = {
            state.aggregates.push(Vec::new());
            state.placeholder_calls.push(HashMap::new());
            state.placeholder_scope.push(PlaceholderScope::SelectProjection);
            state.aggregate_arg_depth.push(0);
        }
        rule SelectClause_option() -> SelectionOption =
            i("DISTINCT") { SelectionOption::Distinct } /
            i("REDUCED") { SelectionOption::Reduced } /
            { SelectionOption::Default }
        rule SelectClause_variables() -> SelectionVariables =
            "*" { SelectionVariables::Star } /
            p:SelectClause_member()+ { SelectionVariables::Explicit(p) }
        rule SelectClause_member() -> SelectionMember =
            v:Var() _ { SelectionMember::Variable(v) } /
            "(" _ e:Expression() _ i("AS") _ v:Var() _ ")" _ { SelectionMember::Expression(e, v) }

        rule ConstructQuery() -> Query =
            i("CONSTRUCT") _ c:ConstructTemplate() ConstructQuery_clear() _ d:DatasetClauses() _ w:WhereClause() _ g:GroupClause()? _ h:HavingClause()? _ o:OrderClause()? _ l:LimitOffsetClauses()? _ v:ValuesClause() {?
                Ok(Query::Construct {
                    template: c.0,
                    graph_templates: c.1,
                    dataset: d,
                    pattern: build_select(Selection::default(), w, g, h, o, l, v, state)?,
                    base_iri: state.base_iri.clone()
                })
            } /
            i("CONSTRUCT") _ d:DatasetClauses() _ i("WHERE") _ "{" _ !"." c:ConstructQuads() _ "}" _ g:GroupClause()? _ h:HavingClause()? _ o:OrderClause()? _ l:LimitOffsetClauses()? _ v:ValuesClause() {?
                // the pattern has the template's shape: its triples, then a GRAPH group
                // per block
                let mut pattern = GraphPattern::Bgp { patterns: c.0.clone() };
                for t in &c.1 {
                    let name = match &t.name {
                        TermPattern::NamedNode(n) => NamedNodePattern::NamedNode(n.clone()),
                        TermPattern::Variable(v) => NamedNodePattern::Variable(v.clone()),
                        _ => return Err("a GRAPH name of CONSTRUCT WHERE that is an IRI or a variable"),
                    };
                    let inner = Box::new(GraphPattern::Bgp { patterns: t.triples.clone() });
                    pattern = new_join(pattern, GraphPattern::Graph { name, inner });
                }
                Ok(Query::Construct {
                    template: c.0,
                    graph_templates: c.1,
                    dataset: d,
                    pattern: build_select(Selection::default(), pattern, g, h, o, l, v, state)?,
                    base_iri: state.base_iri.clone()
                })
            }
        rule ConstructQuery_clear() = {
            state.currently_used_bnodes.clear();
        }

        rule DescribeQuery() -> Query =
            i("DESCRIBE") _ "*" _ d:DatasetClauses() _ w:WhereClause()? _ g:GroupClause()? _ h:HavingClause()? _ o:OrderClause()? _ l:LimitOffsetClauses()? _ v:ValuesClause() {?
                Ok(Query::Describe {
                    dataset: d,
                    pattern: build_select(Selection::default(), w.unwrap_or_default(), g, h, o, l, v, state)?,
                    base_iri: state.base_iri.clone()
                })
            } /
            i("DESCRIBE") _ p:DescribeQuery_item()+ _ d:DatasetClauses() _ w:WhereClause()? _ g:GroupClause()? _ h:HavingClause()? _ o:OrderClause()? _ l:LimitOffsetClauses()? _ v:ValuesClause() {?
                Ok(Query::Describe {
                    dataset: d,
                    pattern: build_select(Selection {
                        start: 0,
                        option: SelectionOption::Default,
                        variables: SelectionVariables::Explicit(p.into_iter().map(|var_or_iri| match var_or_iri {
                            NamedNodePattern::NamedNode(n) => SelectionMember::Expression(n.into(), variable()),
                            NamedNodePattern::Variable(v) => SelectionMember::Variable(v)
                        }).collect())
                    }, w.unwrap_or_default(), g, h, o, l, v, state)?,
                    base_iri: state.base_iri.clone()
                })
            }
        rule DescribeQuery_item() -> NamedNodePattern = i:VarOrIri() _ { i }

        rule AskQuery() -> Query = i("ASK") _ d:DatasetClauses() _ w:WhereClause() _ g:GroupClause()? _ h:HavingClause()? _ o:OrderClause()? _ l:LimitOffsetClauses()? _ v:ValuesClause() {?
            Ok(Query::Ask {
                dataset: d,
                pattern: build_select(Selection::default(), w, g, h, o, l, v, state)?,
                base_iri: state.base_iri.clone()
            })
        }

        rule DatasetClause() -> (Option<NamedNode>, Option<NamedNode>) = i("FROM") _ d:(DefaultGraphClause() / NamedGraphClause()) { d }
        rule DatasetClauses() -> Option<QueryDataset> = d:DatasetClause() ** (_) {
            if d.is_empty() {
                return None;
            }
            let mut default = Vec::new();
            let mut named = Vec::new();
            for (d, n) in d {
                if let Some(d) = d {
                    default.push(d);
                }
                if let Some(n) = n {
                    named.push(n);
                }
            }
            Some(QueryDataset {
                default, named: Some(named)
            })
        }

        rule DefaultGraphClause() -> (Option<NamedNode>, Option<NamedNode>) = s:SourceSelector() {
            (Some(s), None)
        }

        rule NamedGraphClause() -> (Option<NamedNode>, Option<NamedNode>) = i("NAMED") _ s:SourceSelector() {
            (None, Some(s))
        }

        rule SourceSelector() -> NamedNode = iri()

        rule WhereClause() -> GraphPattern = i("WHERE")? _ p:GroupGraphPattern() {
            p
        }

        rule GroupClause() -> (Vec<Variable>, Vec<(Expression,Variable)>) = i("GROUP") _ i("BY") _ c:GroupCondition_item()+ {
            let mut projections: Vec<(Expression,Variable)> = Vec::new();
            let clauses = c.into_iter().map(|(e, vo)| {
                if let Expression::Variable(v) = e {
                    v
                } else {
                    let v = vo.unwrap_or_else(variable);
                    projections.push((e, v.clone()));
                    v
                }
            }).collect();
            (clauses, projections)
        }
        rule GroupCondition_item() -> (Expression, Option<Variable>) = c:GroupCondition() _ { c }

        rule GroupCondition() -> (Expression, Option<Variable>) =
            e:BuiltInCall() { (e, None) } /
            e:FunctionCall() { (e, None) } /
            "(" _ e:Expression() _ v:GroupCondition_as()? ")" { (e, v) } /
            e:Var() { (e.into(), None) }
        rule GroupCondition_as() -> Variable = i("AS") _ v:Var() _ { v }

        rule Modifier_enter() = {
            state.placeholder_scope.push(PlaceholderScope::SolutionModifier);
        }

        rule HavingClause() -> Expression = i("HAVING") _ Modifier_enter() e:HavingClause_body()? {?
            state.placeholder_scope.pop();
            e.ok_or("Expected HAVING condition")?
        }
        rule HavingClause_body() -> Result<Expression, &'static str> = e:HavingCondition()+ {
            not_empty_fold(e.into_iter(), |a, b| Expression::And(Box::new(a), Box::new(b)))
        }

        rule HavingCondition() -> Expression = c:Constraint() _ { c }

        rule OrderClause() -> Vec<OrderExpression> = i("ORDER") _ i("BY") _ Modifier_enter() c:OrderClause_body()? {?
            state.placeholder_scope.pop();
            c.ok_or("Expected ORDER BY condition")
        }
        rule OrderClause_body() -> Vec<OrderExpression> = c:OrderClause_item()+ { c }
        rule OrderClause_item() -> OrderExpression = c:OrderCondition() _ { c }

        rule OrderCondition() -> OrderExpression =
            i("ASC") _ e: BrackettedExpression() { OrderExpression::Asc(e) } /
            i("DESC") _ e: BrackettedExpression() { OrderExpression::Desc(e) } /
            e: Constraint() { OrderExpression::Asc(e) } /
            v: Var() { OrderExpression::Asc(Expression::from(v)) }

        rule LimitOffsetClauses() -> (usize, Option<usize>) =
            l:LimitClause() _ o:OffsetClause()? { (o.unwrap_or(0), Some(l)) } /
            o:OffsetClause() _ l:LimitClause()? { (o, l) }

        rule LimitClause() -> usize = i("LIMIT") _ l:$(INTEGER()) {?
            usize::from_str(l).map_err(|_| "The query limit should be a non negative integer")
        }

        rule OffsetClause() -> usize = i("OFFSET") _ o:$(INTEGER()) {?
            usize::from_str(o).map_err(|_| "The query offset should be a non negative integer")
        }

        rule ValuesClause() -> Option<GraphPattern> =
            i("VALUES") _ p:DataBlock() { Some(p) } /
            { None }

        rule Update() -> Vec<GraphUpdateOperation> = _ Prologue() _ u:(Update_item() ** (";" _))  ( ";" _)? { u.into_iter().flatten().collect() }
        rule Update_item() -> Vec<GraphUpdateOperation> = u:Update1() Update_clear() _ { u }
        rule Update_clear() = {
            state.used_bnodes.clear();
            state.currently_used_bnodes.clear();
        }

        rule Update1() -> Vec<GraphUpdateOperation> = Load() / Clear() / Drop() / Add() / Move() / Copy() / Create() / InsertData() / DeleteData() / DeleteWhere() / Modify()
        rule Update1_silent() -> bool = i("SILENT") { true } / { false }

        rule Load() -> Vec<GraphUpdateOperation> = i("LOAD") _ silent:Update1_silent() _ source:iri() _ destination:Load_to()? {
            vec![GraphUpdateOperation::Load { silent, source, destination: destination.map_or(GraphName::DefaultGraph, GraphName::NamedNode) }]
        }
        rule Load_to() -> NamedNode = i("INTO") _ g: GraphRef() { g }

        rule Clear() -> Vec<GraphUpdateOperation> = i("CLEAR") _ silent:Update1_silent() _ graph:GraphRefAll() {
            vec![GraphUpdateOperation::Clear { silent, graph }]
        }

        rule Drop() -> Vec<GraphUpdateOperation> = i("DROP") _ silent:Update1_silent() _ graph:GraphRefAll() {
            vec![GraphUpdateOperation::Drop { silent, graph }]
        }

        rule Create() -> Vec<GraphUpdateOperation> = i("CREATE") _ silent:Update1_silent() _ graph:GraphRef() {
            vec![GraphUpdateOperation::Create { silent, graph }]
        }

        rule Add() -> Vec<GraphUpdateOperation> = i("ADD") _ silent:Update1_silent() _ from:GraphOrDefault() _ i("TO") _ to:GraphOrDefault() {
            // Rewriting defined by https://www.w3.org/TR/sparql11-update/#add
            if from == to {
                Vec::new() // identity case
            } else {
                let bgp = GraphPattern::Bgp { patterns: vec![TriplePattern::new(Variable::new_unchecked("s"), Variable::new_unchecked("p"), Variable::new_unchecked("o"))] };
                vec![copy_graph(from, to)]
            }
        }

        rule Move() -> Vec<GraphUpdateOperation> = i("MOVE") _ silent:Update1_silent() _ from:GraphOrDefault() _ i("TO") _ to:GraphOrDefault() {
            // Rewriting defined by https://www.w3.org/TR/sparql11-update/#move
            if from == to {
                Vec::new() // identity case
            } else {
                let bgp = GraphPattern::Bgp { patterns: vec![TriplePattern::new(Variable::new_unchecked("s"), Variable::new_unchecked("p"), Variable::new_unchecked("o"))] };
                vec![GraphUpdateOperation::Drop { silent: true, graph: to.clone().into() }, copy_graph(from.clone(), to), GraphUpdateOperation::Drop { silent, graph: from.into() }]
            }
        }

        rule Copy() -> Vec<GraphUpdateOperation> = i("COPY") _ silent:Update1_silent() _ from:GraphOrDefault() _ i("TO") _ to:GraphOrDefault() {
            // Rewriting defined by https://www.w3.org/TR/sparql11-update/#copy
            if from == to {
                Vec::new() // identity case
            } else {
                let bgp = GraphPattern::Bgp { patterns: vec![TriplePattern::new(Variable::new_unchecked("s"), Variable::new_unchecked("p"), Variable::new_unchecked("o"))] };
                vec![GraphUpdateOperation::Drop { silent: true, graph: to.clone().into() }, copy_graph(from, to)]
            }
        }

        rule InsertData() -> Vec<GraphUpdateOperation> = i("INSERT") _ i("DATA") _ data:QuadData() {
            vec![GraphUpdateOperation::InsertData { data }]
        }

        rule DeleteData() -> Vec<GraphUpdateOperation> = i("DELETE") _ i("DATA") _ data:GroundQuadData() {
            vec![GraphUpdateOperation::DeleteData { data }]
        }

        rule DeleteWhere() -> Vec<GraphUpdateOperation> = i("DELETE") _ i("WHERE") _ d:QuadPattern() {?
            let pattern = d.iter().map(|q| {
                let bgp = GraphPattern::Bgp { patterns: vec![TriplePattern::new(q.subject.clone(), q.predicate.clone(), q.object.clone())] };
                match &q.graph_name {
                    GraphNamePattern::NamedNode(graph_name) => GraphPattern::Graph { name: graph_name.clone().into(), inner: Box::new(bgp) },
                    GraphNamePattern::DefaultGraph => bgp,
                    GraphNamePattern::Variable(graph_name) => GraphPattern::Graph { name: graph_name.clone().into(), inner: Box::new(bgp) },
                }
            }).reduce(new_join).unwrap_or_default();
            let delete = d.into_iter().map(GroundQuadPattern::try_from).collect::<Result<Vec<_>,_>>().map_err(|()| "Blank nodes are not allowed in DELETE WHERE")?;
            Ok(vec![GraphUpdateOperation::DeleteInsert {
                delete,
                insert: Vec::new(),
                using: None,
                pattern: Box::new(pattern)
            }])
        }

        rule Modify() -> Vec<GraphUpdateOperation> = with:Modify_with()? _ c:Modify_clauses() _ u:(UsingClause() ** (_)) _ i("WHERE") _ pattern:GroupGraphPattern() {
            let (delete, insert) = c;
            let mut delete = delete.unwrap_or_default();
            let mut insert = insert.unwrap_or_default();
            #[expect(clippy::shadow_same)]
            let mut pattern = pattern;

            let mut using = if u.is_empty() {
                None
            } else {
                let mut default = Vec::new();
                let mut named = Vec::new();
                for (d, n) in u {
                    if let Some(d) = d {
                        default.push(d)
                    }
                    if let Some(n) = n {
                        named.push(n)
                    }
                }
                Some(QueryDataset { default, named: Some(named) })
            };

            if let Some(with) = with {
                // We inject WITH everywhere
                delete = delete.into_iter().map(|q| if q.graph_name == GraphNamePattern::DefaultGraph {
                    GroundQuadPattern {
                        subject: q.subject,
                        predicate: q.predicate,
                        object: q.object,
                        graph_name: with.clone().into()
                    }
                } else {
                    q
                }).collect();
                insert = insert.into_iter().map(|q| if q.graph_name == GraphNamePattern::DefaultGraph {
                    QuadPattern {
                        subject: q.subject,
                        predicate: q.predicate,
                        object: q.object,
                        graph_name: with.clone().into()
                    }
                } else {
                    q
                }).collect();
                if using.is_none() {
                    using = Some(QueryDataset { default: vec![with], named: None });
                }
            }

            vec![GraphUpdateOperation::DeleteInsert {
                delete,
                insert,
                using,
                pattern: Box::new(pattern)
            }]
        }
        rule Modify_with() -> NamedNode = i("WITH") _ i:iri() _ { i }
        rule Modify_clauses() -> (Option<Vec<GroundQuadPattern>>, Option<Vec<QuadPattern>>) = d:DeleteClause() Modify_clear() _ i:InsertClause()? Modify_clear() {
            (Some(d), i)
        } / i:InsertClause() Modify_clear() {
            (None, Some(i))
        }
        rule Modify_clear() = {
            state.currently_used_bnodes.clear();
        }

        rule DeleteClause() -> Vec<GroundQuadPattern> = i("DELETE") _ q:QuadPattern() {?
            q.into_iter().map(GroundQuadPattern::try_from).collect::<Result<Vec<_>,_>>().map_err(|()| "Blank nodes are not allowed in DELETE WHERE")
        }

        rule InsertClause() -> Vec<QuadPattern> = i("INSERT") _ q:QuadPattern() { q }

        rule UsingClause() -> (Option<NamedNode>, Option<NamedNode>) = i("USING") _ d:(UsingClause_default() / UsingClause_named()) { d }
        rule UsingClause_default() -> (Option<NamedNode>, Option<NamedNode>) = i:iri() {
            (Some(i), None)
        }
        rule UsingClause_named() -> (Option<NamedNode>, Option<NamedNode>) = i("NAMED") _ i:iri() {
            (None, Some(i))
        }

        rule GraphOrDefault() -> GraphName = i("DEFAULT") {
            GraphName::DefaultGraph
        } / (i("GRAPH") _)? g:iri() {
            GraphName::NamedNode(g)
        }

        rule GraphRef() -> NamedNode = i("GRAPH") _ g:iri() { g }

        rule GraphRefAll() -> GraphTarget  = i: GraphRef() { i.into() }
            / i("DEFAULT") { GraphTarget::DefaultGraph }
            / i("NAMED") { GraphTarget::NamedGraphs }
            / i("ALL") { GraphTarget::AllGraphs }

        rule QuadPattern() -> Vec<QuadPattern> = "{" _ q:Quads() _ "}" { q }

        rule QuadData() -> Vec<Quad> = "{" _ q:Quads() _ "}" {?
            q.into_iter().map(Quad::try_from).collect::<Result<Vec<_>, ()>>().map_err(|()| "Variables are not allowed in INSERT DATA")
        }
        rule GroundQuadData() -> Vec<GroundQuad> = "{" _ q:Quads() _ "}" {?
            q.into_iter().map(|q| GroundQuad::try_from(Quad::try_from(q)?)).collect::<Result<Vec<_>, ()>>().map_err(|()| "Variables and blank nodes are not allowed in DELETE DATA")
        }

        rule Quads() -> Vec<QuadPattern> = q:(Quads_TriplesTemplate() / Quads_QuadsNotTriples()) ** (_) {
            q.into_iter().flatten().collect()
        }
        rule Quads_TriplesTemplate() -> Vec<QuadPattern> = t:TriplesTemplate() {
            t.into_iter().map(|t| QuadPattern::new(t.subject, t.predicate, t.object, GraphNamePattern::DefaultGraph)).collect()
        } //TODO: return iter?
        rule Quads_QuadsNotTriples() -> Vec<QuadPattern> = q:QuadsNotTriples() _ "."? { q }

        rule QuadsNotTriples() -> Vec<QuadPattern> = i("GRAPH") _ g:VarOrIri() _ "{" _ t:TriplesTemplate()? _ "}" {
            t.unwrap_or_default().into_iter().map(|t| QuadPattern::new(t.subject, t.predicate, t.object, g.clone())).collect()
        }

        rule TriplesTemplate() -> Vec<TriplePattern> = ts:TriplesTemplate_inner() ++ (".") ("." _)? {
            ts.into_iter().flatten().collect()
        }
        rule TriplesTemplate_inner() -> Vec<TriplePattern> = _ t:TriplesSameSubject() _ { t }

        rule GroupGraphPattern() -> GraphPattern = p:GroupGraphPatternWithFilters() {
            match p {
                (inner, Some(expr)) => GraphPattern::Filter { expr, inner: Box::new(inner) },
                (inner, None) => inner,
            }
        }
        // A group's pattern and the conjunction of the FILTERs written directly in it
        // (SPARQL 1.1 §18.2.2.7), kept apart so that OPTIONAL can tell them from a filter
        // of a nested group.
        rule GroupGraphPatternWithFilters() -> (GraphPattern, Option<Expression>) =
            "{" _ GroupGraphPattern_clear() p:GroupGraphPatternSub() GroupGraphPattern_clear() _ "}" { p } /
            "{" _ GroupGraphPattern_clear() p:SubSelect() GroupGraphPattern_clear() _ "}" { (p, None) }
        rule GroupGraphPattern_clear() = {
             // We deal with blank nodes aliases rule
            state.used_bnodes.extend(state.currently_used_bnodes.iter().cloned());
            state.currently_used_bnodes.clear();
        }

        rule GroupGraphPatternSub() -> (GraphPattern, Option<Expression>) = a:TriplesBlock()? _ b:GroupGraphPatternSub_item()* {?
            let mut filter: Option<Expression> = None;
            let mut g = a.map_or_else(GraphPattern::default, build_bgp);
            for e in b.into_iter().flatten() {
                match e {
                    PartialGraphPattern::Optional(p, f) => {
                        g = GraphPattern::LeftJoin { left: Box::new(g), right: Box::new(p), expression: f }
                    }
                    #[cfg(feature = "sep-0006")]
                    PartialGraphPattern::Lateral(p) => {
                        let mut defined_variables = HashSet::new();
                        add_defined_variables(&p, &mut defined_variables);
                        let mut contains = false;
                        g.on_in_scope_variable(|v| {
                            if defined_variables.contains(v) {
                                contains = true;
                            }
                        });
                        if contains {
                            return Err("An existing variable is overridden in the right side of LATERAL");
                        }
                        g = GraphPattern::Lateral { left: Box::new(g), right: Box::new(p) }
                    }
                    PartialGraphPattern::Minus(p) => {
                        g = GraphPattern::Minus { left: Box::new(g), right: Box::new(p) }
                    }
                    PartialGraphPattern::SemiJoin(p) => {
                        g = GraphPattern::SemiJoin { left: Box::new(g), right: Box::new(p) }
                    }
                    PartialGraphPattern::AntiJoin(p) => {
                        g = GraphPattern::AntiJoin { left: Box::new(g), right: Box::new(p) }
                    }
                    PartialGraphPattern::Bind(expression, variable) => {
                        let mut contains = false;
                        g.on_in_scope_variable(|v| {
                            if *v == variable {
                                contains = true;
                            }
                        });
                        if contains {
                            return Err("BIND is overriding an existing variable")
                        }
                        g = GraphPattern::Extend { inner: Box::new(g), variable, expression }
                    }
                    // ARQ's LET may assign a variable that is in scope (`SyntaxVarScope`)
                    PartialGraphPattern::Assign(expression, variable) => {
                        g = GraphPattern::Assign { inner: Box::new(g), variable, expression }
                    }
                    // ARQ's `checkUNFOLD`: neither variable may be in scope
                    PartialGraphPattern::Unfold(expression, variable, second) => {
                        let mut contains = false;
                        g.on_in_scope_variable(|v| {
                            if *v == variable || second.as_ref() == Some(v) {
                                contains = true;
                            }
                        });
                        if contains || second.as_ref() == Some(&variable) {
                            return Err("UNFOLD is overriding an existing variable")
                        }
                        g = GraphPattern::Unfold { inner: Box::new(g), expression, variable, second }
                    }
                    PartialGraphPattern::Filter(expr) => filter = Some(if let Some(f) = filter {
                        Expression::And(Box::new(f), Box::new(expr))
                    } else {
                        expr
                    }),
                    PartialGraphPattern::Other(e) => g = new_join(g, e),
                }
            }

            Ok((g, filter))
        }
        rule GroupGraphPatternSub_item() -> Vec<PartialGraphPattern> = a:GraphPatternNotTriples() _ ("." _)? b:TriplesBlock()? _ {
            let mut result = vec![a];
            if let Some(v) = b {
                result.push(PartialGraphPattern::Other(build_bgp(v)));
            }
            result
        }

        rule TriplesBlock() -> Vec<TripleOrPathPattern> = hs:TriplesBlock_inner() ++ (".") ("." _)? {
            hs.into_iter().flatten().collect()
        }
        rule TriplesBlock_inner() -> Vec<TripleOrPathPattern> = _ h:TriplesSameSubjectPath() _ { h }

        rule ReifiedTripleBlock() -> Vec<TriplePattern> = s:ReifiedTriple() _ po:PropertyList() {?
            let mut patterns = po.patterns;
            patterns.extend(s.patterns);
            for (p, os) in po.focus {
                for o in os {
                    add_to_triple_patterns(s.focus.clone(), p.clone(), o, &mut patterns)?;
                }
            }
            Ok(patterns)
        }

        rule ReifiedTripleBlockPath() -> Vec<TripleOrPathPattern> = s:ReifiedTriple() _ po:PropertyListPath() {?
            let mut patterns = po.patterns;
            patterns.extend(s.patterns.into_iter().map(Into::into));
            for (p, os) in po.focus {
                for o in os {
                    add_to_triple_or_path_patterns(s.focus.clone(), p.clone(), o, &mut patterns)?;
                }
            }
            Ok(patterns)
        }

        rule GraphPatternNotTriples() -> PartialGraphPattern = GroupOrUnionGraphPattern() / OptionalGraphPattern() / LateralGraphPattern() / MinusGraphPattern() / GraphGraphPattern() / ServiceGraphPattern() / Filter() / Bind() / InlineData() / Assignment() / Unfold() / SemiJoinGraphPattern() / AntiJoinGraphPattern()

        // ARQ's `SEMIJOIN { … }` and `ANTIJOIN { … }`
        rule SemiJoinGraphPattern() -> PartialGraphPattern = i("SEMIJOIN") _ arq() p:GroupGraphPattern() {
            PartialGraphPattern::SemiJoin(p)
        }
        rule AntiJoinGraphPattern() -> PartialGraphPattern = i("ANTIJOIN") _ arq() p:GroupGraphPattern() {
            PartialGraphPattern::AntiJoin(p)
        }

        // ARQ's `LET (?v := expr)` (`Assignment` in ARQ's grammar)
        rule Assignment() -> PartialGraphPattern = i("LET") _ arq() "(" _ v:Var() _ ":=" _ e:Expression() _ ")" {
            PartialGraphPattern::Assign(e, v)
        }

        // ARQ's `UNFOLD(expr AS ?v1)` and `UNFOLD(expr AS ?v1, ?v2)`
        rule Unfold() -> PartialGraphPattern = i("UNFOLD") _ arq() "(" _ e:Expression() _ i("AS") _ v:Var() _ w:Unfold_second()? ")" {
            PartialGraphPattern::Unfold(e, v, w)
        }
        rule Unfold_second() -> Variable = "," _ v:Var() _ { v }

        // SPARQL 1.1 §18.2.2.6: OPTIONAL{P} is LeftJoin(G, A2, F) when Translate(P) is
        // Filter(F, A2), i.e. when P itself has FILTERs (§18.2.2.7). The simplification of
        // Join(Z, A) to A (§18.2.2.8) comes after the translation, so in
        // `OPTIONAL { { P FILTER(e) } }` the filter stays inside the right-hand side, where
        // it only sees P's bindings, instead of becoming the LeftJoin expression.
        rule OptionalGraphPattern() -> PartialGraphPattern = i("OPTIONAL") _ p:GroupGraphPatternWithFilters() {
            PartialGraphPattern::Optional(p.0, p.1)
        }

        rule LateralGraphPattern() -> PartialGraphPattern = i("LATERAL") _ arq() p:GroupGraphPattern() {?
                #[cfg(feature = "sep-0006")]{Ok(PartialGraphPattern::Lateral(p))}
                #[cfg(not(feature = "sep-0006"))]{Err("The LATERAL modifier is not supported")}
        }

        rule GraphGraphPattern() -> PartialGraphPattern = i("GRAPH") _ name:VarOrIri() _ p:GroupGraphPattern() {
            PartialGraphPattern::Other(GraphPattern::Graph { name, inner: Box::new(p) })
        }

        rule ServiceGraphPattern() -> PartialGraphPattern =
            i("SERVICE") _ i("SILENT") _ name:VarOrIri() _ p:GroupGraphPattern() { PartialGraphPattern::Other(GraphPattern::Service { name, inner: Box::new(p), silent: true }) } /
            i("SERVICE") _ name:VarOrIri() _ p:GroupGraphPattern() { PartialGraphPattern::Other(GraphPattern::Service{ name, inner: Box::new(p), silent: false }) }

        rule Bind() -> PartialGraphPattern = i("BIND") _ "(" _ e:Expression() _ i("AS") _ v:Var() _ ")" {
            PartialGraphPattern::Bind(e, v)
        }

        rule InlineData() -> PartialGraphPattern = i("VALUES") _ p:DataBlock() { PartialGraphPattern::Other(p) }

        rule DataBlock() -> GraphPattern = l:(InlineDataOneVar() / InlineDataFull()) {
            GraphPattern::Values { variables: l.0, bindings: l.1 }
        }

        rule InlineDataOneVar() -> (Vec<Variable>, Vec<Vec<Option<GroundTerm>>>) = var:Var() _ "{" _ d:InlineDataOneVar_value()* "}" {
            (vec![var], d)
        }
        rule InlineDataOneVar_value() -> Vec<Option<GroundTerm>> = t:DataBlockValue() _ { vec![t] }

        rule InlineDataFull() -> (Vec<Variable>, Vec<Vec<Option<GroundTerm>>>) = "(" _ vars:InlineDataFull_var()* _ ")" _ "{" _ vals:InlineDataFull_values()* "}" {?
            if vars.iter().enumerate().any(|(i, vl)| vars[i+1..].contains(vl)) {
                Err("Repeated variables are not allowed in VALUES clauses.")
            } else if vals.iter().any(|vs| vs.len() != vars.len()) {
                Err("The VALUES clause rows should have exactly the same number of values as there are variables. To set a value to undefined use UNDEF.")
            } else {
                Ok((vars, vals))
            }
        }
        rule InlineDataFull_var() -> Variable = v:Var() _ { v }
        rule InlineDataFull_values() -> Vec<Option<GroundTerm>> = "(" _ v:InlineDataFull_value()* _ ")" _ { v }
        rule InlineDataFull_value() -> Option<GroundTerm> = v:DataBlockValue() _ { v }

        rule DataBlockValue() -> Option<GroundTerm> =
            t:TripleTermData() {?
                #[cfg(feature = "sparql-12")]{Ok(Some(t.into()))}
                #[cfg(not(feature = "sparql-12"))]{Err("Triple terms are only available in SPARQL 1.2")}
            } /
            i:iri() { Some(i.into()) } /
            l:RDFLiteral() { Some(l.into()) } /
            l:NumericLiteral() { Some(l.into()) } /
            l:BooleanLiteral() { Some(l.into()) } /
            i("UNDEF") { None }

        rule Reifier() -> TermPattern = "~" _ v:VarOrReifierId()? { v.unwrap_or_else(|| BlankNode::default().into()) }

        rule VarOrReifierId() -> TermPattern =
            v:Var() { v.into() } /
            i:iri() { i.into() } /
            b:BlankNode() { b.into() }

        rule MinusGraphPattern() -> PartialGraphPattern = i("MINUS") _ p: GroupGraphPattern() {
            PartialGraphPattern::Minus(p)
        }

        rule GroupOrUnionGraphPattern() -> PartialGraphPattern = p:GroupOrUnionGraphPattern_item() **<1,> (i("UNION") _) {?
            not_empty_fold(p.into_iter(), |a, b| {
                GraphPattern::Union { left: Box::new(a), right: Box::new(b) }
            }).map(PartialGraphPattern::Other)
        }
        rule GroupOrUnionGraphPattern_item() -> GraphPattern = p:GroupGraphPattern() _ { p }

        rule Filter() -> PartialGraphPattern = i("FILTER") _ c:Constraint() {
            PartialGraphPattern::Filter(c)
        }

        rule Constraint() -> Expression = BrackettedExpression() / FunctionCall() / BuiltInCall()

        rule FunctionCall() -> Expression = f_and_synth:iri_with_synthetic_flag() _ a:ArgList() {?
            let (f, is_synth) = f_and_synth;
            state.build_custom_function_call(f, is_synth, a)
        }

        rule ArgList() -> Vec<Expression> =
            "(" _ e:ArgList_item() **<1,> ("," _) _ ")" { e } /
            NIL() { Vec::new() }
        rule ArgList_item() -> Expression = e:Expression() _ { e }

        rule ExpressionList() -> Vec<Expression> =
            "(" _ e:ExpressionList_item() **<1,> ("," _) ")" { e } /
            NIL() { Vec::new() }
        rule ExpressionList_item() -> Expression = e:Expression() _ { e }

        rule ConstructTemplate() -> (Vec<TriplePattern>, Vec<GraphTemplate>) = "{" _ t:ConstructQuads() _ "}" { t }

        // Jena ARQ's TriG-like template (`ConstructQuads`): triples, `GRAPH g { … }`
        // blocks and bare `{ … }` blocks for the default graph, in any order. A SPARQL
        // template is the triples alone.
        rule ConstructQuads() -> (Vec<TriplePattern>, Vec<GraphTemplate>) = t:ConstructTriples() _ b:ConstructQuads_block()* {
            let mut triples = t;
            let mut graphs = Vec::new();
            for (graph, more) in b {
                if let Some(g) = graph {
                    graphs.push(g);
                }
                triples.extend(more);
            }
            (triples, graphs)
        }
        // a block and the triples after it; a bare block's triples are in the second part
        rule ConstructQuads_block() -> (Option<GraphTemplate>, Vec<TriplePattern>) = arq() q:ConstructQuadsNotTriples() _ "."? _ t:ConstructTriples() _ {
            match q {
                (Some(name), triples) => (Some(GraphTemplate { name, triples }), t),
                (None, mut triples) => {
                    triples.extend(t);
                    (None, triples)
                }
            }
        }
        rule ConstructQuadsNotTriples() -> (Option<TermPattern>, Vec<TriplePattern>) =
            i("GRAPH") _ g:ConstructGraphName() _ "{" _ t:ConstructTriples() _ "}" { (Some(g), t) } /
            "{" _ t:ConstructTriples() _ "}" { (None, t) }
        rule ConstructGraphName() -> TermPattern =
            v:Var() { v.into() } /
            b:BlankNode() { b.into() } /
            i:iri() { i.into() }

        rule ConstructTriples() -> Vec<TriplePattern> = p:ConstructTriples_item() ** ("." _) "."? {
            p.into_iter().flatten().collect()
        }
        rule ConstructTriples_item() -> Vec<TriplePattern> = t:TriplesSameSubject() _ { t }

        rule TriplesSameSubject() -> Vec<TriplePattern> =
            ReifiedTripleBlock() /
            s:VarOrTerm() _ po:PropertyListNotEmpty() {?
                let mut patterns = po.patterns;
                for (p, os) in po.focus {
                    for o in os {
                        add_to_triple_patterns(s.clone(), p.clone(), o, &mut patterns)?
                    }
                }
                Ok(patterns)
            } /
            s:TriplesNode() _ po:PropertyList() {?
                let mut patterns = s.patterns;
                patterns.extend(po.patterns);
                for (p, os) in po.focus {
                    for o in os {
                        add_to_triple_patterns(s.focus.clone(), p.clone(), o, &mut patterns)?
                    }
                }
                Ok(patterns)
            }

        rule PropertyList() -> FocusedTriplePattern<Vec<(NamedNodePattern,Vec<ReifiedTerm>)>> =
            PropertyListNotEmpty() /
            { FocusedTriplePattern::default() }

        rule PlaceholderCompactA_guard() = ns:PNAME_NS() {?
            if state.resolved_iri_limit_exceeded.is_none()
                && ns.starts_with('a')
                && state.placeholder_prefixes.contains(ns)
            {
                Ok(())
            } else {
                Err("not a placeholder-shadowed compact `a` verb")
            }
        }
        rule PlaceholderCompactA() = &PlaceholderCompactA_guard() "a" {}

        rule PropertyListNotEmpty_head() -> (NamedNodePattern, FocusedTriplePattern<Vec<ReifiedTerm>>) =
            hp:Verb() _ ho:ObjectList() { (hp, ho) } /
            PlaceholderCompactA() _ ho:ObjectList() { (rdf::TYPE.into_owned().into(), ho) }

        rule PropertyListNotEmpty() -> FocusedTriplePattern<Vec<(NamedNodePattern,Vec<ReifiedTerm>)>> = head:PropertyListNotEmpty_head() _ l:PropertyListNotEmpty_item()* {
            let (hp, ho) = head;
            l.into_iter().flatten().fold(FocusedTriplePattern {
                focus: vec![(hp, ho.focus)],
                patterns: ho.patterns
            }, |mut a, b| {
                a.focus.push(b.focus);
                a.patterns.extend(b.patterns);
                a
            })
        }
        rule PropertyListNotEmpty_item() -> Option<FocusedTriplePattern<(NamedNodePattern,Vec<ReifiedTerm>)>> = ";" _ c:PropertyListNotEmpty_item_content()? {
            c
        }
        rule PropertyListNotEmpty_item_content() -> FocusedTriplePattern<(NamedNodePattern,Vec<ReifiedTerm>)> = head:PropertyListNotEmpty_head() _ {
            let (p, o) = head;
            FocusedTriplePattern {
                focus: (p, o.focus),
                patterns: o.patterns
            }
        }

        rule Verb() -> NamedNodePattern = VarOrIri() / "a" { rdf::TYPE.into_owned().into() }

        rule ObjectList() -> FocusedTriplePattern<Vec<ReifiedTerm >> = o:ObjectList_item() **<1,> ("," _) {
            o.into_iter().fold(FocusedTriplePattern::<Vec<ReifiedTerm >>::default(), |mut a, b| {
                a.focus.push(b.focus);
                a.patterns.extend_from_slice(&b.patterns);
                a
            })
        }
        rule ObjectList_item() -> FocusedTriplePattern<ReifiedTerm> = o:Object() _ { o }

        rule Object() -> FocusedTriplePattern<ReifiedTerm> = g:GraphNode() _ a:Annotation() {
            let mut patterns = g.patterns;
            patterns.extend(a.patterns);
            FocusedTriplePattern {
                focus: ReifiedTerm {
                    term: g.focus,
                    reifiers: a.focus
                },
                patterns
            }
        }

        rule TriplesSameSubjectPath() -> Vec<TripleOrPathPattern> =
            ReifiedTripleBlockPath() /
            s:VarOrTerm() _ po:PropertyListPathNotEmpty() {?
                let mut patterns = po.patterns;
                for (p, os) in po.focus {
                    for o in os {
                        add_to_triple_or_path_patterns(s.clone(), p.clone(), o, &mut patterns)?;
                    }
                }
                Ok(patterns)
            } /
            s:TriplesNodePath() _ po:PropertyListPath() {?
                let mut patterns = s.patterns;
                patterns.extend(po.patterns);
                for (p, os) in po.focus {
                    for o in os {
                        add_to_triple_or_path_patterns(s.focus.clone(), p.clone(), o, &mut patterns)?;
                    }
                }
                Ok(patterns)
            }

        rule PropertyListPath() -> FocusedTripleOrPathPattern<Vec<(VariableOrPropertyPath,Vec<ReifiedTerm>)>> =
            PropertyListPathNotEmpty() /
            { FocusedTripleOrPathPattern::default() }

        rule PropertyListPathNotEmpty_head() -> (VariableOrPropertyPath, FocusedTripleOrPathPattern<Vec<ReifiedTerm>>) =
            hp:(VerbPath() / VerbSimple()) _ ho:ObjectListPath() { (hp, ho) } /
            PlaceholderCompactA() _ ho:ObjectListPath() { (PropertyPathExpression::NamedNode(rdf::TYPE.into_owned()).into(), ho) }

        rule PropertyListPathNotEmpty() -> FocusedTripleOrPathPattern<Vec<(VariableOrPropertyPath,Vec<ReifiedTerm>)>> = head:PropertyListPathNotEmpty_head() _ t:PropertyListPathNotEmpty_item()* {
                let (hp, ho) = head;
                t.into_iter().flatten().fold(FocusedTripleOrPathPattern {
                    focus: vec![(hp, ho.focus)],
                    patterns: ho.patterns
                }, |mut a, b| {
                    a.focus.push(b.focus);
                    a.patterns.extend(b.patterns);
                    a
                })
        }
        rule PropertyListPathNotEmpty_item() -> Option<FocusedTripleOrPathPattern<(VariableOrPropertyPath,Vec<ReifiedTerm>)>> = ";" _ c:PropertyListPathNotEmpty_item_content()? {
            c
        }
        rule PropertyListPathNotEmpty_item_content() -> FocusedTripleOrPathPattern<(VariableOrPropertyPath,Vec<ReifiedTerm>)> = head:PropertyListPathNotEmpty_head() _ {
            let (p, o) = head;
            FocusedTripleOrPathPattern {
                focus: (p, o.focus),
                patterns: o.patterns
            }
        }

        rule VerbPath() -> VariableOrPropertyPath = p:Path() {
            p.into()
        }

        rule VerbSimple() -> VariableOrPropertyPath = v:Var() {
            v.into()
        }

        rule ObjectListPath() -> FocusedTripleOrPathPattern<Vec<ReifiedTerm>> = o:ObjectListPath_item() **<1,> ("," _) {
            o.into_iter().fold(FocusedTripleOrPathPattern::<Vec<ReifiedTerm>>::default(), |mut a, b| {
                a.focus.push(b.focus);
                a.patterns.extend(b.patterns);
                a
            })
        }
        rule ObjectListPath_item() -> FocusedTripleOrPathPattern<ReifiedTerm> = o:ObjectPath() _ { o }

        rule ObjectPath() -> FocusedTripleOrPathPattern<ReifiedTerm> = g:GraphNodePath() _ a:AnnotationPath() {
            let mut patterns = g.patterns;
            patterns.extend(a.patterns);
            FocusedTripleOrPathPattern {
                focus: ReifiedTerm {
                    term: g.focus,
                    reifiers: a.focus
                },
                patterns
            }
        }

        rule Path() -> PropertyPathExpression = PathAlternative()

        rule PathAlternative() -> PropertyPathExpression = p:PathAlternative_item() **<1,> ("|" _) {?
            not_empty_fold(p.into_iter(), |a, b| {
                PropertyPathExpression::Alternative(Box::new(a), Box::new(b))
            })
        }
        rule PathAlternative_item() -> PropertyPathExpression = p:PathSequence() _ { p }

        rule PathSequence() -> PropertyPathExpression = first:PathSequence_item() rest:PathSequence_next()* {
            rest.into_iter().fold(first, |a, b| {
                PropertyPathExpression::Sequence(Box::new(a), Box::new(b))
            })
        }
        // `/ elt`, or ARQ's `^ elt`: `:p^:q` is `:p/^:q`
        rule PathSequence_next() -> PropertyPathExpression =
            "/" _ p:PathSequence_item() { p } /
            "^" _ arq() p:PathElt() _ { PropertyPathExpression::Reverse(Box::new(p)) }
        rule PathSequence_item() -> PropertyPathExpression = p:PathEltOrInverse() _ { p }

        rule PathElt() -> PropertyPathExpression = p:PathPrimary() _ o:PathElt_op()? {
            match o {
                Some(PathMod::ZeroOrOne) => PropertyPathExpression::ZeroOrOne(Box::new(p)),
                Some(PathMod::ZeroOrMore) => PropertyPathExpression::ZeroOrMore(Box::new(p)),
                Some(PathMod::OneOrMore) => PropertyPathExpression::OneOrMore(Box::new(p)),
                Some(PathMod::Range(min, max)) => PropertyPathExpression::Range { path: Box::new(p), min, max },
                None => p
            }
        }
        rule PathElt_op() -> PathMod =
            "*" { PathMod::ZeroOrMore } /
            "+" { PathMod::OneOrMore } /
            "?" !(['0'..='9'] / PN_CHARS_U()) { PathMod::ZeroOrOne } / // We mandate that this is not a variable
            "{" _ arq() r:PathRange() _ "}" { r }
        // ARQ's `PathMod` braces: {*} {+} {n} {n,m} {n,} {,m}
        rule PathRange() -> PathMod =
            "*" { PathMod::Range(0, None) } /
            "+" { PathMod::Range(1, None) } /
            n:PathLength() _ "," _ m:PathLength() {?
                if n <= m { Ok(PathMod::Range(n, Some(m))) } else { Err("a path range whose minimum is at most its maximum") }
            } /
            n:PathLength() _ "," { PathMod::Range(n, None) } /
            n:PathLength() { PathMod::Range(n, Some(n)) } /
            "," _ m:PathLength() { PathMod::Range(0, Some(m)) }
        rule PathLength() -> u64 = n:$(['0'..='9']+) {? n.parse().map_err(|_| "a path length below 2^64") }

        rule PathEltOrInverse() -> PropertyPathExpression =
            "^" _ p:PathElt() { PropertyPathExpression::Reverse(Box::new(p)) } /
            PathElt()

        rule PathPrimary() -> PropertyPathExpression =
            v:iri() { v.into() } /
            "a" { rdf::TYPE.into_owned().into() } /
            "!" _ p:PathNegatedPropertySet() { p } /
            "(" _ p:Path() _ ")" { p } /
            // ARQ's `distinct(path)`, `multi(path)` and `shortest(path)`
            i("DISTINCT") _ "(" _ arq() p:Path() _ ")" { PropertyPathExpression::Distinct(Box::new(p)) } /
            i("MULTI") _ "(" _ arq() p:Path() _ ")" { PropertyPathExpression::Multi(Box::new(p)) } /
            i("SHORTEST") _ "(" _ arq() p:Path() _ ")" { PropertyPathExpression::Shortest(Box::new(p)) }

        rule PathNegatedPropertySet() -> PropertyPathExpression =
            "(" _ p:PathNegatedPropertySet_item() **<1,> ("|" _) ")" {
                let mut direct = Vec::new();
                let mut inverse = Vec::new();
                for e in p {
                    match e {
                        Either::Left(a) => direct.push(a),
                        Either::Right(b) => inverse.push(b)
                    }
                }
                if inverse.is_empty() {
                    PropertyPathExpression::NegatedPropertySet(direct)
                } else if direct.is_empty() {
                   PropertyPathExpression::Reverse(Box::new(PropertyPathExpression::NegatedPropertySet(inverse)))
                } else {
                    PropertyPathExpression::Alternative(
                        Box::new(PropertyPathExpression::NegatedPropertySet(direct)),
                        Box::new(PropertyPathExpression::Reverse(Box::new(PropertyPathExpression::NegatedPropertySet(inverse))))
                    )
                }
            } /
            p:PathOneInPropertySet() {
                match p {
                    Either::Left(a) => PropertyPathExpression::NegatedPropertySet(vec![a]),
                    Either::Right(b) => PropertyPathExpression::Reverse(Box::new(PropertyPathExpression::NegatedPropertySet(vec![b]))),
                }
            }
        rule PathNegatedPropertySet_item() -> Either<NamedNode,NamedNode> = p:PathOneInPropertySet() _ { p }

        rule PathOneInPropertySet() -> Either<NamedNode,NamedNode> =
            "^" _ v:iri() { Either::Right(v) } /
            "^" _ "a" { Either::Right(rdf::TYPE.into()) } /
            v:iri() { Either::Left(v) } /
            "a" { Either::Left(rdf::TYPE.into()) }

        rule TriplesNode() -> FocusedTriplePattern<TermPattern> = Collection() / BlankNodePropertyList()

        rule BlankNodePropertyList() -> FocusedTriplePattern<TermPattern> = "[" _ po:PropertyListNotEmpty() _ "]" {?
            let mut patterns = po.patterns;
            let mut bnode = TermPattern::from(BlankNode::default());
            for (p, os) in po.focus {
                for o in os {
                    add_to_triple_patterns(bnode.clone(), p.clone(), o, &mut patterns)?;
                }
            }
            Ok(FocusedTriplePattern {
                focus: bnode,
                patterns
            })
        }

        rule TriplesNodePath() -> FocusedTripleOrPathPattern<TermPattern> = CollectionPath() / BlankNodePropertyListPath()

        rule BlankNodePropertyListPath() -> FocusedTripleOrPathPattern<TermPattern> = "[" _ po:PropertyListPathNotEmpty() _ "]" {?
            let mut patterns = po.patterns;
            let mut bnode = TermPattern::from(BlankNode::default());
            for (p, os) in po.focus {
                for o in os {
                    add_to_triple_or_path_patterns(bnode.clone(), p.clone(), o, &mut patterns)?;
                }
            }
            Ok(FocusedTripleOrPathPattern {
                focus: bnode,
                patterns
            })
        }

        rule Collection() -> FocusedTriplePattern<TermPattern> = "(" _ o:Collection_item()+ ")" {
            let mut patterns: Vec<TriplePattern> = Vec::new();
            let mut current_list_node = TermPattern::from(rdf::NIL.into_owned());
            for objWithPatterns in o.into_iter().rev() {
                let new_blank_node = TermPattern::from(BlankNode::default());
                patterns.push(TriplePattern::new(new_blank_node.clone(), rdf::FIRST.into_owned(), objWithPatterns.focus.clone()));
                patterns.push(TriplePattern::new(new_blank_node.clone(), rdf::REST.into_owned(), current_list_node));
                current_list_node = new_blank_node;
                patterns.extend_from_slice(&objWithPatterns.patterns);
            }
            FocusedTriplePattern {
                focus: current_list_node,
                patterns
            }
        }
        rule Collection_item() -> FocusedTriplePattern<TermPattern> = o:GraphNode() _ { o }

        rule CollectionPath() -> FocusedTripleOrPathPattern<TermPattern> = "(" _ o:CollectionPath_item()+ _ ")" {
            let mut patterns: Vec<TripleOrPathPattern> = Vec::new();
            let mut current_list_node = TermPattern::from(rdf::NIL.into_owned());
            for objWithPatterns in o.into_iter().rev() {
                let new_blank_node = TermPattern::from(BlankNode::default());
                patterns.push(TriplePattern::new(new_blank_node.clone(), rdf::FIRST.into_owned(), objWithPatterns.focus.clone()).into());
                patterns.push(TriplePattern::new(new_blank_node.clone(), rdf::REST.into_owned(), current_list_node).into());
                current_list_node = new_blank_node;
                patterns.extend(objWithPatterns.patterns);
            }
            FocusedTripleOrPathPattern {
                focus: current_list_node,
                patterns
            }
        }
        rule CollectionPath_item() -> FocusedTripleOrPathPattern<TermPattern> = p:GraphNodePath() _ { p }

        rule VarOrTerm() -> TermPattern =
            v:Var() { v.into() } /
            t:TripleTerm() {?
                #[cfg(feature = "sparql-12")]{Ok(t.into())}
                #[cfg(not(feature = "sparql-12"))]{Err("Triple terms are only available in SPARQL 1.2")}
            } /
            i:iri() { i.into() } /
            l:RDFLiteral() { l.into() } /
            l:NumericLiteral() { l.into() } /
            l:BooleanLiteral() { l.into() } /
            b:BlankNode() { b.into() } /
            NIL() { rdf::NIL.into_owned().into() }

        rule AnnotationPath() -> FocusedTripleOrPathPattern<Vec<TermPattern>> = a:AnnotationPath_e()* {
            let mut output: FocusedTripleOrPathPattern<Vec<TermPattern>> = FocusedTripleOrPathPattern::new(Vec::new());
            for a in a {
                output.focus.push(a.focus);
                output.patterns.extend(a.patterns);
            }
            output
        }
        rule AnnotationPath_e() -> FocusedTripleOrPathPattern<TermPattern> =
            r:Reifier() _ a:AnnotationBlockPath()? _ {?
                let mut output: FocusedTripleOrPathPattern<TermPattern> = FocusedTripleOrPathPattern::new(r);
                if let Some(annotations) = a {
                    for (p, os) in annotations.focus {
                        for o in os {
                            add_to_triple_or_path_patterns(output.focus.clone(), p.clone(), o, &mut output.patterns)?;
                        }
                    }
                    output.patterns.extend(annotations.patterns);
                }
                Ok(output)
            } /
            a:AnnotationBlockPath() _ {?
                let mut output: FocusedTripleOrPathPattern<TermPattern> = FocusedTripleOrPathPattern::new(BlankNode::default());
                for (p, os) in a.focus {
                    for o in os {
                        add_to_triple_or_path_patterns(output.focus.clone(), p.clone(), o, &mut output.patterns)?;
                    }
                }
                output.patterns.extend(a.patterns);
                Ok(output)
            }

        rule AnnotationBlockPath() -> FocusedTripleOrPathPattern<Vec<(VariableOrPropertyPath,Vec<ReifiedTerm>)>> = "{|" _ a:PropertyListPathNotEmpty() _ "|}" { a }

        rule Annotation() -> FocusedTriplePattern<Vec<TermPattern>> = a:Annotation_e()* {
            let mut output: FocusedTriplePattern<Vec<TermPattern>> = FocusedTriplePattern::new(Vec::new());
            for a in a {
                output.focus.push(a.focus);
                output.patterns.extend(a.patterns);
            }
            output
        }
        rule Annotation_e() -> FocusedTriplePattern<TermPattern> =
            r:Reifier() _ a:AnnotationBlock()? _ {?
                let mut output: FocusedTriplePattern<TermPattern> = FocusedTriplePattern::new(r);
                if let Some(annotations) = a {
                    for (p, os) in annotations.focus {
                        for o in os {
                            add_to_triple_patterns(output.focus.clone(), p.clone(), o, &mut output.patterns)?;
                        }
                    }
                    output.patterns.extend(annotations.patterns);
                }
                Ok(output)
            } /
            a:AnnotationBlock() _ {?
                let mut output: FocusedTriplePattern<TermPattern> = FocusedTriplePattern::new(BlankNode::default());
                for (p, os) in a.focus {
                    for o in os {
                        add_to_triple_patterns(output.focus.clone(), p.clone(), o, &mut output.patterns)?;
                    }
                }
                output.patterns.extend(a.patterns);
                Ok(output)
            }

        rule AnnotationBlock() -> FocusedTriplePattern<Vec<(NamedNodePattern,Vec<ReifiedTerm>)>> = "{|" _ a:PropertyListNotEmpty() _ "|}" { a }

        rule GraphNode() -> FocusedTriplePattern<TermPattern> =
            ReifiedTriple() /
            t:VarOrTerm() { FocusedTriplePattern::new(t) } /
            TriplesNode()

        rule GraphNodePath() -> FocusedTripleOrPathPattern<TermPattern> =
            t:ReifiedTriple() { t.into() } /
            t:VarOrTerm() { FocusedTripleOrPathPattern::new(t) } /
            TriplesNodePath()

        rule ReifiedTriple_po() -> (NamedNodePattern, FocusedTriplePattern<TermPattern>) =
            p:Verb() _ o:ReifiedTripleObject() { (p, o) } /
            PlaceholderCompactA() _ o:ReifiedTripleObject() { (rdf::TYPE.into_owned().into(), o) }

        rule ReifiedTriple() -> FocusedTriplePattern<TermPattern> = "<<" _ s:ReifiedTripleSubject() _ po:ReifiedTriple_po() _ r:Reifier()? _ ">>" {?
            #[cfg(feature = "sparql-12")]
            {
                let (p, o) = po;
                let r = r.unwrap_or_else(|| BlankNode::default().into());
                let mut output = FocusedTriplePattern::new(r.clone());
                output.patterns.push(TriplePattern {
                        subject: r,
                        predicate: rdf::REIFIES.into_owned().into(),
                        object: TriplePattern {
                            subject: s.focus,
                            predicate: p,
                            object: o.focus
                        }.into()
                    });
                output.patterns.extend(s.patterns);
                output.patterns.extend(o.patterns);
                Ok(output)
            }
            #[cfg(not(feature = "sparql-12"))]
            {
                Err("Reified triples are only available in SPARQL 1.2")
            }
        }

        rule ReifiedTripleSubject() -> FocusedTriplePattern<TermPattern> = ReifiedTripleObject()

        rule ReifiedTripleObject() -> FocusedTriplePattern<TermPattern> =
            v:Var() { FocusedTriplePattern::new(v) } /
            t:TripleTerm() {?
                #[cfg(feature = "sparql-12")]{Ok(FocusedTriplePattern::new(t))}
                #[cfg(not(feature = "sparql-12"))]{Err("Triples terms are only available in SPARQL 1.2")}
            } /
            ReifiedTriple() /
            i:iri() { FocusedTriplePattern::new(i) } /
            l:RDFLiteral() { FocusedTriplePattern::new(l) } /
            l:NumericLiteral() { FocusedTriplePattern::new(l) } /
            l:BooleanLiteral() { FocusedTriplePattern::new(l) } /
            b:BlankNode() { FocusedTriplePattern::new(b) }

        rule TripleTerm_po() -> (NamedNodePattern, TermPattern) =
            p:Verb() _ o:TripleTermObject() { (p, o) } /
            PlaceholderCompactA() _ o:TripleTermObject() { (rdf::TYPE.into_owned().into(), o) }

        rule TripleTerm() -> TriplePattern = "<<(" _ s:TripleTermSubject() _ po:TripleTerm_po() _ ")>>" {
            let (p, o) = po;
            TriplePattern {
                subject: s,
                predicate: p,
                object: o
            }
        }

        rule TripleTermSubject() -> TermPattern = TripleTermObject()

        rule TripleTermObject() -> TermPattern =
            v:Var() { v.into() } /
            t:TripleTerm() {?
                #[cfg(feature = "sparql-12")]{Ok(t.into())}
                #[cfg(not(feature = "sparql-12"))]{Err("Triples terms are only available in SPARQL 1.2")}
            } /
            i:iri() { i.into() } /
            l:RDFLiteral() { l.into() } /
            l:NumericLiteral() { l.into() } /
            l:BooleanLiteral() { l.into() } /
            b:BlankNode() { b.into() }

        rule TripleTermData_po() -> (NamedNode, GroundTerm) =
            p:TripleTermData_p() _ o:TripleTermDataObject() { (p, o) } /
            PlaceholderCompactA() _ o:TripleTermDataObject() { (rdf::TYPE.into_owned(), o) }

        rule TripleTermData() -> GroundTriple = "<<(" _ s:TripleTermDataSubject() _ po:TripleTermData_po() _ ")>>" {
            let (p, o) = po;
            GroundTriple {
                subject: s,
                predicate: p,
                object: o
            }
        }
        rule TripleTermData_p() -> NamedNode = i: iri() { i } / "a" { rdf::TYPE.into() }

        // SPARQL 1.2 §19.7 [123] TripleTermDataSubject ::= iri
        rule TripleTermDataSubject() -> NamedNode = iri()

        rule TripleTermDataObject() -> GroundTerm =
            t:TripleTermData() {?
                #[cfg(feature = "sparql-12")]{Ok(t.into())}
                #[cfg(not(feature = "sparql-12"))]{Err("Triples terms are only available in SPARQL 1.2")}
            } /
            i:iri() { i.into() } /
            l:RDFLiteral() { l.into() } /
            l:NumericLiteral() { l.into() } /
            l:BooleanLiteral() { l.into() }

        rule VarOrIri() -> NamedNodePattern =
            v:Var() { v.into() } /
            i:iri() { i.into() }

        rule Var() -> Variable = name:(VAR1() / VAR2()) { Variable::new_unchecked(name) }

        rule Expression() -> Expression = e:ConditionalOrExpression() {e}

        rule ConditionalOrExpression() -> Expression = e:ConditionalOrExpression_item() **<1,> ("||" _) {?
            not_empty_fold(e.into_iter(), |a, b| Expression::Or(Box::new(a), Box::new(b)))
        }
        rule ConditionalOrExpression_item() -> Expression = e:ConditionalAndExpression() _ { e }

        rule ConditionalAndExpression() -> Expression = e:ConditionalAndExpression_item() **<1,> ("&&" _) {?
            not_empty_fold(e.into_iter(), |a, b| Expression::And(Box::new(a), Box::new(b)))
        }
        rule ConditionalAndExpression_item() -> Expression = e:ValueLogical() _ { e }

        rule ValueLogical() -> Expression = RelationalExpression()

        rule RelationalExpression() -> Expression = a:NumericExpression() _ o: RelationalExpression_inner()? { match o {
            Some(("=", Some(b), None)) => Expression::Equal(Box::new(a), Box::new(b)),
            Some(("!=", Some(b), None)) => Expression::Not(Box::new(Expression::Equal(Box::new(a), Box::new(b)))),
            Some((">", Some(b), None)) => Expression::Greater(Box::new(a), Box::new(b)),
            Some((">=", Some(b), None)) => Expression::GreaterOrEqual(Box::new(a), Box::new(b)),
            Some(("<", Some(b), None)) => Expression::Less(Box::new(a), Box::new(b)),
            Some(("<=", Some(b), None)) => Expression::LessOrEqual(Box::new(a), Box::new(b)),
            Some(("IN", None, Some(l))) => Expression::In(Box::new(a), l),
            Some(("NOT IN", None, Some(l))) => Expression::Not(Box::new(Expression::In(Box::new(a), l))),
            Some(_) => unreachable!(),
            None => a
        } }
        // SPARQL 1.1 §19.8: "When tokenizing the input and choosing grammar rules, the
        // longest match is chosen". Where the input from `<` on forms an IRIREF token,
        // that token wins over the `<` / `<=` operator: `?x<?a&&?b>?y` is `?x <?a&&?b> ?y`,
        // a syntax error, not `?x < ?a && ?b > ?y`.
        rule RelationalExpression_inner() -> (&'input str, Option<Expression>, Option<Vec<Expression>>) =
            s: $("="  / "!=" / ">=" / ">" / !IRIREF_token() ("<=" / "<")) _ e:NumericExpression() { (s, Some(e), None) } /
            i("IN") _ l:ExpressionList() { ("IN", None, Some(l)) } /
            i("NOT") _ i("IN") _ l:ExpressionList() { ("NOT IN", None, Some(l)) }

        rule NumericExpression() -> Expression = AdditiveExpression()

        rule AdditiveExpression() -> Expression = a:MultiplicativeExpression() _ o:AdditiveExpression_inner()* {
            o.into_iter().fold(a, |a, (operator, b)| match operator {
                "+" => Expression::Add(Box::new(a), Box::new(b)),
                "-" => Expression::Subtract(Box::new(a), Box::new(b)),
                _ => unreachable!(),
            })
        }
        rule AdditiveExpression_inner() -> (&'input str, Expression) = s: $("+" / "-") _ e:MultiplicativeExpression() _ {
            (s, e)
        }

        rule MultiplicativeExpression() -> Expression = a:UnaryExpression() _ o:MultiplicativeExpression_inner()* {
            o.into_iter().fold(a, |a, (operator, b)| match operator {
                "*" => Expression::Multiply(Box::new(a), Box::new(b)),
                "/" => Expression::Divide(Box::new(a), Box::new(b)),
                _ => unreachable!(),
            })
        }
        rule MultiplicativeExpression_inner() -> (&'input str, Expression) = s: $("*" / "/") _ e:UnaryExpression() _ {
            (s, e)
        }

        rule UnaryExpression() -> Expression = s: "!" _ e:UnaryExpression() {?
            #[cfg(feature = "sparql-12")]{Ok(Expression::Not(Box::new(e)))}
            #[cfg(not(feature = "sparql-12"))]{Err("Double negation (!!) is only available in SPARQL 1.2")}
        } / s: $("!" / "+" / "-")? _ e:PrimaryExpression() { match s {
            Some("!") => Expression::Not(Box::new(e)),
            Some("+") => Expression::UnaryPlus(Box::new(e)),
            Some("-") => Expression::UnaryMinus(Box::new(e)),
            Some(_) => unreachable!(),
            None => e,
        } }

        rule PrimaryExpression() -> Expression =
            BrackettedExpression() /
            ExprTripleTerm() /
            iriOrFunction() /
            v:Var() { v.into() } /
            l:RDFLiteral() { l.into() } /
            l:NumericLiteral() { l.into() } /
            l:BooleanLiteral() { l.into() } /
            BuiltInCall()

        rule ExprTripleTerm_po() -> (NamedNodePattern, Expression) =
            p:Verb() _ o:ExprTripleTermObject() { (p, o) } /
            PlaceholderCompactA() _ o:ExprTripleTermObject() { (rdf::TYPE.into_owned().into(), o) }

        rule ExprTripleTerm() -> Expression = "<<(" _ s:ExprTripleTermSubject() _ po:ExprTripleTerm_po() _ ")>>" {?
            #[cfg(feature = "sparql-12")]
            {
                let (p, o) = po;
                Ok(Expression::FunctionCall(Function::Triple, vec![s, p.into(), o]))
            }
            #[cfg(not(feature = "sparql-12"))]{Err("Triple terms are only available in SPARQL 1.2")}
        }

        // SPARQL 1.2 §19.7 [138] ExprTripleTermSubject ::= iri | Var: no literal and no
        // nested triple term in the subject of `<<( ... )>>` in an expression.
        rule ExprTripleTermSubject() -> Expression =
            i:iri() { i.into() } /
            v:Var() { v.into() }

        rule ExprTripleTermObject() -> Expression =
            ExprTripleTerm() /
            i:iri() { i.into() } /
            l:RDFLiteral() { l.into() } /
            l:NumericLiteral() { l.into() } /
            l:BooleanLiteral() { l.into() } /
            v:Var() { v.into() }

        rule BrackettedExpression() -> Expression = "(" _ e:Expression() _ ")" { e }

        rule BuiltInCall() -> Expression =
            a:Aggregate_or_inferred() {? state.new_aggregation(a.0, a.1).map(Into::into) } /
            i("STR") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Str, vec![e]) } /
            i("LANG") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Lang, vec![e]) } /
            i("LANGMATCHES") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::FunctionCall(Function::LangMatches, vec![a, b]) } /
            i("LANGDIR") _ "(" _ e:Expression() _ ")" {?
                #[cfg(feature = "sparql-12")]{Ok(Expression::FunctionCall(Function::LangDir, vec![e]))}
                #[cfg(not(feature = "sparql-12"))]{Err("The LANGDIR function is only available in SPARQL 1.2")}
            } /
            i("DATATYPE") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Datatype, vec![e]) } /
            i("BOUND") _ "(" _ v:Var() _ ")" { Expression::Bound(v) } /
            (i("IRI") / i("URI")) _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Iri, vec![e]) } /
            i("BNODE") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::BNode, vec![e]) } /
            i("BNODE") NIL() { Expression::FunctionCall(Function::BNode, vec![]) }  /
            i("RAND") _ NIL() { Expression::FunctionCall(Function::Rand, vec![]) } /
            i("ABS") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Abs, vec![e]) } /
            i("CEIL") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Ceil, vec![e]) } /
            i("FLOOR") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Floor, vec![e]) } /
            i("ROUND") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Round, vec![e]) } /
            i("CONCAT") e:ExpressionList() { Expression::FunctionCall(Function::Concat, e) } /
            SubstringExpression() /
            i("STRLEN") _ "(" _ e: Expression() _ ")" { Expression::FunctionCall(Function::StrLen, vec![e]) } /
            StrReplaceExpression() /
            i("UCASE") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::UCase, vec![e]) } /
            i("LCASE") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::LCase, vec![e]) } /
            i("ENCODE_FOR_URI") _ "(" _ e: Expression() _ ")" { Expression::FunctionCall(Function::EncodeForUri, vec![e]) } /
            i("CONTAINS") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::FunctionCall(Function::Contains, vec![a, b]) } /
            i("STRSTARTS") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::FunctionCall(Function::StrStarts, vec![a, b]) } /
            i("STRENDS") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::FunctionCall(Function::StrEnds, vec![a, b]) } /
            i("STRBEFORE") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::FunctionCall(Function::StrBefore, vec![a, b]) } /
            i("STRAFTER") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::FunctionCall(Function::StrAfter, vec![a, b]) } /
            i("YEAR") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Year, vec![e]) } /
            i("MONTH") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Month, vec![e]) } /
            i("DAY") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Day, vec![e]) } /
            i("HOURS") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Hours, vec![e]) } /
            i("MINUTES") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Minutes, vec![e]) } /
            i("SECONDS") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Seconds, vec![e]) } /
            i("TIMEZONE") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Timezone, vec![e]) } /
            i("TZ") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Tz, vec![e]) } /
            i("NOW") _ NIL() { Expression::FunctionCall(Function::Now, vec![]) } /
            i("UUID") _ NIL() { Expression::FunctionCall(Function::Uuid, vec![]) }/
            i("STRUUID") _ NIL() { Expression::FunctionCall(Function::StrUuid, vec![]) } /
            i("MD5") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Md5, vec![e]) } /
            i("SHA1") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Sha1, vec![e]) } /
            i("SHA256") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Sha256, vec![e]) } /
            i("SHA384") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Sha384, vec![e]) } /
            i("SHA512") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::Sha512, vec![e]) } /
            i("COALESCE") e:ExpressionList() { Expression::Coalesce(e) } /
            i("IF") _ "(" _ a:Expression() _ "," _ b:Expression() _ "," _ c:Expression() _ ")" { Expression::If(Box::new(a), Box::new(b), Box::new(c)) } /
            i("STRLANG") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::FunctionCall(Function::StrLang, vec![a, b]) }  /
            i("STRLANGDIR") _ "(" _ a:Expression() _ "," _ b:Expression() _ "," _ c:Expression() _ ")" {?
                #[cfg(feature = "sparql-12")]{Ok(Expression::FunctionCall(Function::StrLangDir, vec![a, b, c]))}
                #[cfg(not(feature = "sparql-12"))]{Err("The STRLANGDIR function is only available in SPARQL 1.2")}
            } /
            i("STRDT") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::FunctionCall(Function::StrDt, vec![a, b]) } /
            i("sameTerm") _ "(" _ a:Expression() _ "," _ b:Expression() _ ")" { Expression::SameTerm(Box::new(a), Box::new(b)) } /
            (i("isIRI") / i("isURI")) _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::IsIri, vec![e]) } /
            i("isBLANK") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::IsBlank, vec![e]) } /
            i("isLITERAL") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::IsLiteral, vec![e]) } /
            i("isNUMERIC") _ "(" _ e:Expression() _ ")" { Expression::FunctionCall(Function::IsNumeric, vec![e]) } /
            i("hasLang") _ "(" _ e:Expression() _ ")" {?
                #[cfg(feature = "sparql-12")]{Ok(Expression::FunctionCall(Function::HasLang, vec![e]))}
                #[cfg(not(feature = "sparql-12"))]{Err("The hasLang function is only available in SPARQL 1.2")}
            } /
            i("hasLangDir") _ "(" _ e:Expression() _ ")" {?
                #[cfg(feature = "sparql-12")]{Ok(Expression::FunctionCall(Function::HasLangDir, vec![e]))}
                #[cfg(not(feature = "sparql-12"))]{Err("The hasLangDir function is only available in SPARQL 1.2")}
            } /
            RegexExpression() /
            ExistsFunc() /
            NotExistsFunc() /
            i("TRIPLE") _ "(" _ s:Expression() _ "," _ p:Expression() "," _ o:Expression() ")" {?
                #[cfg(feature = "sparql-12")]{Ok(Expression::FunctionCall(Function::Triple, vec![s, p, o]))}
                #[cfg(not(feature = "sparql-12"))]{Err("The TRIPLE function is only available in SPARQL 1.2")}
            } /
            i("SUBJECT") _ "(" _ e:Expression() _ ")" {?
                #[cfg(feature = "sparql-12")]{Ok(Expression::FunctionCall(Function::Subject, vec![e]))}
                #[cfg(not(feature = "sparql-12"))]{Err("The SUBJECT function is only available in SPARQL 1.2")}
            } /
            i("PREDICATE") _ "(" _ e:Expression() _ ")" {?
                #[cfg(feature = "sparql-12")]{Ok(Expression::FunctionCall(Function::Predicate, vec![e]))}
                #[cfg(not(feature = "sparql-12"))]{Err("The PREDICATE function is only available in SPARQL 1.2")}
            } /
            i("OBJECT") _ "(" _ e:Expression() _ ")" {?
                #[cfg(feature = "sparql-12")]{Ok(Expression::FunctionCall(Function::Object, vec![e]))}
                #[cfg(not(feature = "sparql-12"))]{Err("The OBJECT function is only available in SPARQL 1.2")}
            } /
            i("isTriple") _ "(" _ e:Expression() _ ")" {?
                #[cfg(feature = "sparql-12")]{Ok(Expression::FunctionCall(Function::IsTriple, vec![e]))}
                #[cfg(not(feature = "sparql-12"))]{Err("The isTriple function is only available in SPARQL 1.2")}
            } /
            i("ADJUST") _ "("  _ a:Expression() _ "," _ b:Expression() _ ")" {?
                #[cfg(feature = "sep-0002")]{Ok(Expression::FunctionCall(Function::Adjust, vec![a, b]))}
                #[cfg(not(feature = "sep-0002"))]{Err("The ADJUST function is only available in SPARQL-dev SEP 0002")}
            }

        rule RegexExpression() -> Expression =
            i("REGEX") _ "(" _ a:Expression() _ "," _ b:Expression() _ c:("," _ e:Expression() _ { e })? ")" {
                match c {
                    Some(c) => Expression::FunctionCall(Function::Regex, vec![a, b, c]),
                    None => Expression::FunctionCall(Function::Regex, vec![a, b]),
                }
            }


        rule SubstringExpression() -> Expression =
            i("SUBSTR") _ "(" _ a:Expression() _ "," _ b:Expression() _ c:("," _ e:Expression() _ { e })? ")" {
                match c {
                    Some(c) => Expression::FunctionCall(Function::SubStr, vec![a, b, c]),
                    None => Expression::FunctionCall(Function::SubStr, vec![a, b]),
                }
            }


        rule StrReplaceExpression() -> Expression =
            i("REPLACE") _ "(" _ a:Expression() _ "," _ b:Expression() _ "," _ c:Expression() _ d:("," _ e:Expression() _ { e })? ")" {
                match d {
                    Some(d) => Expression::FunctionCall(Function::Replace, vec![a, b, c, d]),
                    None => Expression::FunctionCall(Function::Replace, vec![a, b, c]),
                }
            }

        rule Exists_enter() = {
            state.placeholder_scope.push(PlaceholderScope::None);
            state.aggregate_arg_depth.push(0);
        }

        rule ExistsBody() -> GraphPattern = &['{'] Exists_enter() p:GroupGraphPattern()? {?
            state.placeholder_scope.pop();
            state.aggregate_arg_depth.pop();
            p.ok_or("Expected group graph pattern")
        }

        rule ExistsFunc() -> Expression = i("EXISTS") _ p:ExistsBody() {
            Expression::Exists(Box::new(p))
        }

        rule NotExistsFunc() -> Expression = i("NOT") _ i("EXISTS") _ p:ExistsBody() {
            Expression::Not(Box::new(Expression::Exists(Box::new(p))))
        }

        rule Aggregate_enter() = {
            if let Some(depth) = state.aggregate_arg_depth.last_mut() {
                *depth += 1;
            }
        }

        rule AggExpr() -> Expression = Aggregate_enter() e:Expression()? {?
            if let Some(depth) = state.aggregate_arg_depth.last_mut() {
                *depth -= 1;
            }
            e.ok_or("Expected aggregate expression")
        }

        rule AggFoldBody() -> AggregateExpression = Aggregate_enter() f:AggFoldBody_inner()? {?
            if let Some(depth) = state.aggregate_arg_depth.last_mut() {
                *depth -= 1;
            }
            f.ok_or("Expected FOLD arguments")
        }

        rule AggFoldBody_inner() -> AggregateExpression =
            d:Fold_distinct()? expr:Expression() _ value:Fold_value()? order:Fold_order()? {
                AggregateExpression::Fold { expr, value, distinct: d.is_some(), order: order.unwrap_or_default() }
            }

        rule KnownCustomAggregateName() -> NamedNode = name_and_synth:iri_with_synthetic_flag() {?
            let (name, is_synth) = name_and_synth;
            if state.custom_aggregate_functions.contains(&name)
                || (is_synth && state.synthetic_calls.is_inferred_aggregate(&name))
            {
                Ok(name)
            } else {
                Err("This custom function is a regular function and not an aggregate function")
            }
        }

        rule Aggregate_or_inferred() -> (AggregateExpression, Option<NamedNode>) =
            a:Aggregate() { (a, None) } /
            name_and_synth:iri_with_synthetic_flag() _ "(" _ i("DISTINCT") _ expr:AggExpr() _ ")" {?
                let (name, is_synth) = name_and_synth;
                if is_synth {
                    Ok((
                        AggregateExpression::FunctionCall { name: AggregateFunction::Custom(name.clone()), expr, distinct: true },
                        Some(name),
                    ))
                } else if state.custom_aggregate_functions.contains(&name) {
                    Ok((
                        AggregateExpression::FunctionCall { name: AggregateFunction::Custom(name), expr, distinct: true },
                        None,
                    ))
                } else {
                    Err("This custom function is a regular function and not an aggregate function")
                }
            } /
            name:KnownCustomAggregateName() _ "(" _ expr:AggExpr() _ ")" {
                (
                    AggregateExpression::FunctionCall { name: AggregateFunction::Custom(name), expr, distinct: false },
                    None,
                )
            }

        rule Aggregate() -> AggregateExpression =
            i("COUNT") _ "(" _ i("DISTINCT") _ "*" _ ")" { AggregateExpression::CountSolutions { distinct: true } } /
            i("COUNT") _ "(" _ i("DISTINCT") _ expr:AggExpr() _ ")" { AggregateExpression::FunctionCall { name: AggregateFunction::Count, expr, distinct: true } } /
            i("COUNT") _ "(" _ "*" _ ")" { AggregateExpression::CountSolutions { distinct: false } } /
            i("COUNT") _ "(" _ expr:AggExpr() _ ")" { AggregateExpression::FunctionCall { name: AggregateFunction::Count, expr, distinct: false } } /
            i("SUM") _ "(" _ i("DISTINCT") _ expr:AggExpr() _ ")" { AggregateExpression::FunctionCall { name: AggregateFunction::Sum, expr, distinct: true } } /
            i("SUM") _ "(" _ expr:AggExpr() _ ")" { AggregateExpression::FunctionCall { name: AggregateFunction::Sum, expr, distinct: false } } /
            i("MIN") _ "(" _ i("DISTINCT") _ expr:AggExpr() _ ")" { AggregateExpression::FunctionCall { name: AggregateFunction::Min, expr, distinct: true } } /
            i("MIN") _ "(" _ expr:AggExpr() _ ")" { AggregateExpression::FunctionCall { name: AggregateFunction::Min, expr, distinct: false } } /
            i("MAX") _ "(" _ i("DISTINCT") _ expr:AggExpr() _ ")" { AggregateExpression::FunctionCall { name: AggregateFunction::Max, expr, distinct: true } } /
            i("MAX") _ "(" _ expr:AggExpr() _ ")" { AggregateExpression::FunctionCall { name: AggregateFunction::Max, expr, distinct: false } } /
            i("AVG") _ "(" _ i("DISTINCT") _ expr:AggExpr() _ ")" { AggregateExpression::FunctionCall { name: AggregateFunction::Avg, expr, distinct: true } } /
            i("AVG") _ "(" _ expr:AggExpr() _ ")" { AggregateExpression::FunctionCall { name: AggregateFunction::Avg, expr, distinct: false } } /
            i("SAMPLE") _ "(" _ i("DISTINCT") _ expr:AggExpr() _ ")" { AggregateExpression::FunctionCall { name: AggregateFunction::Sample, expr, distinct: true } } /
            i("SAMPLE") _ "(" _ expr:AggExpr() _ ")" { AggregateExpression::FunctionCall { name: AggregateFunction::Sample, expr, distinct: false } } /
            i("GROUP_CONCAT") _ "(" _ i("DISTINCT") _ expr:AggExpr() _ s:(";" _ i("SEPARATOR") _ "=" _ s:String() _ { s })? ")" { AggregateExpression::FunctionCall { name: AggregateFunction::GroupConcat { separator: s }, expr, distinct: true } } /
            i("GROUP_CONCAT") _ "(" _ expr:AggExpr() _ s:(";" _ i("SEPARATOR") _ "=" _ s:String() _ { s })? ")" { AggregateExpression::FunctionCall { name: AggregateFunction::GroupConcat { separator: s }, expr, distinct: false } } /
            arq() i("FOLD") _ "(" _ f:AggFoldBody() ")" { f } /
            arq() name:ArqAggregateKeyword() _ "(" _ i("DISTINCT") _ expr:AggExpr() _ ")" { AggregateExpression::FunctionCall { name: AggregateFunction::Custom(name), expr, distinct: true } } /
            arq() name:ArqAggregateKeyword() _ "(" _ expr:AggExpr() _ ")" { AggregateExpression::FunctionCall { name: AggregateFunction::Custom(name), expr, distinct: false } } /
            arq() i("AGG") &[' ' | '\t' | '\n' | '\r' | '#' | '<'] _ name:iri() _ "(" _ i("DISTINCT") _ expr:AggExpr() _ ")" { AggregateExpression::FunctionCall { name: AggregateFunction::Custom(name), expr, distinct: true } } /
            arq() i("AGG") &[' ' | '\t' | '\n' | '\r' | '#' | '<'] _ name:iri() _ "(" _ expr:AggExpr() _ ")" { AggregateExpression::FunctionCall { name: AggregateFunction::Custom(name), expr, distinct: false } }

        // the parts of ARQ's FOLD after `FOLD(`
        rule Fold_distinct() -> () = i("DISTINCT") _ { }
        rule Fold_value() -> Expression = "," _ e:Expression() _ { e }
        rule Fold_order() -> Vec<OrderExpression> = i("ORDER") _ i("BY") _ c:OrderClause_item()+ { c }

        // ARQ's aggregate keywords (ARQ_AGGREGATE_KEYWORDS), as the IRIs they stand for
        rule ArqAggregateKeyword() -> NamedNode =
            i("MEDIAN") { arq_aggregate("median") } /
            i("MODE") { arq_aggregate("mode") } /
            i("STDEV_SAMP") { arq_aggregate("stdev_samp") } /
            i("STDEV_POP") { arq_aggregate("stdev_pop") } /
            i("STDEV") { arq_aggregate("stdev") } /
            i("VARIANCE") { arq_aggregate("variance") } /
            i("VAR_SAMP") { arq_aggregate("var_samp") } /
            i("VAR_POP") { arq_aggregate("var_pop") }

        rule iriOrFunction() -> Expression =
            i_and_synth: iri_with_synthetic_flag() _ a: ArgList() {?
                let (i, is_synth) = i_and_synth;
                state.build_custom_function_call(i, is_synth, a)
            } /
            // an IRI followed by `(` that is no argument list is a custom aggregate with
            // DISTINCT (`Aggregate`), not an IRI
            i: iri() !(_ "(") { i.into() }

        rule RDFLiteral() -> Literal =
            value:String() _ "^^" _ datatype:iri() { Literal::new_typed_literal(value, datatype) } /
            value:String() _ language_and_direction:LANGDIR() {?
                let (language, direction) = language_and_direction;
                #[cfg(feature = "sparql-12")]
                if let Some(is_ltr) = direction {
                    return Ok(Literal::new_directional_language_tagged_literal_unchecked(value, language.into_inner(), if is_ltr { oxrdf::BaseDirection::Ltr } else { oxrdf::BaseDirection::Rtl }))
                }
                #[cfg(not(feature = "sparql-12"))]
                if direction.is_some() {
                    return Err("Literal base directions are only supported in SPARQL 1.2")
                }
                Ok(Literal::new_language_tagged_literal_unchecked(value, language.into_inner()))
            } /
            value:String() { Literal::new_simple_literal(value) }

        rule NumericLiteral() -> Literal  = NumericLiteralUnsigned() / NumericLiteralPositive() / NumericLiteralNegative()

        rule NumericLiteralUnsigned() -> Literal =
            d:$(DOUBLE()) { Literal::new_typed_literal(d, xsd::DOUBLE) } /
            d:$(DECIMAL()) { Literal::new_typed_literal(d, xsd::DECIMAL) } /
            i:$(INTEGER()) { Literal::new_typed_literal(i, xsd::INTEGER) }

        rule NumericLiteralPositive() -> Literal =
            d:$(DOUBLE_POSITIVE()) { Literal::new_typed_literal(d, xsd::DOUBLE) } /
            d:$(DECIMAL_POSITIVE()) { Literal::new_typed_literal(d, xsd::DECIMAL) } /
            i:$(INTEGER_POSITIVE()) { Literal::new_typed_literal(i, xsd::INTEGER) }


        rule NumericLiteralNegative() -> Literal =
            d:$(DOUBLE_NEGATIVE()) { Literal::new_typed_literal(d, xsd::DOUBLE) } /
            d:$(DECIMAL_NEGATIVE()) { Literal::new_typed_literal(d, xsd::DECIMAL) } /
            i:$(INTEGER_NEGATIVE()) { Literal::new_typed_literal(i, xsd::INTEGER) }

        // 'true' and 'false' are keywords, and SPARQL 1.1 §19.8 matches keywords
        // case-insensitively (only 'a' is case-sensitive): `TRUE` is xsd:boolean "true".
        rule BooleanLiteral() -> Literal =
            i("true") { Literal::new_typed_literal("true", xsd::BOOLEAN) } /
            i("false") { Literal::new_typed_literal("false", xsd::BOOLEAN) }

        rule String() -> String = STRING_LITERAL_LONG1() / STRING_LITERAL_LONG2() / STRING_LITERAL1() / STRING_LITERAL2()

        rule iri() -> NamedNode = i:(IRIREF() / PrefixedName()) {
            NamedNode::from(i)
        }

        rule iri_with_synthetic_flag() -> (NamedNode, bool) =
            i:IRIREF() { (NamedNode::from(i), false) } /
            i:PrefixedName_with_flag() { (NamedNode::from(i.0), i.1) }

        rule PrefixedName() -> Iri<String> = i:PrefixedName_with_flag() { i.0 }

        rule PrefixedName_with_flag() -> (Iri<String>, bool) = PNAME_LN_with_flag() /
            ns:PNAME_NS() {?
                let Some(len) = state.prefixes.get(ns).map(String::len) else {
                    return Err("Prefix not found");
                };
                state.charge_resolved_iri_bytes(len)?;
                let iri = state.prefixes.get(ns).expect("checked").clone();
                let is_synth = state.placeholder_prefixes.contains(ns);
                Iri::parse(iri)
                    .map(|i| (i, is_synth))
                    .map_err(|_| "prefix IRI parsing failed")
            }

        rule BlankNode() -> BlankNode = id:BLANK_NODE_LABEL() {?
            let node = BlankNode::new_unchecked(id);
            if state.used_bnodes.contains(&node) {
                Err("Already used blank node id")
            } else {
                state.currently_used_bnodes.insert(node.clone());
                Ok(node)
            }
        } / ANON() { BlankNode::default() }

        rule IRIREF() -> Iri<String> = "<" i:$((!['>'] [_])*) ">" {?
            state.parse_iriref(i)
        }

        // The IRIREF terminal exactly as the grammar defines it (SPARQL 1.1 §19.8 [139]:
        // '<' ([^<>"{}|^`\]-[#x00-#x20])* '>', plus the \u / \U escapes §19.2 allows
        // anywhere). Used only as a lookahead to apply the longest-match rule.
        rule IRIREF_token() = "<" (UCHAR() / !['<' | '>' | '"' | '{' | '}' | '|' | '^' | '`' | '\\' | '\u{00}'..='\u{20}'] [_])* ">"

        rule PNAME_NS() -> &'input str = ns:$(PN_PREFIX()?) ":" {
            ns
        }

        rule PNAME_LN() -> Iri<String> = i:PNAME_LN_with_flag() { i.0 }

        rule PNAME_LN_with_flag() -> (Iri<String>, bool) = ns:PNAME_NS() local:$(PN_LOCAL()) {?
            let Some(base_len) = state.prefixes.get(ns).map(String::len) else {
                return Err("Prefix not found");
            };
            let unescaped_local_len =
                local.len() - local.as_bytes().iter().filter(|&&b| b == b'\\').count();
            let resolved_len = base_len.saturating_add(unescaped_local_len);
            state.charge_resolved_iri_bytes(resolved_len)?;
            let base = state.prefixes.get(ns).expect("checked");
            let is_synth = state.placeholder_prefixes.contains(ns);
            let mut iri = String::with_capacity(resolved_len);
            iri.push_str(base);
            for chunk in local.split('\\') { // We remove \
                iri.push_str(chunk);
            }
            Iri::parse(iri)
                .map(|i| (i, is_synth))
                .map_err(|_| "IRI parsing failed")
        }

        rule BLANK_NODE_LABEL() -> &'input str = "_:" b:$((['0'..='9'] / PN_CHARS_U()) PN_CHARS()* ("."+ PN_CHARS()+)*) {
            b
        }

        rule VAR1() -> &'input str = "?" v:$(VARNAME()) { v }

        rule VAR2() -> &'input str = "$" v:$(VARNAME()) { v }

        rule LANGDIR() -> (LanguageTag<String>, Option<bool>) = "@" l:$(['a' ..= 'z' | 'A' ..= 'Z']+ ("-" ['a' ..= 'z' | 'A' ..= 'Z' | '0' ..= '9']+)*) d:$("--" ['a' ..= 'z' | 'A' ..= 'Z']+)? {?
            Ok((
                LanguageTag::parse(l.to_ascii_lowercase()).map_err(|_| "language tag parsing failed")?,
                d.map(|d| match d {
                    "--ltr" => Ok(true),
                    "--rtl" => Ok(false),
                    _ => Err("the only base directions allowed are 'rtl' and 'ltr'")
                }).transpose()?
            ))
        }

        rule INTEGER() = ['0'..='9']+

        rule DECIMAL() = ['0'..='9']* "." ['0'..='9']+

        rule DOUBLE() = (['0'..='9']+ "." ['0'..='9']* / "." ['0'..='9']+ / ['0'..='9']+) EXPONENT()

        rule INTEGER_POSITIVE() = "+" _ INTEGER()

        rule DECIMAL_POSITIVE() = "+" _ DECIMAL()

        rule DOUBLE_POSITIVE() = "+" _ DOUBLE()

        rule INTEGER_NEGATIVE() = "-" _ INTEGER()

        rule DECIMAL_NEGATIVE() = "-" _ DECIMAL()

        rule DOUBLE_NEGATIVE() = "-" _ DOUBLE()

        rule EXPONENT() = ['e' | 'E'] ['+' | '-']? ['0'..='9']+

        rule STRING_LITERAL1() -> String = "'" l:$((STRING_LITERAL1_simple_char() / ECHAR() / UCHAR())*) "'" {?
             unescape_string(l)
        }
        rule STRING_LITERAL1_simple_char() = !['\u{27}' | '\u{5C}' | '\u{0A}' | '\u{0D}'] [_]


        rule STRING_LITERAL2() -> String = "\"" l:$((STRING_LITERAL2_simple_char() / ECHAR() / UCHAR())*) "\"" {?
             unescape_string(l)
        }
        rule STRING_LITERAL2_simple_char() = !['\u{22}' | '\u{5C}' | '\u{0A}' | '\u{0D}'] [_]

        rule STRING_LITERAL_LONG1() -> String = "'''" l:$(STRING_LITERAL_LONG1_inner()*) "'''" {?
             unescape_string(l)
        }
        rule STRING_LITERAL_LONG1_inner() = ("''" / "'")? (STRING_LITERAL_LONG1_simple_char() / ECHAR() / UCHAR())
        rule STRING_LITERAL_LONG1_simple_char() = !['\'' | '\\'] [_]

        rule STRING_LITERAL_LONG2() -> String = "\"\"\"" l:$(STRING_LITERAL_LONG2_inner()*) "\"\"\"" {?
             unescape_string(l)
        }
        rule STRING_LITERAL_LONG2_inner() = ("\"\"" / "\"")? (STRING_LITERAL_LONG2_simple_char() / ECHAR() / UCHAR())
        rule STRING_LITERAL_LONG2_simple_char() = !['"' | '\\'] [_]

        rule UCHAR() = "\\u" HEX() HEX() HEX() HEX() / "\\U" HEX() HEX() HEX() HEX() HEX() HEX() HEX() HEX()

        rule ECHAR() = "\\" ['t' | 'b' | 'n' | 'r' | 'f' | '"' |'\'' | '\\']

        rule NIL() = "(" WS()* ")"

        rule WS() = quiet! { ['\u{20}' | '\u{09}' | '\u{0D}' | '\u{0A}'] }

        rule ANON() = "[" WS()* "]"

        rule PN_CHARS_BASE() = ['A' ..= 'Z' | 'a' ..= 'z' | '\u{00C0}'..='\u{00D6}' | '\u{00D8}'..='\u{00F6}' | '\u{00F8}'..='\u{02FF}' | '\u{0370}'..='\u{037D}' | '\u{037F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}' | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}' | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}']

        rule PN_CHARS_U() = ['_'] / PN_CHARS_BASE()

        rule VARNAME() = (['0'..='9'] / PN_CHARS_U()) (['0' ..= '9' | '\u{00B7}' | '\u{0300}'..='\u{036F}' | '\u{203F}'..='\u{2040}'] / PN_CHARS_U())*

        rule PN_CHARS() = ['-' | '0' ..= '9' | '\u{00B7}' | '\u{0300}'..='\u{036F}' | '\u{203F}'..='\u{2040}'] / PN_CHARS_U()

        rule PN_PREFIX() = PN_CHARS_BASE() PN_CHARS()* ("."+ PN_CHARS()+)*

        rule PN_LOCAL() = (PN_CHARS_U() / [':' | '0'..='9'] / PLX()) (PN_CHARS() / [':'] / PLX())* (['.']+ (PN_CHARS() / [':'] / PLX())+)?

        rule PLX() = PERCENT() / PN_LOCAL_ESC()

        rule PERCENT() = ['%'] HEX() HEX()

        rule HEX() = ['0' ..= '9' | 'A' ..= 'F' | 'a' ..= 'f']

        rule PN_LOCAL_ESC() = ['\\'] ['_' | '~' | '.' | '-' | '!' | '$' | '&' | '\'' | '(' | ')' | '*' | '+' | ',' | ';' | '=' | '/' | '?' | '#' | '@' | '%']

        //space
        rule _() = quiet! { ([' ' | '\t' | '\n' | '\r'] / comment())* }

        //comment
        rule comment() = quiet! { ['#'] (!['\r' | '\n'] [_])* }

        // Jena ARQ's syntax extensions are accepted (`SparqlParser::with_arq_syntax`):
        // the first element of every ARQ-only rule
        rule arq() = {? if state.arq { Ok(()) } else { Err("ARQ syntax") } }

        rule i(literal: &'static str) = input: $([_]*<{literal.len()}>) {?
            if input.eq_ignore_ascii_case(literal) {
                Ok(())
            } else {
                Err(literal)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_iri_budget_enforced_in_parser_state() {
        let long_base = format!("https://example.org/{}/", "a".repeat(50_000));
        let budget = 250_000;

        // 1. Short-prefix shadowing on compact FILTER (`FILTERp` declared short, `p` long).
        let mut filter_shadow = format!(
            "BASE <{long_base}>\nPREFIX FILTERp: <http://s/>\nPREFIX p: <ns/>\nSELECT ?x WHERE {{ ?s <urn:in> ?x .\n"
        );
        for i in 0..8 {
            filter_shadow.push_str(&format!("  FILTERp:f{i}(?x)\n"));
        }
        filter_shadow.push('}');
        let err = SparqlParser::new()
            .with_max_resolved_iri_bytes(budget)
            .parse_query(&filter_shadow)
            .unwrap_err();
        assert!(err.is_resolved_iri_limit_exceeded(), "{err}");

        // 2. Short-prefix shadowing on compact GRAPH (`GRAPHp` declared short, `p` long).
        let mut graph_shadow = format!(
            "BASE <{long_base}>\nPREFIX GRAPHp: <http://s/>\nPREFIX p: <ns/>\nSELECT ?x WHERE {{\n"
        );
        for i in 0..8 {
            graph_shadow.push_str(&format!("  GRAPHp:g{i} {{ ?s <urn:in> ?x }}\n"));
        }
        graph_shadow.push('}');
        let err = SparqlParser::new()
            .with_max_resolved_iri_bytes(budget)
            .parse_query(&graph_shadow)
            .unwrap_err();
        assert!(err.is_resolved_iri_limit_exceeded(), "{err}");

        // 3. Case-sensitive `a` shorthand (`?this ap:C{i}` in both CONSTRUCT and WHERE).
        let mut a_construct = format!("BASE <{long_base}>\nPREFIX p: <ns/>\nCONSTRUCT {{\n");
        let mut a_where = format!("BASE <{long_base}>\nPREFIX p: <ns/>\nSELECT ?this WHERE {{\n");
        for i in 0..8 {
            a_construct.push_str(&format!("  ?this ap:C{i} .\n"));
            a_where.push_str(&format!("  ?this ap:C{i} .\n"));
        }
        a_construct.push_str("} WHERE { ?this <urn:in> ?x }");
        a_where.push('}');
        for (label, q) in [("CONSTRUCT ap:", a_construct), ("WHERE ap:", a_where)] {
            let err = SparqlParser::new()
                .with_max_resolved_iri_bytes(budget)
                .parse_query(&q)
                .unwrap_err();
            assert!(err.is_resolved_iri_limit_exceeded(), "{label}: {err}");
        }

        // 3. Zero-whitespace VALUES with >3 chained booleans and UNDEF.
        let mut values_glued = format!(
            "BASE <{long_base}>\nPREFIX p: <ns/>\nSELECT ?x WHERE {{ VALUES ?x {{"
        );
        for i in 0..8 {
            values_glued.push_str(&format!(" truetruetruetruep:a{i} UNDEFp:b{i}"));
        }
        values_glued.push_str(" } }");
        let err = SparqlParser::new()
            .with_max_resolved_iri_bytes(budget)
            .parse_query(&values_glued)
            .unwrap_err();
        assert!(err.is_resolved_iri_limit_exceeded(), "{err}");

        // 4. parse_update enforces the same budget.
        let mut update = format!("BASE <{long_base}>\nPREFIX p: <ns/>\nINSERT DATA {{\n");
        for i in 0..8 {
            update.push_str(&format!("  <urn:s> p:k{i} <urn:o> .\n"));
        }
        update.push('}');
        let err = SparqlParser::new()
            .with_max_resolved_iri_bytes(budget)
            .parse_update(&update)
            .unwrap_err();
        assert!(err.is_resolved_iri_limit_exceeded(), "{err}");

        // 5. Absolute and root-relative IRIs under a long BASE do NOT charge the long base path.
        let mut ok_query = format!("BASE <{long_base}>\nSELECT * WHERE {{\n");
        for i in 0..32 {
            ok_query.push_str(&format!("  ?s <urn:p{i}> </short{i}> .\n"));
        }
        ok_query.push('}');
        assert!(
            SparqlParser::new()
                .with_max_resolved_iri_bytes(budget)
                .parse_query(&ok_query)
                .is_ok()
        );
    }

    #[test]
    fn inferred_synthetic_aggregate_nesting_in_modifiers() {
        let parse = |q: &str| {
            SparqlParser::new()
                .with_placeholder_prefix("p", "urn:sparkles:test:p0/")
                .unwrap()
                .parse_query(q)
        };

        for bad in [
            "SELECT (p:f(DISTINCT ?v) AS ?a) WHERE { ?s <urn:p> ?v } ORDER BY p:f(COUNT(?v))",
            "SELECT (p:f(DISTINCT ?v) AS ?a) WHERE { ?s <urn:p> ?v } HAVING p:f(COUNT(?v))",
            "SELECT (COUNT(?v) AS ?n) WHERE { ?s <urn:p> ?v } HAVING(p:f(COUNT(?v))) ORDER BY p:f(DISTINCT ?v)",
            "SELECT (COUNT(?v) AS ?n) WHERE { ?s <urn:p> ?v } HAVING p:f(COUNT(?v)) ORDER BY p:f(DISTINCT ?v)",
            "SELECT (COUNT(?v) AS ?n) WHERE { ?s <urn:p> ?v } HAVING (p:f(COUNT(?v)) > 0 && p:f(DISTINCT ?v) > 0)",
            "SELECT (COUNT(?v) AS ?n) WHERE { ?s <urn:p> ?v } ORDER BY p:f(COUNT(?v)) p:f(DISTINCT ?v)",
            "SELECT (p:f(COUNT(?v)) AS ?x) WHERE { ?s <urn:p> ?v } ORDER BY p:f(DISTINCT ?v)",
        ] {
            assert!(parse(bad).is_err(), "must reject nested aggregate: {bad}");
        }

        for ok in [
            "SELECT (p:f(DISTINCT ?v) AS ?a) WHERE { ?s <urn:p> ?v } ORDER BY p:f(?v)",
            "SELECT (p:f(DISTINCT ?v) AS ?a) WHERE { ?s <urn:p> ?v } HAVING p:f(?v)",
            "SELECT (COUNT(?v) AS ?n) WHERE { ?s <urn:p> ?v } HAVING p:f(?v) ORDER BY p:f(DISTINCT ?v)",
            "SELECT (p:f(DISTINCT ?v) AS ?a) WHERE { ?s <urn:p> ?v } ORDER BY <urn:sparkles:test:p0/f>(COUNT(?v))",
            "SELECT (COUNT(?v) AS ?n) WHERE { ?s <urn:p> ?v } HAVING (<urn:sparkles:test:p0/f>(COUNT(?v)) > 0) ORDER BY p:f(DISTINCT ?v)",
            "SELECT (COUNT(?v) AS ?n) WHERE { ?s <urn:p> ?v } HAVING (p:f(COUNT(?v)) > 0)",
        ] {
            assert!(parse(ok).is_ok(), "must accept valid query: {ok}");
        }
    }

    #[test]
    fn round8_relative_iri_bounds_and_escaped_iriref_admission() {
        // R8-1: O(1) cached component bounds across authority/path/query/fragment shapes.
        let base_with_query = Iri::parse(format!(
            "https://example.org/dir/page?{}",
            "q".repeat(100_000)
        ))
        .unwrap();
        let path_end = "https://example.org/dir/page".len();
        let query_end = base_with_query.as_str().len();
        assert_eq!(
            relative_iri_resolution_bound(&base_with_query, IriRefHead::from_plain("item")),
            path_end + 1 + "item".len()
        );
        assert_eq!(
            relative_iri_resolution_bound(&base_with_query, IriRefHead::from_plain("?k=1")),
            path_end + "?k=1".len()
        );
        assert_eq!(
            relative_iri_resolution_bound(&base_with_query, IriRefHead::from_plain("#frag")),
            query_end + "#frag".len()
        );
        assert_eq!(
            relative_iri_resolution_bound(&base_with_query, IriRefHead::from_plain("")),
            query_end
        );
        assert_eq!(
            relative_iri_resolution_bound(&base_with_query, IriRefHead::from_plain("/root")),
            "https://example.org".len() + "/root".len()
        );
        assert_eq!(
            relative_iri_resolution_bound(&base_with_query, IriRefHead::from_plain("//host/p")),
            "https:".len() + "//host/p".len()
        );

        // Edge cases: empty authority (`file:///a`) and empty query (`https://ex/p?`).
        let empty_auth = Iri::parse("file:///a".to_string()).unwrap();
        assert_eq!(
            relative_iri_resolution_bound(&empty_auth, IriRefHead::from_plain("/b")),
            "file://".len() + "/b".len()
        );
        let empty_query = Iri::parse("https://ex/p?".to_string()).unwrap();
        assert_eq!(
            relative_iri_resolution_bound(&empty_query, IriRefHead::from_plain("#f")),
            "https://ex/p?".len() + "#f".len()
        );

        // Long query component in BASE: many path-relative and query-relative IRIs stay under
        // budget while fragment-relative IRIs (which retain the query) exceed it.
        let long_query_base = base_with_query.as_str();
        let budget = long_query_base.len() + 10_000;
        let mut path_rel_query = format!("BASE <{long_query_base}>\nSELECT * WHERE {{\n");
        for i in 0..64 {
            path_rel_query.push_str(&format!("  ?s <item{i}> <?k={i}> .\n"));
        }
        path_rel_query.push('}');
        assert!(
            SparqlParser::new()
                .with_max_resolved_iri_bytes(budget)
                .parse_query(&path_rel_query)
                .is_ok()
        );

        let frag_rel_query =
            format!("BASE <{long_query_base}>\nSELECT * WHERE {{ ?s <#f1> <#f2> . }}");
        let err = SparqlParser::new()
            .with_max_resolved_iri_bytes(budget)
            .parse_query(&frag_rel_query)
            .unwrap_err();
        assert!(err.is_resolved_iri_limit_exceeded(), "{err}");

        // R8-6: Escaped absolute and relative IRIREFs measure decoded bytes before admission.
        let escaped_abs = r"SELECT * WHERE { ?s <https://example.org/\u0061\u0062\u00e9> ?o }";
        // Decoded IRI is "https://example.org/abé" (20 + 2 + 2 = 24 bytes).
        let decoded_len = "https://example.org/ab\u{00e9}".len();
        assert!(
            SparqlParser::new()
                .with_max_resolved_iri_bytes(decoded_len)
                .parse_query(escaped_abs)
                .is_ok()
        );
        let err = SparqlParser::new()
            .with_max_resolved_iri_bytes(decoded_len - 1)
            .parse_query(escaped_abs)
            .unwrap_err();
        assert!(err.is_resolved_iri_limit_exceeded(), "{err}");

        // Escaped scheme (`\u0068ttps://...`) under a long BASE is recognized as absolute
        // without charging the long BASE path.
        let long_path_base = format!("https://example.org/{}/", "a".repeat(20_000));
        let escaped_scheme = format!(
            r"BASE <{long_path_base}> SELECT * WHERE {{ ?s <\u0068ttps://s.example/\u0061> ?o }}"
        );
        let tight_budget = long_path_base.len() + "https://s.example/a".len();
        assert!(
            SparqlParser::new()
                .with_max_resolved_iri_bytes(tight_budget)
                .parse_query(&escaped_scheme)
                .is_ok()
        );
        let err = SparqlParser::new()
            .with_max_resolved_iri_bytes(tight_budget - 1)
            .parse_query(&escaped_scheme)
            .unwrap_err();
        assert!(err.is_resolved_iri_limit_exceeded(), "{err}");

        // Escaped root-relative (`\u002fshort`) under a long BASE path uses authority bound.
        let escaped_root =
            format!(r"BASE <{long_path_base}> SELECT * WHERE {{ ?s <\u002fshort> ?o }}");
        let root_budget = long_path_base.len() + "https://example.org/short".len();
        assert!(
            SparqlParser::new()
                .with_max_resolved_iri_bytes(root_budget)
                .parse_query(&escaped_root)
                .is_ok()
        );
        let err = SparqlParser::new()
            .with_max_resolved_iri_bytes(root_budget - 1)
            .parse_query(&escaped_root)
            .unwrap_err();
        assert!(err.is_resolved_iri_limit_exceeded(), "{err}");
    }

    #[test]
    fn round8_grouped_projection_multi_arg_and_sibling_subselect_inference() {
        let parse = |q: &str| {
            SparqlParser::new()
                .with_placeholder_prefix("p", "urn:sparkles:test:p0/")
                .unwrap()
                .parse_query(q)
        };

        // R8-3: Multi-argument and zero-argument synthetic scalar calls in grouped projections
        // preserve argument-based variable binding checks.
        for ok in [
            "SELECT (p:f(1, 2) AS ?x) (COUNT(?v) AS ?n) WHERE { ?s <urn:p> ?v }",
            "SELECT (p:f() AS ?x) (COUNT(?v) AS ?n) WHERE { ?s <urn:p> ?v }",
            "SELECT (p:f(?g, 1) AS ?x) (COUNT(?v) AS ?n) WHERE { ?s <urn:g> ?g ; <urn:p> ?v } GROUP BY ?g",
            "SELECT (p:f(COUNT(?v), 1) AS ?x) WHERE { ?s <urn:p> ?v }",
            "SELECT (p:g(p:f(?v), 1) AS ?x) (COUNT(?v) AS ?n) WHERE { ?s <urn:p> ?v }",
        ] {
            assert!(parse(ok).is_ok(), "must accept grouped scalar call: {ok}");
        }
        for bad in [
            "SELECT (p:f(?v, 1) AS ?x) (COUNT(?v) AS ?n) WHERE { ?s <urn:p> ?v }",
            "SELECT (p:g(p:h(?v, 2), 1) AS ?x) (COUNT(?v) AS ?n) WHERE { ?s <urn:p> ?v }",
        ] {
            assert!(
                parse(bad).is_err(),
                "must reject unbound arg in grouped scalar call: {bad}"
            );
        }

        // R8-4: Sibling subselects inferring an aggregate in either order reject nested calls
        // in ORDER BY, HAVING, and SELECT projections, including self- and cross-nesting.
        for bad in [
            // Earlier sibling ORDER BY wraps COUNT; later sibling infers p:f.
            "SELECT ?n WHERE { \
                { SELECT (COUNT(?v) AS ?n) WHERE { ?s <urn:p> ?v } ORDER BY p:f(COUNT(?v)) } \
                { SELECT (p:f(DISTINCT ?w) AS ?m) WHERE { ?s <urn:q> ?w } } \
            }",
            // Reverse sibling order.
            "SELECT ?n WHERE { \
                { SELECT (p:f(DISTINCT ?w) AS ?m) WHERE { ?s <urn:q> ?w } } \
                { SELECT (COUNT(?v) AS ?n) WHERE { ?s <urn:p> ?v } ORDER BY p:f(COUNT(?v)) } \
            }",
            // Earlier sibling HAVING wraps COUNT; later sibling infers p:f.
            "SELECT ?n WHERE { \
                { SELECT (COUNT(?v) AS ?n) WHERE { ?s <urn:p> ?v } HAVING (p:f(COUNT(?v)) > 0) } \
                { SELECT (p:f(DISTINCT ?w) AS ?m) WHERE { ?s <urn:q> ?w } } \
            }",
            // Earlier sibling SELECT projection wraps COUNT; later sibling infers p:f.
            "SELECT ?n WHERE { \
                { SELECT (p:f(COUNT(?v)) AS ?n) WHERE { ?s <urn:p> ?v } } \
                { SELECT (p:f(DISTINCT ?w) AS ?m) WHERE { ?s <urn:q> ?w } } \
            }",
            // Inner subselect wraps COUNT; outer ORDER BY infers p:f.
            "SELECT (COUNT(?n) AS ?c) WHERE { \
                { SELECT (p:f(COUNT(?v)) AS ?n) WHERE { ?s <urn:p> ?v } } \
            } ORDER BY p:f(DISTINCT ?n)",
            // Earlier sibling nests p:f(p:f(?v)); later sibling infers p:f.
            "SELECT ?n WHERE { \
                { SELECT (p:f(p:f(?v)) AS ?n) WHERE { ?s <urn:p> ?v } } \
                { SELECT (p:f(DISTINCT ?w) AS ?m) WHERE { ?s <urn:q> ?w } } \
            }",
            // Earlier sibling nests p:f(p:g(?v)); later siblings infer p:f and p:g.
            "SELECT ?n WHERE { \
                { SELECT (p:f(p:g(?v)) AS ?n) WHERE { ?s <urn:p> ?v } } \
                { SELECT (p:f(DISTINCT ?w) AS ?m) WHERE { ?s <urn:q> ?w } } \
                { SELECT (p:g(DISTINCT ?z) AS ?k) WHERE { ?s <urn:r> ?z } } \
            }",
        ] {
            assert!(
                parse(bad).is_err(),
                "must reject cross-subselect nested inferred aggregate: {bad}"
            );
        }

        // Valid controls: non-nested sibling calls and explicit-IRI controls must pass.
        for ok in [
            "SELECT ?n WHERE { \
                { SELECT (COUNT(?v) AS ?n) WHERE { ?s <urn:p> ?v } ORDER BY p:f(?v) } \
                { SELECT (p:f(DISTINCT ?w) AS ?m) WHERE { ?s <urn:q> ?w } } \
            }",
            "SELECT ?n WHERE { \
                { SELECT (COUNT(?v) AS ?n) WHERE { ?s <urn:p> ?v } ORDER BY <urn:sparkles:test:p0/f>(COUNT(?v)) } \
                { SELECT (p:f(DISTINCT ?w) AS ?m) WHERE { ?s <urn:q> ?w } } \
            }",
            "SELECT ?n WHERE { \
                { SELECT (p:f(p:g(?v)) AS ?n) WHERE { ?s <urn:p> ?v } } \
                { SELECT (p:f(DISTINCT ?w) AS ?m) WHERE { ?s <urn:q> ?w } } \
            }",
        ] {
            assert!(parse(ok).is_ok(), "must accept valid sibling query: {ok}");
        }
    }

    #[test]
    fn round8_placeholder_compact_a_shorthand() {
        let parse_with_placeholders = |q: &str| {
            SparqlParser::new()
                .with_placeholder_prefix("ap", "urn:sparkles:test:p0/")
                .unwrap()
                .with_placeholder_prefix("p", "urn:sparkles:test:p1/")
                .unwrap()
                .with_placeholder_prefix("a", "urn:sparkles:test:p2/")
                .unwrap()
                .with_placeholder_prefix("", "urn:sparkles:test:p3/")
                .unwrap()
                .parse_query(q)
        };

        // Both compact `a` shorthand and real two-term `ap`/`a` predicates succeed when synthetic.
        for ok in [
            "SELECT ?s WHERE { ?s ap:C . }",
            "SELECT ?s WHERE { ?s a:C . }",
            "SELECT ?s WHERE { ?s ap:C , p:D . }",
            "SELECT ?s WHERE { ?s <urn:q> ?o ; ap:C . }",
            "SELECT ?s WHERE { [ ap:C ] <urn:q> ?s . }",
            "SELECT ?s WHERE { ?s ap:p ?o . }",
            "SELECT ?s WHERE { ?s a:p ?o . }",
            "SELECT ?s WHERE { ?s <urn:q> ap:C . }",
            "CONSTRUCT { ?s ap:C . } WHERE { ?s ap:p ?o . }",
        ] {
            assert!(
                parse_with_placeholders(ok).is_ok(),
                "must accept placeholder compact `a` / `ap` query: {ok}"
            );
        }

        // Explicitly declaring `PREFIX ap:` removes it from placeholder_prefixes, so `?s ap:C .`
        // must fail under standard SPARQL longest-match semantics.
        assert!(
            parse_with_placeholders(
                "PREFIX ap: <http://example.org/ap/> SELECT ?s WHERE { ?s ap:C . }"
            )
            .is_err()
        );
        // Bare `a` with no object must still fail.
        assert!(parse_with_placeholders("SELECT ?s WHERE { ?s a . }").is_err());
    }

    #[test]
    fn round9_synthetic_call_forest_bounded_and_boundaries() {
        let parse = |q: &str| {
            SparqlParser::new()
                .with_placeholder_prefix("p", "urn:sparkles:test:p0/")
                .unwrap()
                .parse_query(q)
        };

        // R9-1: Shallow wide projection with one long outer synthetic IRI and many distinct
        // short sibling synthetic calls stays O(occurrences) in both interned IRIs and nodes.
        let ns = "urn:sparkles:test:p0/";
        let outer_local = "f".repeat(100_000);
        let width = 2_000usize;
        let mut args = String::new();
        let mut inner_iri_bytes = 0usize;
        for i in 0..width {
            if i > 0 {
                args.push_str(", ");
            }
            let local = format!("a{i}");
            inner_iri_bytes += ns.len() + local.len();
            args.push_str("p:");
            args.push_str(&local);
            args.push_str("(?v)");
        }
        let wide_query = format!("SELECT (p:{outer_local}({args}) AS ?x) WHERE {{ ?s <urn:p> ?v }}");
        let exact_iri_bytes = ns.len() + outer_local.len() + inner_iri_bytes + "urn:p".len();
        assert!(
            SparqlParser::new()
                .with_placeholder_prefix("p", ns)
                .unwrap()
                .with_max_resolved_iri_bytes(exact_iri_bytes)
                .parse_query(&wide_query)
                .is_ok()
        );
        let err = SparqlParser::new()
            .with_placeholder_prefix("p", ns)
            .unwrap()
            .with_max_resolved_iri_bytes(exact_iri_bytes - 1)
            .parse_query(&wide_query)
            .unwrap_err();
        assert!(err.is_resolved_iri_limit_exceeded(), "{err}");

        let mut wide_state = ParserState::new(
            None,
            HashMap::from([("p".to_string(), ns.to_string())]),
            Arc::new(HashSet::new()),
        );
        wide_state.placeholder_prefixes.insert("p".to_string());
        assert!(parser::QueryUnit(&wide_query, &mut wide_state).is_ok());
        assert_eq!(wide_state.synthetic_calls.iris.len(), width + 1);
        assert_eq!(wide_state.synthetic_calls.nodes.len(), width + 1);
        assert_eq!(
            wide_state.synthetic_calls.iris.hashed_bytes.get(),
            ns.len() + outer_local.len() + inner_iri_bytes
        );

        // Inferring only the long outer synthetic call as an aggregate is allowed.
        let wide_outer_only = format!(
            "SELECT ?x WHERE {{ \
                {{ SELECT (p:{outer_local}({args}) AS ?x) WHERE {{ ?s <urn:p> ?v }} }} \
                {{ SELECT (p:{outer_local}(DISTINCT ?w) AS ?m) WHERE {{ ?s <urn:q> ?w }} }} \
            }}"
        );
        assert!(parse(&wide_outer_only).is_ok());

        // Inferring both the long outer call and one sibling inner call rejects nesting.
        let wide_both_inferred = format!(
            "SELECT ?x WHERE {{ \
                {{ SELECT (p:{outer_local}({args}) AS ?x) WHERE {{ ?s <urn:p> ?v }} }} \
                {{ SELECT (p:{outer_local}(DISTINCT ?w) AS ?m) WHERE {{ ?s <urn:q> ?w }} }} \
                {{ SELECT (p:a{last}(DISTINCT ?z) AS ?k) WHERE {{ ?s <urn:r> ?z }} }} \
            }}",
            last = width - 1
        );
        assert!(parse(&wide_both_inferred).is_err());

        // R9-1: Deep chain of distinct nested synthetic calls stays strictly O(depth) nodes
        // (not O(depth^2) pairs) and still catches transitive nesting across non-inferred calls.
        let depth = 150usize;
        let mut chain_expr = String::new();
        for i in 0..depth {
            chain_expr.push_str(&format!("p:c{i}("));
        }
        chain_expr.push_str("?v");
        for _ in 0..depth {
            chain_expr.push(')');
        }
        let chain_query = format!("SELECT ({chain_expr} AS ?x) WHERE {{ ?s <urn:p> ?v }}");
        let mut chain_state = ParserState::new(
            None,
            HashMap::from([("p".to_string(), ns.to_string())]),
            Arc::new(HashSet::new()),
        );
        chain_state.placeholder_prefixes.insert("p".to_string());
        assert!(parser::QueryUnit(&chain_query, &mut chain_state).is_ok());
        assert_eq!(chain_state.synthetic_calls.iris.len(), depth);
        assert_eq!(chain_state.synthetic_calls.nodes.len(), depth);

        let chain_outer_only = format!(
            "SELECT ?x WHERE {{ \
                {{ SELECT ({chain_expr} AS ?x) WHERE {{ ?s <urn:p> ?v }} }} \
                {{ SELECT (p:c0(DISTINCT ?w) AS ?m) WHERE {{ ?s <urn:q> ?w }} }} \
            }}"
        );
        assert!(parse(&chain_outer_only).is_ok());

        let chain_ends_inferred = format!(
            "SELECT ?x WHERE {{ \
                {{ SELECT ({chain_expr} AS ?x) WHERE {{ ?s <urn:p> ?v }} }} \
                {{ SELECT (p:c0(DISTINCT ?w) AS ?m) WHERE {{ ?s <urn:q> ?w }} }} \
                {{ SELECT (p:c{last}(DISTINCT ?z) AS ?k) WHERE {{ ?s <urn:r> ?z }} }} \
            }}",
            last = depth - 1
        );
        assert!(parse(&chain_ends_inferred).is_err());

        let chain_with_count = chain_expr.replace("?v", "COUNT(?v)");
        let chain_wraps_count = format!(
            "SELECT ?x WHERE {{ \
                {{ SELECT ({chain_with_count} AS ?x) WHERE {{ ?s <urn:p> ?v }} }} \
                {{ SELECT (p:c0(DISTINCT ?w) AS ?m) WHERE {{ ?s <urn:q> ?w }} }} \
            }}"
        );
        assert!(parse(&chain_wraps_count).is_err());

        // Subselect, EXISTS, and mixed-order inference boundaries.
        for ok in [
            "SELECT (p:f(EXISTS { SELECT (p:g(?w) AS ?m) WHERE { ?s <urn:q> ?w } }) AS ?x) \
                (p:f(DISTINCT ?v) AS ?a) (p:g(DISTINCT ?v) AS ?b) WHERE { ?s <urn:p> ?v }",
            "SELECT (COUNT(EXISTS { SELECT (p:f(?w) AS ?m) WHERE { ?s <urn:q> ?w } }) AS ?c) \
                (p:f(DISTINCT ?v) AS ?a) WHERE { ?s <urn:p> ?v }",
            "SELECT (p:f(?n) AS ?a) WHERE { \
                { SELECT (p:f(?v) AS ?n) WHERE { ?s <urn:p> ?v } } \
            } ORDER BY p:f(DISTINCT ?a)",
        ] {
            assert!(parse(ok).is_ok(), "must preserve boundary: {ok}");
        }
        for bad in [
            "SELECT (p:f(EXISTS { SELECT (p:f(COUNT(?w)) AS ?m) WHERE { ?s <urn:q> ?w } }) AS ?x) \
                (p:f(DISTINCT ?v) AS ?a) WHERE { ?s <urn:p> ?v }",
            "SELECT ?n WHERE { \
                { SELECT (p:f(DISTINCT ?w) AS ?m) WHERE { ?s <urn:q> ?w } } \
                { SELECT (p:f(p:g(?v)) AS ?n) WHERE { ?s <urn:p> ?v } } \
                { SELECT (p:g(DISTINCT ?z) AS ?k) WHERE { ?s <urn:r> ?z } } \
            }",
            "SELECT ?n WHERE { \
                { SELECT (p:g(DISTINCT ?z) AS ?k) WHERE { ?s <urn:r> ?z } } \
                { SELECT (p:f(p:g(?v)) AS ?n) WHERE { ?s <urn:p> ?v } } \
                { SELECT (p:f(DISTINCT ?w) AS ?m) WHERE { ?s <urn:q> ?w } } \
            }",
            "SELECT ?n WHERE { \
                { SELECT (p:f(p:h(p:g(?v))) AS ?n) WHERE { ?s <urn:p> ?v } } \
                { SELECT (p:f(DISTINCT ?w) AS ?m) WHERE { ?s <urn:q> ?w } } \
                { SELECT (p:g(DISTINCT ?z) AS ?k) WHERE { ?s <urn:r> ?z } } \
            }",
            "SELECT ?n WHERE { \
                { SELECT (p:f(p:h(COUNT(?v))) AS ?n) WHERE { ?s <urn:p> ?v } } \
                { SELECT (p:f(DISTINCT ?w) AS ?m) WHERE { ?s <urn:q> ?w } } \
            }",
        ] {
            assert!(parse(bad).is_err(), "must reject nested aggregate: {bad}");
        }
    }

    #[test]
    fn round10_backtracking_deduplicates_subselect_forest() {
        let ns = "urn:sparkles:test:p0/";
        let parse_with_state = |q: &str| {
            let mut state = ParserState::new(
                None,
                HashMap::from([("p".to_string(), ns.to_string())]),
                Arc::new(HashSet::new()),
            );
            state.placeholder_prefixes.insert("p".to_string());
            let res = parser::QueryUnit(q, &mut state);
            (res, state)
        };
        let parse = |q: &str| {
            SparqlParser::new()
                .with_placeholder_prefix("p", ns)
                .unwrap()
                .parse_query(q)
        };

        // R10-1: Single-layer two-argument REGEX around EXISTS { SELECT ... } retains
        // exactly two arena nodes for the two source call sites.
        let single_regex = "SELECT (REGEX(STR(EXISTS { \
            SELECT (p:f(p:g(?v)) AS ?n) WHERE { ?s <urn:p> ?v } \
        }), \"true\") AS ?x) WHERE {}";
        let (res, state) = parse_with_state(single_regex);
        assert!(res.is_ok(), "single REGEX wrapper must parse");
        assert_eq!(state.synthetic_calls.iris.len(), 2);
        assert_eq!(state.synthetic_calls.nodes.len(), 2);

        // Deeply nested two-argument REGEX wrappers parse in O(depth) time, charge each
        // resolved IRI once, and retain exactly two nodes.
        let mut nested_regex =
            "EXISTS { SELECT (p:f(p:g(?v)) AS ?n) WHERE { ?s <urn:p> ?v } }".to_string();
        for _ in 0..20 {
            nested_regex = format!("REGEX(STR({nested_regex}), \"true\")");
        }
        let nested_regex_query = format!("SELECT ({nested_regex} AS ?x) WHERE {{}}");
        let exact_bytes = (ns.len() + 1) * 2 + "urn:p".len();
        assert!(
            SparqlParser::new()
                .with_placeholder_prefix("p", ns)
                .unwrap()
                .with_max_resolved_iri_bytes(exact_bytes)
                .parse_query(&nested_regex_query)
                .is_ok()
        );
        let (res, state) = parse_with_state(&nested_regex_query);
        assert!(res.is_ok(), "nested REGEX wrappers must parse");
        assert_eq!(state.synthetic_calls.iris.len(), 2);
        assert_eq!(state.synthetic_calls.nodes.len(), 2);

        // Nested inferred aggregates inside wrapped REGEX subselects are still rejected.
        let nested_regex_bad = format!(
            "SELECT ({nested_regex} AS ?x) WHERE {{ \
                {{ SELECT (p:f(DISTINCT ?w) AS ?a) WHERE {{ ?s <urn:q> ?w }} }} \
                {{ SELECT (p:g(DISTINCT ?z) AS ?b) WHERE {{ ?s <urn:r> ?z }} }} \
            }}"
        );
        assert!(parse(&nested_regex_bad).is_err());

        // Left-factored SUBSTR and REPLACE wrappers also retain exactly two nodes.
        let mut nested_substr =
            "EXISTS { SELECT (p:f(p:g(?v)) AS ?n) WHERE { ?s <urn:p> ?v } }".to_string();
        let mut nested_replace = nested_substr.clone();
        for _ in 0..16 {
            nested_substr = format!("SUBSTR(STR({nested_substr}), 1)");
            nested_replace = format!("REPLACE(STR({nested_replace}), \"a\", \"b\")");
        }
        for q in [
            format!("SELECT ({nested_substr} AS ?x) WHERE {{}}"),
            format!("SELECT ({nested_replace} AS ?x) WHERE {{}}"),
        ] {
            let (res, state) = parse_with_state(&q);
            assert!(res.is_ok(), "wrapped query must parse: {q}");
            assert_eq!(state.synthetic_calls.iris.len(), 2);
            assert_eq!(state.synthetic_calls.nodes.len(), 2);
        }

        // Genuine PEG backtracking through PrimaryExpression (iriOrFunction -> BuiltInCall
        // for an already-inferred aggregate wrapping nested EXISTS subselects) still
        // deduplicates committed subselect forests to O(AST) nodes.
        let mut bt_expr =
            "EXISTS { SELECT (p:f(p:g(?v)) AS ?n) WHERE { ?s <urn:p> ?v } }".to_string();
        for i in 0..5 {
            bt_expr = format!("EXISTS {{ SELECT (p:h({bt_expr}) AS ?x{i}) WHERE {{}} }}");
        }
        let bt_query = format!(
            "SELECT (p:h(DISTINCT ?a) AS ?d) (p:h({bt_expr}) AS ?x) WHERE {{ ?s <urn:q> ?a }}"
        );
        let (res, state) = parse_with_state(&bt_query);
        assert!(res.is_ok(), "backtracked aggregate query must parse");
        assert_eq!(state.synthetic_calls.iris.len(), 3);
        assert_eq!(state.synthetic_calls.nodes.len(), 2);

        let bt_bad = format!(
            "SELECT (p:h(DISTINCT ?a) AS ?d) (p:h({bt_expr}) AS ?x) \
                (p:f(DISTINCT ?a) AS ?af) (p:g(DISTINCT ?a) AS ?ag) WHERE {{ ?s <urn:q> ?a }}"
        );
        assert!(parse(&bt_bad).is_err());

        // Failed build_select rolls back any nodes appended before the error.
        let dup_select_err = "SELECT (p:f(?v) AS ?a) (1 AS ?a) WHERE { ?s <urn:p> ?v }";
        let (res, state) = parse_with_state(dup_select_err);
        assert!(res.is_err());
        assert_eq!(state.synthetic_calls.nodes.len(), 0);
    }
}
