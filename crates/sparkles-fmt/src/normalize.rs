//! The normalizations: prefix scopes, prefix sorting and grouping (N5), IRI compaction
//! (N7), `rdf:type` → `a` (N8), numeric and boolean shorthand (N9) and quote style
//! (N10). Pure functions over the tree; printers pass what they return to
//! [`crate::doc::DocArena::token`].

pub mod prune;

use crate::lex::{TokenKind, is_pn_chars, is_pn_chars_u};
use crate::sparql::keywords::Kw;
use crate::tree::{NodeId, TokenId, Tree};

/// `rdf:type`
pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
/// The namespace of the XML Schema datatypes.
pub const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// One prefix declaration as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declared {
    /// the label without its `:` (`""` for the empty prefix)
    pub label: String,
    /// the namespace IRI without its `<` `>` (escapes as written)
    pub iri: String,
    /// the declaration's token: it is in scope after it
    pub at: TokenId,
}

/// The prefix declarations of a document, to know which are in scope at a token.
#[derive(Clone, Debug, Default)]
pub struct PrefixScope {
    declared: Vec<Declared>,
}

impl PrefixScope {
    /// Every `PREFIX label: <iri>` of the tree, in source order. A declaration is in
    /// scope from its IRI token on: in an update request a prologue's prefixes stay
    /// declared for the later operations.
    pub fn from_tree(tree: &Tree<'_>) -> PrefixScope {
        let sig: Vec<TokenId> = (0..tree.tokens.len() as u32)
            .map(TokenId)
            .filter(|&t| !tree.token_kind(t).is_trivia())
            .collect();
        let mut declared = Vec::new();
        for w in sig.windows(3) {
            let is_prefix = match tree.token_kind(w[0]) {
                TokenKind::Kw(Kw::Prefix) => true,
                TokenKind::Word => tree.token_text(w[0]).eq_ignore_ascii_case("prefix"),
                // Turtle's `@prefix` (case-sensitive)
                TokenKind::LangDir => tree.token_text(w[0]) == "@prefix",
                _ => false,
            };
            if is_prefix
                && tree.token_kind(w[1]) == TokenKind::PnameNs
                && tree.token_kind(w[2]) == TokenKind::IriRef
            {
                let label = tree.token_text(w[1]);
                let iri = tree.token_text(w[2]);
                declared.push(Declared {
                    label: label[..label.len() - 1].to_string(),
                    iri: iri[1..iri.len() - 1].to_string(),
                    at: w[2],
                });
            }
        }
        PrefixScope { declared }
    }

    /// The declarations in scope at `token`, a later one of a label shadowing an earlier
    /// one.
    pub fn at(&self, token: TokenId) -> Vec<&Declared> {
        let mut seen = std::collections::HashSet::new();
        let mut v: Vec<&Declared> = self
            .declared
            .iter()
            .rev()
            .filter(|d| d.at < token && seen.insert(d.label.as_str()))
            .collect();
        v.reverse();
        v
    }

    /// The namespace `label` stands for at `token`.
    pub fn resolve(&self, label: &str, token: TokenId) -> Option<&str> {
        self.declared
            .iter()
            .rev()
            .find(|d| d.at < token && d.label == label)
            .map(|d| d.iri.as_str())
    }
}

// ----------------------------------------------------------------------- N5 ------

/// A prefix declaration of a prologue, for [`plan_runs`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrefixDecl {
    pub node: NodeId,
    pub label: String,
    pub iri: String,
    /// it has comments of its own (it is never dropped as a duplicate)
    pub has_comments: bool,
    /// a run ends before it whatever the options: a `BASE`, a `VERSION` or a detached
    /// comment block between it and the previous declaration
    pub barrier_before: bool,
    /// the source has a blank line between it and the previous declaration: a run ends
    /// there when no prefix groups are configured; with groups the formatter owns the
    /// blank lines
    pub blank_before: bool,
}

