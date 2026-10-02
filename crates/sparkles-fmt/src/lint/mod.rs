//! `sparkles lint`: rules over the lossless syntax tree of SPARQL queries and updates and
//! of Turtle and TriG documents (spec X03). A document is lexed and checked by the
//! reference parser first. A syntax error is the only diagnostic then, except where a
//! rule explains it better (a `SELECT *` with `GROUP BY`, an IRI with a space). A
//! document that parses gets the formatter's syntax tree, and every rule runs over it.
//!
//! Each [`Diagnostic`] names its rule, has a severity and a byte range in the input, and
//! may carry a [`Fix`]. [`fix`] applies the fixes of the rules marked safe in [`RULES`]
//! and keeps the result only when it parses to the same SPARQL algebra, or to an
//! isomorphic graph or dataset, as the input.

mod prefixes;
mod sparql;
mod terms;

use crate::lex::{LexMode, Token, TokenKind, lex};
use crate::sparql::Unit;
use crate::tree::{Element, NodeId, TokenId, Tree};
use crate::{FormatError, Language};
use std::collections::BTreeMap;
use std::time::Instant;

/// How much a diagnostic matters. The names are those of the configuration and of
/// LSP's `DiagnosticSeverity`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

impl Severity {
    /// `error`, `warning`, `info` or `hint`.
    pub fn name(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Hint => "hint",
        }
    }

    /// A severity by name, or `None` for `off` (`Err` for anything else).
    pub fn parse(s: &str) -> Result<Option<Severity>, String> {
        Ok(Some(match s.trim().to_ascii_lowercase().as_str() {
            "error" => Severity::Error,
            "warning" | "warn" => Severity::Warning,
            "info" | "information" => Severity::Info,
            "hint" => Severity::Hint,
            "off" | "none" => return Ok(None),
            other => {
                return Err(format!(
                    "'{other}' is not a severity: error, warning, info, hint or off"
                ));
            }
        }))
    }
}

/// A lint rule.
#[derive(Clone, Copy, Debug)]
pub struct Rule {
    /// the kebab-case name of the configuration and of [`Diagnostic::rule`]
    pub id: &'static str,
    pub sparql: bool,
    pub turtle: bool,
    pub default: Severity,
    /// whether [`fix`] applies its fixes (they never change what the document means)
    pub safe_fix: bool,
    /// one sentence for `--list-rules` and the docs
    pub summary: &'static str,
}