/// How to print one run of declarations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Run {
    /// indexes into the declarations, in printing order
    pub order: Vec<usize>,
    /// indexes of exact, comment-free duplicates that are not printed
    pub drop: Vec<usize>,
    /// positions in `order` before which a blank line separates two groups
    pub group_breaks: Vec<usize>,
    /// `false` when the run is printed as written (a label bound to two different
    /// IRIs): neither sorted nor grouped, its source blank lines kept
    pub sorted: bool,
}

/// Sort and group the prefix declarations within their runs (N5 and `prefix-groups`).
///
/// A run is a contiguous stretch of declarations without a barrier. Within it, exact
/// duplicates (same label and IRI) that carry no comment are dropped, keeping one;
/// the rest are ordered by group (configured order, unlisted labels last), then by label
/// in codepoint order (the empty label first), stably. A run that binds a label to two
/// different IRIs is left as written: sorting it would change which binding wins.
pub fn plan_runs(decls: &[PrefixDecl], groups: &[Vec<String>]) -> Vec<Run> {
    let mut bounds: Vec<std::ops::Range<usize>> = Vec::new();
    for (i, d) in decls.iter().enumerate() {
        let new_run = i == 0 || d.barrier_before || (groups.is_empty() && d.blank_before);
        if new_run {
            bounds.push(i..i + 1);
        } else {
            bounds.last_mut().expect("a run").end = i + 1;
        }
    }
    bounds
        .into_iter()
        .map(|r| plan_run(decls, r, groups))
        .collect()
}

fn plan_run(decls: &[PrefixDecl], run: std::ops::Range<usize>, groups: &[Vec<String>]) -> Run {
    let idx: Vec<usize> = run.collect();
    let conflict = idx.iter().any(|&i| {
        idx.iter()
            .any(|&j| decls[i].label == decls[j].label && decls[i].iri != decls[j].iri)
    });
    if conflict {
        return Run {
            order: idx,
            ..Run::default()
        };
    }
    // of identical declarations, keep those with comments, or else the first
    let mut drop = Vec::new();
    for &i in &idx {
        let d = &decls[i];
        if d.has_comments {
            continue;
        }
        let twins = || {
            idx.iter()
                .filter(|&&j| j != i && decls[j].label == d.label && decls[j].iri == d.iri)
        };
        let commented_twin = twins().any(|&j| decls[j].has_comments);
        let earlier_plain_twin = twins().any(|&j| j < i && !decls[j].has_comments);
        if commented_twin || earlier_plain_twin {
            drop.push(i);
        }
    }
    let group_of = |label: &str| {
        groups
            .iter()
            .position(|g| g.iter().any(|l| l == label))
            .unwrap_or(groups.len())
    };
    let mut order: Vec<usize> = idx.into_iter().filter(|i| !drop.contains(i)).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&decls[a], &decls[b]);
        (group_of(&a.label), a.label.as_str()).cmp(&(group_of(&b.label), b.label.as_str()))
    });
    let group_breaks = (1..order.len())
        .filter(|&k| group_of(&decls[order[k - 1]].label) != group_of(&decls[order[k]].label))
        .collect();
    Run {
        order,
        drop,
        group_breaks,
        sorted: true,
    }
}

// ----------------------------------------------------------------------- N7 ------

/// The prefixed name for a full IRI token (`<…>` as written) when a prefix in scope at
/// `at` covers it (N7).
///
/// Only an absolute IRI without `\u` escapes or dot segments is compacted, with a
/// namespace that is itself absolute and plain; the rest must be a `PN_LOCAL` that needs
/// no `\` escape and does not end in `.` (it may be empty; `%HH` stays as it is). The
/// longest namespace wins, a tie goes to the earliest declaration. Printers never call
/// it for the IRI of a `PREFIX` or `BASE`.
pub fn compact_iri(iri_token: &str, scope: &PrefixScope, at: TokenId) -> Option<String> {
    let iri = iri_token.strip_prefix('<')?.strip_suffix('>')?;
    if !plain_absolute(iri) {
        return None;
    }
    let mut best: Option<&Declared> = None;
    for d in scope.at(at) {
        let longer = best.is_none_or(|b| d.iri.len() > b.iri.len());
        if longer
            && plain_absolute(&d.iri)
            && iri.starts_with(d.iri.as_str())
            && is_pn_local(&iri[d.iri.len()..])
        {
            best = Some(d);
        }
    }
    best.map(|d| format!("{}:{}", d.label, &iri[d.iri.len()..]))
}