/// Every rule, in the order of the docs. `syntax` stands for the reference parser's
/// errors; it is always on.
pub const RULES: &[Rule] = &[
    Rule {
        id: "syntax",
        sparql: true,
        turtle: true,
        default: Severity::Error,
        safe_fix: false,
        summary: "The document does not parse.",
    },
    Rule {
        id: "undefined-prefix",
        sparql: true,
        turtle: true,
        default: Severity::Error,
        safe_fix: false,
        summary: "A prefixed name uses a prefix that is not declared before it.",
    },
    Rule {
        id: "unused-prefix",
        sparql: true,
        turtle: true,
        default: Severity::Warning,
        safe_fix: true,
        summary: "A prefix declaration that no prefixed name uses. The fix removes it.",
    },
    Rule {
        id: "unused-variable",
        sparql: true,
        turtle: false,
        default: Severity::Warning,
        safe_fix: false,
        summary: "A variable bound by BIND or VALUES that nothing reads. Names starting with `_` are exempt.",
    },
    Rule {
        id: "single-use-variable",
        sparql: true,
        turtle: false,
        default: Severity::Hint,
        safe_fix: false,
        summary: "A variable that occurs once, in a triple pattern or `GRAPH`, and is not projected: a wildcard or a typo. Names starting with `_` are exempt.",
    },
    Rule {
        id: "unbound-variable",
        sparql: true,
        turtle: false,
        default: Severity::Warning,
        safe_fix: false,
        summary: "A variable that is projected, filtered, ordered or used in a template but never bound.",
    },
    Rule {
        id: "cartesian-product",
        sparql: true,
        turtle: false,
        default: Severity::Warning,
        safe_fix: false,
        summary: "Patterns of one group that share no variable, so the group joins them as a cross product.",
    },
    Rule {
        id: "select-star-group-by",
        sparql: true,
        turtle: false,
        default: Severity::Error,
        safe_fix: false,
        summary: "`SELECT *` in a query that groups, which SPARQL does not allow.",
    },
    Rule {
        id: "ungrouped-variable",
        sparql: true,
        turtle: false,
        default: Severity::Error,
        safe_fix: false,
        summary: "A projected variable that is neither grouped nor inside an aggregate.",
    },
    Rule {
        id: "filter-scope",
        sparql: true,
        turtle: false,
        default: Severity::Warning,
        safe_fix: false,
        summary: "A FILTER in a nested group that tests a variable bound only outside that group.",
    },
    Rule {
        id: "filter-equality",
        sparql: true,
        turtle: false,
        default: Severity::Hint,
        safe_fix: false,
        summary: "`FILTER(?v = <iri>)` where the IRI could be written in the triple pattern, which an index answers directly.",
    },
    Rule {
        id: "iri-space",
        sparql: true,
        turtle: true,
        default: Severity::Warning,
        safe_fix: false,
        summary: "An IRI that holds a space or another whitespace character.",
    },
    Rule {
        id: "language-tag-case",
        sparql: true,
        turtle: true,
        default: Severity::Warning,
        safe_fix: true,
        summary: "A language tag not in the case BCP 47 recommends (`en-US`, `zh-Hant`). The fix rewrites it.",
    },
    Rule {
        id: "deprecated-language-tag",
        sparql: true,
        turtle: true,
        default: Severity::Warning,
        safe_fix: false,
        summary: "A language tag with a deprecated subtag (`iw`, `i-klingon`), with its replacement.",
    },
    Rule {
        id: "suspicious-datatype",
        sparql: true,
        turtle: true,
        default: Severity::Warning,
        safe_fix: false,
        summary: "A literal whose lexical form is not valid for its XML Schema datatype, a misspelled XML Schema datatype, or `rdf:langString` written as a datatype.",
    },
    Rule {
        id: "redundant-datatype",
        sparql: true,
        turtle: true,
        default: Severity::Info,
        safe_fix: true,
        summary: "`\"…\"^^xsd:string`, which is the same term as the plain string. The fix drops the datatype.",
    },
    Rule {
        id: "deprecated-syntax",
        sparql: true,
        turtle: true,
        default: Severity::Warning,
        safe_fix: false,
        summary: "A deprecated name: Jena's `jena.hpl.hp.com` function namespaces or OWL's `owl:DataRange`.",
    },
];

/// The rule named `id`.
pub fn rule(id: &str) -> Option<&'static Rule> {
    RULES.iter().find(|r| r.id == id)
}

/// Which rules run, and at what severity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LintOptions {
    /// per-rule severities over the defaults; `None` turns a rule off
    pub levels: BTreeMap<String, Option<Severity>>,
    /// give up with [`LintError::Timeout`] after this instant
    pub deadline: Option<Instant>,
}

impl LintOptions {
    /// Set rule `id` to `level` (`error`, `warning`, `info`, `hint` or `off`).
    pub fn set(&mut self, id: &str, level: &str) -> Result<(), String> {
        let r = rule(id).ok_or_else(|| format!("{id}: unknown lint rule"))?;
        let level = Severity::parse(level).map_err(|e| format!("{id}: {e}"))?;
        if r.id == "syntax" && level.is_none() {
            return Err("syntax: syntax errors cannot be turned off".to_string());
        }
        self.levels.insert(r.id.to_string(), level);
        Ok(())
    }

    /// The severity of rule `id`, or `None` when it is off.
    pub fn level(&self, id: &str) -> Option<Severity> {
        match self.levels.get(id) {
            Some(l) => *l,
            None => rule(id).map(|r| r.default),
        }
    }
}

/// A change to the input: replace bytes `start..end` with `insert`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub insert: String,
}

/// How to fix a diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    /// what the fix does, for an editor's quick-fix menu
    pub title: String,
    pub edits: Vec<Edit>,
}

/// One finding. Positions are byte offsets in the input; lines and columns are 1-based,
/// the columns in Unicode scalar values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub rule: &'static str,
    pub severity: Severity,
    pub message: String,
    pub start: usize,
    pub end: usize,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub fix: Option<Fix>,
}

/// What [`lint`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Linted {
    pub language: Language,
    /// in order of position
    pub diagnostics: Vec<Diagnostic>,
}

/// What [`fix`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fixed {
    pub text: String,
    /// the number of fixes applied
    pub applied: usize,
    /// the diagnostics of `text`
    pub diagnostics: Vec<Diagnostic>,
}

/// Why a document was not linted.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LintError {
    #[error("{0} is not linted: lint takes SPARQL, Turtle and TriG")]
    UnsupportedLanguage(&'static str),
    #[error("linting did not finish before its deadline")]
    Timeout,
    #[error("the input is too large to lint")]
    TooLarge,
    /// the fixed document does not mean the same as the input (a bug in a fix)
    #[error("a fix changed the document's meaning; nothing was fixed; please report")]
    UnsafeFix,
}

impl LintError {
    /// `unsupported-language`, `timeout`, `too-large` or `unsafe-fix`.
    pub fn code(&self) -> &'static str {
        match self {
            LintError::UnsupportedLanguage(_) => "unsupported-language",
            LintError::Timeout => "timeout",
            LintError::TooLarge => "too-large",
            LintError::UnsafeFix => "unsafe-fix",
        }
    }
}

/// Whether `lint` takes `lang`.
pub fn lints(lang: Language) -> bool {
    matches!(lang, Language::Sparql | Language::Turtle | Language::TriG)
}

/// Lint `text` as `lang`.
pub fn lint(text: &str, lang: Language, opts: &LintOptions) -> Result<Linted, LintError> {
    if !lints(lang) {
        return Err(LintError::UnsupportedLanguage(lang.display_name()));
    }
    if text.len() > u32::MAX as usize {
        return Err(LintError::TooLarge);
    }
    let mut out = Out {
        text,
        opts,
        found: Vec::new(),
    };
    match lang {
        Language::Sparql => lint_sparql(text, &mut out)?,
        _ => lint_turtle(text, lang == Language::TriG, &mut out)?,
    }
    let mut diagnostics = out.found;
    diagnostics.sort_by(|a, b| (a.start, a.end, a.rule).cmp(&(b.start, b.end, b.rule)));
    diagnostics.dedup_by(|a, b| a.rule == b.rule && a.start == b.start && a.end == b.end);
    Ok(Linted {
        language: lang,
        diagnostics,
    })
}

/// Apply the safe fixes of the enabled rules, again until none is left (at most a few
/// rounds), and lint the result. The output is refused unless it means what the input
/// meant ([`LintError::UnsafeFix`]). A document with a syntax error is returned as it is.
pub fn fix(text: &str, lang: Language, opts: &LintOptions) -> Result<Fixed, LintError> {
    let mut current = text.to_string();
    let mut applied = 0;
    let mut linted = lint(&current, lang, opts)?;
    for _ in 0..4 {
        let edits = safe_edits(&linted.diagnostics);
        if edits.is_empty() {
            break;
        }
        applied += edits.len();
        current = apply(&current, &edits);
        linted = lint(&current, lang, opts)?;
    }
    if applied > 0 && !same_meaning(text, &current, lang) {
        return Err(LintError::UnsafeFix);
    }
    Ok(Fixed {
        text: current,
        applied,
        diagnostics: linted.diagnostics,
    })
}

/// The fixes of the safe rules among `diagnostics`, as non-overlapping edits in order.
/// Two fixes that touch the same bytes keep the first; the next round applies the other.
fn safe_edits(diagnostics: &[Diagnostic]) -> Vec<Edit> {
    let mut edits: Vec<Edit> = Vec::new();
    for d in diagnostics {
        let (Some(f), Some(r)) = (&d.fix, rule(d.rule)) else {
            continue;
        };
        if !r.safe_fix {
            continue;
        }
        let mut fe = f.edits.clone();
        fe.sort_by_key(|e| e.start);
        let clashes = fe.iter().any(|e| {
            edits
                .iter()
                .any(|o| e.start < o.end.max(o.start + 1) && o.start < e.end.max(e.start + 1))
        });
        if !clashes {
            edits.extend(fe);
        }
    }
    edits.sort_by_key(|e| e.start);
    edits
}