/// An absolute IRI (it has a scheme) that is written without escapes and without `.` or
/// `..` path segments, which resolution would remove.
fn plain_absolute(iri: &str) -> bool {
    let scheme_end = match iri.find(':') {
        Some(i) if i > 0 => i,
        _ => return false,
    };
    let scheme = &iri[..scheme_end];
    scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !iri.contains('\\')
        && !iri
            .split(['/', '?', '#'])
            .any(|seg| seg == "." || seg == "..")
}

/// Whether `s` can follow `label:` as written: `PN_LOCAL` without `\` escapes, or empty.
fn is_pn_local(s: &str) -> bool {
    if s.ends_with('.') {
        return false;
    }
    let b = s.as_bytes();
    let mut i = 0;
    while i < s.len() {
        let c = s[i..].chars().next().expect("not at the end");
        if c == '%' {
            if !(b.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                && b.get(i + 2).is_some_and(u8::is_ascii_hexdigit))
            {
                return false;
            }
            i += 3;
            continue;
        }
        let ok = if i == 0 {
            is_pn_chars_u(c) || c == ':' || c.is_ascii_digit()
        } else {
            is_pn_chars(c) || c == '.' || c == ':'
        };
        if !ok {
            return false;
        }
        i += c.len_utf8();
    }
    true
}

// ----------------------------------------------------------------------- N8 ------

/// Whether a verb token denotes `rdf:type` (an `IRIREF` or a prefixed name), for N8.
/// Only plain spellings count: an IRI with escapes or a relative IRI never does.
pub fn is_rdf_type(tree: &Tree<'_>, token: TokenId, scope: &PrefixScope) -> bool {
    let text = tree.token_text(token);
    match tree.token_kind(token) {
        TokenKind::IriRef => {
            text.strip_prefix('<').and_then(|t| t.strip_suffix('>')) == Some(RDF_TYPE)
        }
        TokenKind::PnameLn => {
            let (label, local) = text.split_once(':').expect("a prefixed name has a colon");
            scope
                .resolve(label, token)
                .is_some_and(|ns| RDF_TYPE.strip_prefix(ns) == Some(local) && plain_absolute(ns))
        }
        _ => false,
    }
}

// ----------------------------------------------------------------------- N9 ------

/// The numeric or boolean token a typed literal can be written as: `lexical` is the
/// string token as written (quotes included), `datatype` the datatype's full IRI (with
/// or without `<` `>`) (N9).
///
/// The shorthand is the string's content, and only when that content is exactly the
/// token the grammar reads as a literal of that datatype with that lexical form:
/// `"01"^^xsd:integer` → `01`, `".5"^^xsd:decimal` → `.5`, `"1e3"^^xsd:double` → `1e3`,
/// `"true"^^xsd:boolean` → `true`. Never for `"1"^^xsd:decimal` (`1` is an integer),
/// `"1."^^xsd:decimal`, `"INF"^^xsd:double`, `"TRUE"`/`"1"^^xsd:boolean`, or a string
/// with escapes.
pub fn literal_shorthand<'a>(lexical: &'a str, datatype: &str) -> Option<&'a str> {
    let datatype = datatype
        .strip_prefix('<')
        .and_then(|d| d.strip_suffix('>'))
        .unwrap_or(datatype);
    let content = string_content(lexical)?;
    let ok = match datatype.strip_prefix(XSD)? {
        "integer" => is_integer(content),
        "decimal" => is_decimal(content),
        "double" => is_double(content),
        "boolean" => content == "true" || content == "false",
        _ => false,
    };
    ok.then_some(content)
}