/// `text` with `edits` (sorted, not overlapping) applied.
pub fn apply(text: &str, edits: &[Edit]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for e in edits {
        if e.start < at {
            continue;
        }
        out.push_str(&text[at..e.start]);
        out.push_str(&e.insert);
        at = e.end;
    }
    out.push_str(&text[at..]);
    out
}

/// Whether `a` and `b` parse to the same algebra (SPARQL) or isomorphic data.
fn same_meaning(a: &str, b: &str, lang: Language) -> bool {
    match lang {
        Language::Sparql => {
            let ta = lex(a, LexMode::Sparql);
            match crate::check::sparql_reference(a, &ta) {
                Ok(r) => crate::check::sparql_equivalent(&r, b).is_ok(),
                Err(_) => false,
            }
        }
        _ => match crate::check::graph::rdf_reference(a, lang) {
            Ok(r) => crate::check::graph::rdf_equivalent(&r, b).is_ok(),
            Err(_) => false,
        },
    }
}

// ------------------------------------------------------------------ pipeline ------

/// Where the rules put what they find.
pub(crate) struct Out<'a> {
    pub text: &'a str,
    pub opts: &'a LintOptions,
    pub found: Vec<Diagnostic>,
}

impl Out<'_> {
    /// Whether rule `id` runs.
    pub fn on(&self, id: &str) -> bool {
        self.opts.level(id).is_some()
    }

    /// Report a finding of rule `id` over bytes `start..end`, unless the rule is off.
    pub fn report(&mut self, id: &'static str, start: usize, end: usize, message: String) {
        self.report_fix(id, start, end, message, None);
    }

    pub fn report_fix(
        &mut self,
        id: &'static str,
        start: usize,
        end: usize,
        message: String,
        fix: Option<Fix>,
    ) {
        let Some(severity) = self.opts.level(id) else {
            return;
        };
        let end = end.max(start).min(self.text.len());
        let start = start.min(end);
        let (line, column) = crate::line_col(self.text, start);
        let (end_line, end_column) = crate::line_col(self.text, end);
        self.found.push(Diagnostic {
            rule: id,
            severity,
            message,
            start,
            end,
            line,
            column,
            end_line,
            end_column,
            fix,
        });
    }

    pub fn deadline(&self) -> Result<(), LintError> {
        crate::check::deadline(self.opts.deadline).map_err(|_| LintError::Timeout)
    }

    /// The reference parser's error as a `syntax` diagnostic on the character there.
    fn syntax(&mut self, e: &FormatError) {
        let (message, offset) = match e {
            FormatError::Syntax {
                message, offset, ..
            } => (message.clone(), *offset),
            other => (other.to_string(), 0),
        };
        let end = next_char_end(self.text, offset);
        self.report("syntax", offset, end, message);
    }
}

/// The end of the character at `offset` (at a line break or the end: `offset`).
fn next_char_end(text: &str, offset: usize) -> usize {
    let mut o = offset.min(text.len());
    while !text.is_char_boundary(o) {
        o -= 1;
    }
    text[o..]
        .chars()
        .next()
        .filter(|c| !matches!(c, '\n' | '\r'))
        .map_or(o, |c| o + c.len_utf8())
}

fn lint_sparql(text: &str, out: &mut Out<'_>) -> Result<(), LintError> {
    let tokens = lex(text, LexMode::Sparql);
    if let Err(e) = crate::sparql::nesting(text, &tokens) {
        out.syntax(&e);
        return Ok(());
    }
    let reference = crate::check::sparql_reference(text, &tokens);
    out.deadline()?;
    let unit = match &reference {
        Ok(r) => r.unit,
        Err(_) => crate::check::sparql_unit(text, &tokens).unwrap_or(Unit::Query),
    };
    let tree = crate::sparql::parse::parse(text, tokens.clone(), unit).or_else(|e| {
        // a document the reference parser could not place
        let other = match unit {
            Unit::Query => Unit::Update,
            Unit::Update => Unit::Query,
        };
        crate::sparql::parse::parse(text, tokens.clone(), other).map_err(|_| e)
    });
    match (reference, tree) {
        (Ok(_), Ok(tree)) => {
            let c = Cst::new(&tree);
            prefixes::run(&c, out);
            sparql::run(&c, out)?;
            terms::run(&c, out);
        }
        (Ok(_), Err(e)) => out.syntax(&e),
        // the tree's parser takes what fails only on variable scoping: the scope rules
        // explain it, else the parser's message stands
        (Err(e), Ok(tree)) => {
            let before = out.found.len();
            let c = Cst::new(&tree);
            sparql::scope_errors(&c, out);
            if out.found.len() == before {
                out.syntax(&e);
            }
        }
        (Err(e), Err(_)) => {
            if !terms::iri_space_in_error(text, &e, out) {
                out.syntax(&e);
            }
        }
    }
    Ok(())
}

fn lint_turtle(text: &str, trig: bool, out: &mut Out<'_>) -> Result<(), LintError> {
    let tokens = lex(text, LexMode::Turtle);
    if let Err(e) = crate::sparql::nesting(text, &tokens) {
        out.syntax(&e);
        return Ok(());
    }
    // undeclared prefixes are this lint's finding, not a syntax error: the reference
    // parse declares them
    let undeclared = undeclared_turtle_labels(text, &tokens);
    let reference = turtle_reference(text, trig, &undeclared);
    out.deadline()?;
    if let Err(e) = reference {
        if !terms::iri_space_in_error(text, &e, out) {
            out.syntax(&e);
        }
        return Ok(());
    }
    match crate::turtle::parse::parse(text, tokens, trig) {
        Ok(tree) => {
            let c = Cst::new(&tree);
            prefixes::run(&c, out);
            terms::run(&c, out);
        }
        Err(e) => out.syntax(&e),
    }
    Ok(())
}

/// The prefix labels a Turtle or TriG document uses without declaring them first
/// (`@prefix` or `PREFIX`), each once.
fn undeclared_turtle_labels(text: &str, tokens: &[Token]) -> Vec<String> {
    let sig: Vec<&Token> = tokens.iter().filter(|t| !t.kind.is_trivia()).collect();
    let mut declared = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for (i, t) in sig.iter().enumerate() {
        if !matches!(t.kind, TokenKind::PnameNs | TokenKind::PnameLn) {
            continue;
        }
        let s = t.text(text);
        let label = &s[..s.find(':').unwrap_or(s.len())];
        let directive = i > 0
            && match sig[i - 1].kind {
                TokenKind::LangDir => sig[i - 1].text(text) == "@prefix",
                TokenKind::Word => sig[i - 1].text(text).eq_ignore_ascii_case("prefix"),
                _ => false,
            };
        if directive && t.kind == TokenKind::PnameNs {
            declared.insert(label.to_string());
        } else if !declared.contains(label) && !out.iter().any(|l| l == label) {
            out.push(label.to_string());
        }
    }
    out
}

/// oxttl's parse of a Turtle or TriG document, with `undeclared` prefixes declared.
fn turtle_reference(text: &str, trig: bool, undeclared: &[String]) -> Result<(), FormatError> {
    let bom = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let body = &text[bom..];
    let positioned = |e: oxttl::TurtleSyntaxError| {
        let offset = bom + e.location().start.offset as usize;
        let (line, column) = crate::line_col(text, offset);
        FormatError::Syntax {
            message: e.message().to_string(),
            line,
            column,
            offset,
        }
    };
    let base = crate::check::graph::RDF_BASE;
    if trig {
        let mut p = oxttl::TriGParser::new()
            .with_base_iri(base)
            .expect("a valid base IRI");
        for l in undeclared {
            p = p
                .with_prefix(l.as_str(), crate::check::undeclared_namespace(l))
                .expect("a valid namespace IRI");
        }
        for q in p.for_slice(body) {
            q.map_err(positioned)?;
        }
    } else {
        let mut p = oxttl::TurtleParser::new()
            .with_base_iri(base)
            .expect("a valid base IRI");
        for l in undeclared {
            p = p
                .with_prefix(l.as_str(), crate::check::undeclared_namespace(l))
                .expect("a valid namespace IRI");
        }
        for t in p.for_slice(body) {
            t.map_err(positioned)?;
        }
    }
    Ok(())
}

// ----------------------------------------------------------------- tree helpers ------

/// A syntax tree with what the rules ask of it.
pub(crate) struct Cst<'a, 's> {
    pub tree: &'a Tree<'s>,
    pub scope: crate::normalize::PrefixScope,
}