/// The characters between the quotes of a string token.
fn string_content(s: &str) -> Option<&str> {
    for q in ["\"\"\"", "'''"] {
        if s.len() >= 6 && s.starts_with(q) && s.ends_with(q) {
            return Some(&s[3..s.len() - 3]);
        }
    }
    for q in ["\"", "'"] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return Some(&s[1..s.len() - 1]);
        }
    }
    None
}

/// `s` without a leading `+` or `-`.
fn unsigned(s: &str) -> &str {
    s.strip_prefix(['+', '-']).unwrap_or(s)
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `[+-]? [0-9]+`
fn is_integer(s: &str) -> bool {
    digits(unsigned(s))
}

/// `[+-]? [0-9]* '.' [0-9]+`
fn is_decimal(s: &str) -> bool {
    match unsigned(s).split_once('.') {
        Some((int, frac)) => (int.is_empty() || digits(int)) && digits(frac),
        None => false,
    }
}

/// `[+-]? ( [0-9]+ '.' [0-9]* EXPONENT | '.' [0-9]+ EXPONENT | [0-9]+ EXPONENT )`,
/// `EXPONENT ::= [eE] [+-]? [0-9]+`
fn is_double(s: &str) -> bool {
    let s = unsigned(s);
    let Some(e) = s.find(['e', 'E']) else {
        return false;
    };
    let (mantissa, exp) = (&s[..e], &s[e + 1..]);
    let mantissa_ok = match mantissa.split_once('.') {
        Some((int, frac)) => {
            (digits(int) && (frac.is_empty() || digits(frac))) || (int.is_empty() && digits(frac))
        }
        None => digits(mantissa),
    };
    mantissa_ok && digits(unsigned(exp))
}

// ---------------------------------------------------------------------- N10 ------

/// A string token rewritten with double quotes, if it can be (N10): `'…'` → `"…"` and
/// `'''…'''` → `"""…"""` when the content holds no `"`, with `\'` unescaped. Every
/// other escape stays as written; double-quoted strings are already right (`None`).
pub fn requote(text: &str, kind: TokenKind) -> Option<String> {
    let (content, quote) = match kind {
        TokenKind::String1 => (text.strip_prefix('\'')?.strip_suffix('\'')?, "\""),
        TokenKind::StringLong1 => (text.strip_prefix("'''")?.strip_suffix("'''")?, "\"\"\""),
        _ => return None,
    };
    if content.contains('"') {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(quote);
    let mut chars = content.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\'') => out.push('\''),
            Some(n) => {
                out.push('\\');
                out.push(n);
            }
            None => out.push('\\'),
        }
    }
    out.push_str(quote);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::{LexMode, lex};
    use crate::sparql::Unit;

    fn tree(src: &str) -> Tree<'_> {
        crate::sparql::parse::parse(src, lex(src, LexMode::Sparql), Unit::Query).unwrap()
    }

    /// The id of the first token whose text is `text`.
    fn tok(t: &Tree<'_>, text: &str) -> TokenId {
        (0..t.tokens.len() as u32)
            .map(TokenId)
            .find(|&i| t.token_text(i) == text)
            .unwrap_or_else(|| panic!("no token {text}"))
    }

    /// The id of the last token whose text is `text`.
    fn last_tok(t: &Tree<'_>, text: &str) -> TokenId {
        (0..t.tokens.len() as u32)
            .rev()
            .map(TokenId)
            .find(|&i| t.token_text(i) == text)
            .unwrap_or_else(|| panic!("no token {text}"))
    }

    #[test]
    fn scope_follows_the_declarations() {
        let src =
            "PREFIX ex: <http://e/1/>\nprefix : <http://e/>\nSELECT * { ?s ex:p <http://e/1/a> }";
        let t = tree(src);
        let scope = PrefixScope::from_tree(&t);
        let labels = |at| -> Vec<String> { scope.at(at).iter().map(|d| d.label.clone()).collect() };
        assert_eq!(labels(tok(&t, "ex:")), Vec::<String>::new());
        assert_eq!(labels(tok(&t, ":")), ["ex"]);
        assert_eq!(labels(tok(&t, "?s")), ["ex", ""]);
        assert_eq!(scope.resolve("ex", tok(&t, "?s")), Some("http://e/1/"));
        assert_eq!(scope.resolve("ex", tok(&t, "ex:")), None);
    }

    #[test]
    fn a_later_declaration_shadows() {
        let src = "PREFIX ex: <http://e/1/> PREFIX ex: <http://e/2/> PREFIX ex2: <http://e/2/> SELECT * { ?s ?p <http://e/2/a> }";
        let t = tree(src);
        let scope = PrefixScope::from_tree(&t);
        let at = tok(&t, "<http://e/2/a>");
        assert_eq!(scope.resolve("ex", at), Some("http://e/2/"));
        assert_eq!(scope.at(at).len(), 2);
        // a tie goes to the earliest declaration in scope
        assert_eq!(
            compact_iri("<http://e/2/a>", &scope, at).as_deref(),
            Some("ex:a")
        );
        assert_eq!(compact_iri("<http://e/1/a>", &scope, at), None);
    }

    fn compact(src: &str, iri: &str) -> Option<String> {
        let t = tree(src);
        let scope = PrefixScope::from_tree(&t);
        compact_iri(iri, &scope, last_tok(&t, iri))
    }

    #[test]
    fn compacts_iris_under_the_conditions() {
        let p = "PREFIX ex: <http://e/> PREFIX exa: <http://e/a/> PREFIX : <http://f/> SELECT * { ";
        let q = |iri: &str| compact(&format!("{p}?s ?p {iri} }}"), iri);
        assert_eq!(q("<http://e/x>").as_deref(), Some("ex:x"));
        // the longest namespace
        assert_eq!(q("<http://e/a/x>").as_deref(), Some("exa:x"));
        assert_eq!(q("<http://f/x>").as_deref(), Some(":x"));
        // an empty local name
        assert_eq!(q("<http://e/>").as_deref(), Some("ex:"));
        assert_eq!(q("<http://e/a/>").as_deref(), Some("exa:"));
        // allowed local names: digits first, inner dots, colons, %HH, non-ASCII
        assert_eq!(q("<http://e/1a>").as_deref(), Some("ex:1a"));
        assert_eq!(q("<http://e/a.b>").as_deref(), Some("ex:a.b"));
        assert_eq!(q("<http://e/a:b>").as_deref(), Some("ex:a:b"));
        assert_eq!(q("<http://e/a%20b>").as_deref(), Some("ex:a%20b"));
        assert_eq!(q("<http://e/café>").as_deref(), Some("ex:café"));
        assert_eq!(q("<http://e/_x-y>").as_deref(), Some("ex:_x-y"));
        // local names that would need a `\` escape, or end in `.`
        for iri in [
            "<http://e/a.>",
            "<http://e/a/b/c>",
            "<http://e/-a>",
            "<http://e/.a>",
            "<http://e/a~b>",
            "<http://e/a?b>",
            "<http://e/a#b>",
            "<http://e/a&b>",
            "<http://e/a%2>",
            "<http://e/a%zz>",
        ] {
            assert_eq!(q(iri), None, "{iri}");
        }
        // no namespace covers it
        assert_eq!(q("<http://g/x>"), None);
        // UCHAR escapes, relative IRIs, dot segments
        assert_eq!(q("<http://e/\\u0061>"), None);
        assert_eq!(q("<x>"), None);
        assert_eq!(q("<http://e/../e/x>"), None);
    }

    #[test]
    fn only_declared_prefixes_in_scope_compact() {
        // declared after the use (in a later update operation) or never: no
        let t = tree("SELECT * { ?s ?p <http://e/x> }");
        let scope = PrefixScope::from_tree(&t);
        assert_eq!(
            compact_iri("<http://e/x>", &scope, tok(&t, "<http://e/x>")),
            None
        );
        let src = "INSERT DATA { <http://e/x> <http://e/p> 1 } ; PREFIX ex: <http://e/> INSERT DATA { <http://e/x> <http://e/p> 2 }";
        let t = crate::sparql::parse::parse(src, lex(src, LexMode::Sparql), Unit::Update).unwrap();
        let scope = PrefixScope::from_tree(&t);
        assert_eq!(
            compact_iri("<http://e/x>", &scope, tok(&t, "<http://e/x>")),
            None
        );
        assert_eq!(
            compact_iri("<http://e/x>", &scope, last_tok(&t, "<http://e/x>")).as_deref(),
            Some("ex:x")
        );
        // relative and escaped namespaces are never used
        let src =
            "PREFIX r: <e/> PREFIX u: <http://\\u0065/> SELECT * { ?s ?p <http://e/x>, <e/x> }";
        assert_eq!(compact(src, "<http://e/x>"), None);
        assert_eq!(compact(src, "<e/x>"), None);
    }

    #[test]
    fn rdf_type_spellings() {
        let src = "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> PREFIX r: <http://www.w3.org/1999/02/22-rdf-syntax-ns#ty> PREFIX x: <http://e/> SELECT * { ?a rdf:type ?b . ?a r:pe ?b . ?a <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> ?b . ?a x:type ?b . ?a rdf:types ?b . ?a rdf:Type ?b . ?a undeclared:type ?b }";
        let t = tree(src);
        let scope = PrefixScope::from_tree(&t);
        let is = |text: &str| is_rdf_type(&t, tok(&t, text), &scope);
        assert!(is("rdf:type"));
        assert!(is("r:pe"));
        assert!(is("<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>"));
        assert!(!is("x:type"));
        assert!(!is("rdf:types"));
        assert!(!is("rdf:Type"));
        assert!(!is("undeclared:type"));
        assert!(!is("?a"));
        // before its declaration
        let t = tree("SELECT * { ?a rdf:type ?b }");
        assert!(!is_rdf_type(
            &t,
            tok(&t, "rdf:type"),
            &PrefixScope::from_tree(&t)
        ));
    }

    #[test]
    fn literal_shorthand_table() {
        let xsd = |t: &str| format!("{XSD}{t}");
        let cases: &[(&str, &str, Option<&str>)] = &[
            ("\"1\"", "integer", Some("1")),
            ("\"01\"", "integer", Some("01")),
            ("\"+1\"", "integer", Some("+1")),
            ("\"-0\"", "integer", Some("-0")),
            ("'7'", "integer", Some("7")),
            ("\"\"\"7\"\"\"", "integer", Some("7")),
            ("'''7'''", "integer", Some("7")),
            ("\"1.5\"", "decimal", Some("1.5")),
            ("\".5\"", "decimal", Some(".5")),
            ("\"-.5\"", "decimal", Some("-.5")),
            ("\"1e3\"", "double", Some("1e3")),
            ("\"1.E-3\"", "double", Some("1.E-3")),
            ("\".5e+1\"", "double", Some(".5e+1")),
            ("\"-1.5e3\"", "double", Some("-1.5e3")),
            ("\"true\"", "boolean", Some("true")),
            ("\"false\"", "boolean", Some("false")),
            // the lexical form is not the token
            ("\"1\"", "decimal", None),
            ("\"1.\"", "decimal", None),
            ("\"1.0\"", "integer", None),
            ("\"1\"", "double", None),
            ("\"1.5\"", "double", None),
            ("\"e3\"", "double", None),
            ("\"1e\"", "double", None),
            ("\"INF\"", "double", None),
            ("\"NaN\"", "double", None),
            ("\"TRUE\"", "boolean", None),
            ("\"1\"", "boolean", None),
            ("\" 1\"", "integer", None),
            ("\"1 \"", "integer", None),
            ("\"\"", "integer", None),
            ("\"+\"", "integer", None),
            ("\"\\u0031\"", "integer", None),
            ("\"١\"", "integer", None),
            // other datatypes
            ("\"1\"", "int", None),
            ("\"1\"", "string", None),
        ];
        for &(lexical, dt, want) in cases {
            assert_eq!(
                literal_shorthand(lexical, &xsd(dt)),
                want,
                "{lexical}^^xsd:{dt}"
            );
        }
        assert_eq!(
            literal_shorthand("\"1\"", &format!("<{XSD}integer>")),
            Some("1")
        );
        assert_eq!(literal_shorthand("\"1\"", "http://e/integer"), None);
    }

    #[test]
    fn requote_rules() {
        use TokenKind::*;
        assert_eq!(requote("'a'", String1).as_deref(), Some("\"a\""));
        assert_eq!(requote("''", String1).as_deref(), Some("\"\""));
        assert_eq!(requote(r"'it\'s'", String1).as_deref(), Some("\"it's\""));
        // other escapes stay as written
        assert_eq!(
            requote(r"'a\nb\\'", String1).as_deref(),
            Some(r#""a\nb\\""#)
        );
        assert_eq!(requote(r"'a\\\'b'", String1).as_deref(), Some(r#""a\\'b""#));
        assert_eq!(
            requote(r"'\u0022'", String1).as_deref(),
            Some(r#""\u0022""#)
        );
        // a `"` in the content: unchanged
        assert_eq!(requote(r#"'say "hi"'"#, String1), None);
        assert_eq!(requote(r#"'say \"hi\"'"#, String1), None);
        // long strings stay long
        assert_eq!(
            requote("'''a\nb's'''", StringLong1).as_deref(),
            Some("\"\"\"a\nb's\"\"\"")
        );
        assert_eq!(requote(r#"'''a"b'''"#, StringLong1), None);
        // double quotes already
        assert_eq!(requote("\"a\"", String2), None);
        assert_eq!(requote("\"\"\"a\"\"\"", StringLong2), None);
    }

    fn decl(label: &str, iri: &str) -> PrefixDecl {
        PrefixDecl {
            node: NodeId(0),
            label: label.to_string(),
            iri: iri.to_string(),
            has_comments: false,
            barrier_before: false,
            blank_before: false,
        }
    }

    /// The labels of each run, in printing order, with `|` at group breaks.
    fn printed(decls: &[PrefixDecl], groups: &[&[&str]]) -> Vec<String> {
        let groups: Vec<Vec<String>> = groups
            .iter()
            .map(|g| g.iter().map(|l| l.to_string()).collect())
            .collect();
        plan_runs(decls, &groups)
            .iter()
            .map(|r| {
                let mut s = Vec::new();
                for (k, &i) in r.order.iter().enumerate() {
                    if r.group_breaks.contains(&k) {
                        s.push("|".to_string());
                    }
                    let l = &decls[i].label;
                    s.push(if l.is_empty() {
                        "\"\"".to_string()
                    } else {
                        l.clone()
                    });
                }
                s.join(" ")
            })
            .collect()
    }

    #[test]
    fn sorts_by_label_empty_first() {
        let d = [
            decl("foaf", "http://f/"),
            decl("ex", "http://e/"),
            decl("", "http://d/"),
            decl("Z", "http://z/"),
            decl("é", "http://x/"),
            decl("a", "http://a/"),
        ];
        // codepoint order: uppercase before lowercase, non-ASCII last
        assert_eq!(printed(&d, &[]), ["\"\" Z a ex foaf é"]);
        let runs = plan_runs(&d, &[]);
        assert!(runs[0].sorted && runs[0].drop.is_empty() && runs[0].group_breaks.is_empty());
    }

    #[test]
    fn drops_identical_comment_free_duplicates() {
        // the S1 example: the second foaf is identical
        let d = [
            decl("foaf", "http://f/"),
            decl("ex", "http://e/"),
            decl("foaf", "http://f/"),
        ];
        let runs = plan_runs(&d, &[]);
        assert_eq!(runs[0].order, [1, 0]);
        assert_eq!(runs[0].drop, [2]);
        // a duplicate with comments is kept, and its plain twin goes instead
        let mut d2 = d.clone();
        d2[2].has_comments = true;
        let runs = plan_runs(&d2, &[]);
        assert_eq!(runs[0].order, [1, 2]);
        assert_eq!(runs[0].drop, [0]);
        // two with comments: both kept, in source order
        d2[0].has_comments = true;
        let runs = plan_runs(&d2, &[]);
        assert_eq!(runs[0].order, [1, 0, 2]);
        assert!(runs[0].drop.is_empty());
    }

    #[test]
    fn a_label_bound_twice_leaves_the_run() {
        let d = [
            decl("z", "http://z/"),
            decl("ex", "http://e/1/"),
            decl("ex", "http://e/2/"),
            decl("z", "http://z/"),
        ];
        let runs = plan_runs(&d, &[vec!["z".to_string()]]);
        assert_eq!(
            runs,
            [Run {
                order: vec![0, 1, 2, 3],
                drop: vec![],
                group_breaks: vec![],
                sorted: false,
            }]
        );
    }

    #[test]
    fn barriers_and_blank_lines_split_runs() {
        let mut d = vec![
            decl("c", "http://c/"),
            decl("b", "http://b/"),
            decl("a", "http://a/"),
            decl("z", "http://z/"),
            decl("y", "http://y/"),
        ];
        // a BASE between b and a
        d[2].barrier_before = true;
        assert_eq!(printed(&d, &[]), ["b c", "a y z"]);
        // a blank line between z and y: a barrier without groups
        d[4].blank_before = true;
        assert_eq!(printed(&d, &[]), ["b c", "a z", "y"]);
        // with groups the formatter owns blank lines, but BASE still ends a run
        assert_eq!(printed(&d, &[&["y", "c"]]), ["c | b", "y | a z"]);
        // the same label on both sides of a barrier is not a conflict
        let d = [
            decl("ex", "http://e/1/"),
            PrefixDecl {
                barrier_before: true,
                ..decl("ex", "http://e/2/")
            },
        ];
        assert!(plan_runs(&d, &[]).iter().all(|r| r.sorted));
    }

    #[test]
    fn prefix_groups_goimports_style() {
        let d = [
            decl("foaf", "http://xmlns.com/foaf/0.1/"),
            decl("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
            decl("ex", "http://example.org/"),
            decl("xsd", "http://www.w3.org/2001/XMLSchema#"),
        ];
        // the T6 example: groups sorted inside, unlisted labels last, absent ones skipped
        assert_eq!(
            printed(&d, &[&["rdf", "rdfs", "xsd", "owl"]]),
            ["rdf xsd | ex foaf"]
        );
        // groups in configured order; labels sorted whatever the configured order
        assert_eq!(
            printed(&d, &[&["xsd", "foaf"], &["rdf"]]),
            ["foaf xsd | rdf | ex"]
        );
        // empty groups are skipped: no break for them
        assert_eq!(
            printed(&d, &[&["owl"], &["rdf", "xsd", "foaf", "ex"]]),
            ["ex foaf rdf xsd"]
        );
        // the empty label in a group
        let d = [
            decl("ex", "http://e/"),
            decl("", "http://d/"),
            decl("b", "http://b/"),
        ];
        assert_eq!(printed(&d, &[&["", "ex"]]), ["\"\" ex | b"]);
        assert_eq!(printed(&d, &[&["b"]]), ["b | \"\" ex"]);
        let runs = plan_runs(&d, &[vec!["b".to_string()]]);
        assert_eq!(runs[0].group_breaks, [1]);
    }
}