impl<'a, 's> Cst<'a, 's> {
    fn new(tree: &'a Tree<'s>) -> Self {
        Cst {
            tree,
            scope: crate::normalize::PrefixScope::from_tree(tree),
        }
    }

    pub fn kind(&self, n: NodeId) -> crate::syntax::NodeKind {
        self.tree.kind(n)
    }

    /// Every node of the tree, in document order.
    pub fn nodes(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![self.tree.root()];
        while let Some(n) = stack.pop() {
            out.push(n);
            for c in self
                .tree
                .child_nodes(n)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                stack.push(c);
            }
        }
        out
    }

    /// The tokens that are direct children of `n`.
    pub fn own_tokens(&self, n: NodeId) -> Vec<TokenId> {
        self.tree
            .children(n)
            .iter()
            .filter_map(|e| match *e {
                Element::Token(t) => Some(t),
                Element::Node(_) => None,
            })
            .collect()
    }

    /// The tokens of `n`'s subtree, not entering nodes for which `skip` holds.
    pub fn tokens_where(
        &self,
        n: NodeId,
        skip: &dyn Fn(crate::syntax::NodeKind) -> bool,
    ) -> Vec<TokenId> {
        let mut out = Vec::new();
        self.collect(n, skip, &mut out);
        out
    }

    fn collect(
        &self,
        n: NodeId,
        skip: &dyn Fn(crate::syntax::NodeKind) -> bool,
        out: &mut Vec<TokenId>,
    ) {
        for e in self.tree.children(n) {
            match *e {
                Element::Token(t) => out.push(t),
                Element::Node(c) if !skip(self.tree.kind(c)) => self.collect(c, skip, out),
                Element::Node(_) => {}
            }
        }
    }

    pub fn token(&self, t: TokenId) -> Token {
        self.tree.token(t)
    }

    pub fn token_kind(&self, t: TokenId) -> TokenKind {
        self.tree.token_kind(t)
    }

    pub fn text(&self, t: TokenId) -> &'s str {
        self.tree.token_text(t)
    }

    pub fn span(&self, t: TokenId) -> (usize, usize) {
        let tok = self.tree.token(t);
        (tok.start as usize, tok.end())
    }

    /// The span of a node's tokens.
    pub fn node_span(&self, n: NodeId) -> (usize, usize) {
        let r = self.tree.range(n);
        (r.start, r.end)
    }

    /// The nearest ancestor of `n` (not `n`) of one of `kinds`.
    pub fn ancestor(&self, n: NodeId, kinds: &[crate::syntax::NodeKind]) -> Option<NodeId> {
        let mut at = self.tree.parent(n);
        while let Some(p) = at {
            if kinds.contains(&self.tree.kind(p)) {
                return Some(p);
            }
            at = self.tree.parent(p);
        }
        None
    }

    /// The namespace and local part a prefixed name token stands for, unescaped.
    pub fn expand(&self, t: TokenId) -> Option<String> {
        let text = self.text(t);
        let (label, local) = text.split_once(':')?;
        let ns = self.scope.resolve(label, t)?;
        Some(format!("{ns}{}", unescape_local(local)))
    }

    /// The IRI of an IRI or prefixed-name token, if it can be told (relative IRIs as
    /// written).
    pub fn iri(&self, t: TokenId) -> Option<String> {
        match self.token_kind(t) {
            TokenKind::IriRef => {
                let s = self.text(t);
                Some(unescape_iri(&s[1..s.len() - 1]))
            }
            TokenKind::PnameLn | TokenKind::PnameNs => self.expand(t),
            _ => None,
        }
    }
}

/// A prefixed name's local part without its `\` escapes.
fn unescape_local(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// An IRI reference's text with its `\u` and `\U` escapes decoded.
pub(crate) fn unescape_iri(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('\\') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let (len, digits) = match tail.as_bytes().get(1) {
            Some(b'u') => (6, 4),
            Some(b'U') => (10, 8),
            _ => (1, 0),
        };
        let decoded = (digits > 0)
            .then(|| tail.get(2..2 + digits))
            .flatten()
            .and_then(|h| u32::from_str_radix(h, 16).ok())
            .and_then(char::from_u32);
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &tail[len..];
            }
            None => {
                out.push('\\');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests;
