//! Comparing two SPARQL parses. spargebra makes up blank nodes (`[]`, paths, reifiers)
//! and variables (aggregates) with random names, so the comparison renames both by first
//! occurrence before comparing; everything else (base IRI, dataset, order) must be equal.
//!
//! The comparison is textual: spargebra's SSE rendering and its SPARQL rendering of each
//! parse, with every blank node and every made-up variable renamed in order of first
//! occurrence. String literals and IRIs are copied as they are, so a `?x` or `_:b` inside
//! them is never taken for a term. Numbering by first occurrence is injective, so equal
//! texts mean the parses are equal up to a renaming of exactly those terms.

use super::Algebra;
use std::collections::HashMap;
use std::collections::HashSet;

/// Whether two parses denote the same algebra. `vars` are the variable names the input
/// spells; any other variable was made up by the parser.
pub fn equivalent(a: &Algebra, b: &Algebra, vars: &HashSet<String>) -> bool {
    canonical(a, vars) == canonical(b, vars)
}

/// A text that two equivalent parses share: the SSE and the SPARQL rendering, with blank
/// nodes (`_:b0`, `_:b1` …) and made-up variables (`?{h0}` …, a spelling no SPARQL
/// variable has) renamed by first occurrence.
pub fn canonical(a: &Algebra, vars: &HashSet<String>) -> String {
    let text = match a {
        Algebra::Query(q) => format!("{}\n{q}", q.to_sse()),
        Algebra::Update(u) => format!("{}\n{u}", u.to_sse()),
    };
    rename(&text, vars)
}

/// `text` (an SSE or SPARQL rendering) with its blank nodes and the variables not in
/// `vars` renamed by first occurrence. One numbering covers the whole text.
pub fn rename(text: &str, vars: &HashSet<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blanks: HashMap<&str, usize> = HashMap::new();
    let mut hidden: HashMap<&str, usize> = HashMap::new();
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let end = string_end(text, i);
                out.push_str(&text[i..end]);
                i = end;
            }
            b'<' => {
                let end = iri_end(text, i).unwrap_or(i + 1);
                out.push_str(&text[i..end]);
                i = end;
            }
            b'?' | b'$' => {
                let name = var_name(&text[i + 1..]);
                if name.is_empty() {
                    // a path modifier
                    out.push(b[i] as char);
                    i += 1;
                    continue;
                }
                if vars.contains(name) {
                    out.push('?');
                    out.push_str(name);
                } else {
                    let n = hidden.len();
                    let n = *hidden.entry(name).or_insert(n);
                    out.push_str(&format!("?{{h{n}}}"));
                }
                i += 1 + name.len();
            }
            b'_' if b.get(i + 1) == Some(&b':') => {
                let label = blank_label(&text[i + 2..]);
                if label.is_empty() {
                    out.push('_');
                    i += 1;
                    continue;
                }
                let n = blanks.len();
                let n = *blanks.entry(label).or_insert(n);
                out.push_str(&format!("_:b{n}"));
                i += 2 + label.len();
            }
            _ => {
                let c = text[i..].chars().next().expect("not at the end");
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

/// The end of the `"…"` literal starting at `start` (escapes skipped), or of the text.
fn string_end(text: &str, start: usize) -> usize {
    let b = text.as_bytes();
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    b.len()
}

/// The end of the `<…>` IRI starting at `start`, if it is one: an IRI holds no space and
/// no `<`, which tells it from `<`, `<=` and `<<` with their operands.
fn iri_end(text: &str, start: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'>' if i > start + 1 => return Some(i + 1),
            b'>' | b'<' | b'"' | b' ' | b'\t' | b'\n' | b'\r' => return None,
            _ => i += 1,
        }
    }
    None
}

/// The variable name at the start of `s` (after its sigil).
fn var_name(s: &str) -> &str {
    let end = s
        .char_indices()
        .find(|&(_, c)| !(is_name_char(c) || c == '\u{B7}'))
        .map_or(s.len(), |(i, _)| i);
    &s[..end]
}

/// A letter, digit, `_` or any non-ASCII character but whitespace: the renderings put
/// ASCII punctuation or a space after every term.
fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || (!c.is_ascii() && !c.is_whitespace())
}

/// The blank node label at the start of `s` (after `_:`): a trailing `.` ends the
/// statement, it is not part of the label.
fn blank_label(s: &str) -> &str {
    let end = s
        .char_indices()
        .find(|&(_, c)| !(is_name_char(c) || matches!(c, '-' | '.')))
        .map_or(s.len(), |(i, _)| i);
    s[..end].trim_end_matches('.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{sparql_parse, sparql_reference};
    use crate::lex::{LexMode, lex};
    use crate::sparql::Unit;

    fn vars(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// Whether `a` and `b` parse (as `unit`) to the same algebra, with the variables of
    /// `a`.
    fn same(unit: Unit, a: &str, b: &str) -> bool {
        let r =
            sparql_reference(a, &lex(a, LexMode::Sparql)).unwrap_or_else(|e| panic!("{a}: {e:?}"));
        assert_eq!(r.unit, unit, "{a}");
        let pa = sparql_parse(a, unit, &[]).unwrap();
        let pb = sparql_parse(b, unit, &[]).unwrap_or_else(|e| panic!("{b}: {e:?}"));
        equivalent(&pa, &pb, &r.vars)
    }

    fn same_query(a: &str, b: &str) -> bool {
        same(Unit::Query, a, b)
    }

    fn same_update(a: &str, b: &str) -> bool {
        same(Unit::Update, a, b)
    }

    #[test]
    fn renames_blank_nodes_and_hidden_variables() {
        let v = vars(&["x"]);
        assert_eq!(
            rename("(bgp (triple _:abc <p> ?x) (triple _:f00 <q> _:abc))", &v),
            "(bgp (triple _:b0 <p> ?x) (triple _:b1 <q> _:b0))"
        );
        assert_eq!(
            rename(
                "(group () ((?7f3a (count))) (extend ((?c ?7f3a)) ?x ?7f3a))",
                &v
            ),
            "(group () ((?{h0} (count))) (extend ((?{h1} ?{h0})) ?x ?{h0}))"
        );
        // `$` is the same sigil
        assert_eq!(rename("$x $y ?y", &v), "?x ?{h0} ?{h0}");
        // terms inside strings and IRIs are not terms
        assert_eq!(
            rename(r#""?a _:b \" ?c" <http://e/?q=_:d> ?a"#, &v),
            r#""?a _:b \" ?c" <http://e/?q=_:d> ?{h0}"#
        );
        // comparisons, triple terms and path modifiers are not IRIs or variables
        assert_eq!(
            rename("(?x < ?y) (<= ?x 1) <<( _:a <p> ?x )>> (<p>)? (<q>)*", &v),
            "(?x < ?{h0}) (<= ?x 1) <<( _:b0 <p> ?x )>> (<p>)? (<q>)*"
        );
        assert_eq!(rename("_:a . _:a.", &v), "_:b0 . _:b0.");
    }

    #[test]
    fn random_names_compare_equal() {
        // each parse makes up new blank nodes and variables
        for q in [
            "SELECT * WHERE { [] <http://e/p> [ <http://e/q> ?o ] }",
            "SELECT * WHERE { ?s <http://e/p>/<http://e/q>* ?o }",
            "SELECT * WHERE { ?s ^<http://e/p>|!(<http://e/q>|^<http://e/r>) ?o }",
            "SELECT (COUNT(*) AS ?c) (SAMPLE(?o) AS ?any) WHERE { ?s ?p ?o } GROUP BY ?s HAVING (SUM(?o) > 1)",
            "SELECT ?s WHERE { ?s ?p ?o } GROUP BY ?s ORDER BY DESC(COUNT(?o))",
            "SELECT * WHERE { ?s <http://e/p> ( 1 2 [] ) }",
            "SELECT * WHERE { << ?s <http://e/p> ?o >> <http://e/q> ?z . ?a <http://e/b> ?c ~ {| <http://e/d> 1 |} }",
            "CONSTRUCT { [] <http://e/p> ?o } WHERE { _:x <http://e/p> ?o }",
            "SELECT * WHERE { ?s <http://e/p> \"_:not ?a <term>\" }",
        ] {
            assert!(same_query(q, q), "{q}");
        }
        for u in [
            "INSERT DATA { [] <http://e/p> 1 }",
            "DELETE { ?s <http://e/p> ?o } INSERT { [] <http://e/q> ?o } WHERE { ?s <http://e/p>+ ?o }",
            "ADD <http://e/a> TO <http://e/b>",
        ] {
            assert!(same_update(u, u), "{u}");
        }
        // user blank node labels are local
        assert!(same_query(
            "SELECT * WHERE { _:a <http://e/p> _:b . _:b <http://e/q> _:a }",
            "SELECT * WHERE { _:x <http://e/p> _:y . _:y <http://e/q> _:x }"
        ));
        // layout and the sigil are not part of the algebra
        assert!(same_query(
            "SELECT $s WHERE{?s<http://e/p>?v FILTER(?v+1>2)}",
            "SELECT ?s\nWHERE {\n  ?s <http://e/p> ?v .\n  FILTER(?v + 1 > 2)\n}\n"
        ));
    }

    #[test]
    fn different_algebras_differ() {
        let pairs: &[(&str, &str)] = &[
            (
                "SELECT * FROM <http://e/g> WHERE { ?s ?p ?o }",
                "SELECT * FROM NAMED <http://e/g> WHERE { ?s ?p ?o }",
            ),
            (
                "SELECT * WHERE { ?s ?p ?o } ORDER BY ASC(?s)",
                "SELECT * WHERE { ?s ?p ?o } ORDER BY DESC(?s)",
            ),
            (
                "SELECT DISTINCT ?s WHERE { ?s ?p ?o }",
                "SELECT REDUCED ?s WHERE { ?s ?p ?o }",
            ),
            (
                "SELECT ?s WHERE { ?s ?p ?o }",
                "SELECT DISTINCT ?s WHERE { ?s ?p ?o }",
            ),
            (
                "SELECT * WHERE { ?s ?p ?o } LIMIT 10",
                "SELECT * WHERE { ?s ?p ?o } LIMIT 11",
            ),
            (
                "SELECT * WHERE { ?s ?p ?o } LIMIT 10 OFFSET 2",
                "SELECT * WHERE { ?s ?p ?o } LIMIT 10 OFFSET 3",
            ),
            (
                "SELECT * WHERE { ?s ?p ?o } OFFSET 10",
                "SELECT * WHERE { ?s ?p ?o } LIMIT 10",
            ),
            (
                "SELECT * WHERE { SERVICE SILENT <http://e/s> { ?s ?p ?o } }",
                "SELECT * WHERE { SERVICE <http://e/s> { ?s ?p ?o } }",
            ),
            (
                "SELECT ?a ?b WHERE { ?a ?p ?b }",
                "SELECT ?b ?a WHERE { ?a ?p ?b }",
            ),
            (
                "SELECT * WHERE { ?s ?p ?o OPTIONAL { ?s ?q ?a } OPTIONAL { ?s ?r ?b } }",
                "SELECT * WHERE { ?s ?p ?o OPTIONAL { ?s ?r ?b } OPTIONAL { ?s ?q ?a } }",
            ),
            (
                "SELECT * WHERE { { ?s ?p 1 } UNION { ?s ?p 2 } }",
                "SELECT * WHERE { { ?s ?p 2 } UNION { ?s ?p 1 } }",
            ),
            (
                "SELECT * WHERE { ?s ?p ?o } VALUES ?o { 1 2 }",
                "SELECT * WHERE { ?s ?p ?o } VALUES ?o { 2 1 }",
            ),
            (
                "SELECT * WHERE { ?s ?p \"01\"^^<http://www.w3.org/2001/XMLSchema#integer> }",
                "SELECT * WHERE { ?s ?p 1 }",
            ),
            (
                "SELECT * WHERE { ?s ?p \"a\"@en }",
                "SELECT * WHERE { ?s ?p \"a\"@fr }",
            ),
            (
                "SELECT * WHERE { ?s ?p \"a ?x\" }",
                "SELECT * WHERE { ?s ?p \"a ?y\" }",
            ),
            (
                "SELECT * WHERE { ?s <http://e/p?a> ?o }",
                "SELECT * WHERE { ?s <http://e/p?b> ?o }",
            ),
            (
                "SELECT * WHERE { ?s ?p ?o FILTER(?o > 1) }",
                "SELECT * WHERE { ?s ?p ?o FILTER(?o >= 1) }",
            ),
            (
                "SELECT * WHERE { ?s ?p ?o FILTER(?o - 1 - 2 = 0) }",
                "SELECT * WHERE { ?s ?p ?o FILTER(?o - (1 - 2) = 0) }",
            ),
            (
                "SELECT (GROUP_CONCAT(?o; SEPARATOR=\",\") AS ?g) WHERE { ?s ?p ?o }",
                "SELECT (GROUP_CONCAT(?o; SEPARATOR=\";\") AS ?g) WHERE { ?s ?p ?o }",
            ),
            (
                "SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { ?s ?p ?o }",
                "SELECT (COUNT(?o) AS ?c) WHERE { ?s ?p ?o }",
            ),
            (
                "BASE <http://e/a/> SELECT * WHERE { ?s <p> ?o }",
                "BASE <http://e/b/> SELECT * WHERE { ?s <p> ?o }",
            ),
            (
                "SELECT * WHERE { ?s <http://e/p>* ?o }",
                "SELECT * WHERE { ?s <http://e/p>+ ?o }",
            ),
            (
                "SELECT * WHERE { _:a <http://e/p> _:a }",
                "SELECT * WHERE { _:a <http://e/p> _:b }",
            ),
            (
                "SELECT * WHERE { [] <http://e/p> [] }",
                "SELECT * WHERE { _:a <http://e/p> _:a }",
            ),
            (
                "SELECT * WHERE { ?s ?p ?o MINUS { ?s ?q ?o } }",
                "SELECT * WHERE { ?s ?p ?o FILTER NOT EXISTS { ?s ?q ?o } }",
            ),
            ("ASK { ?s ?p ?o }", "SELECT * WHERE { ?s ?p ?o }"),
            ("DESCRIBE <http://e/a>", "DESCRIBE <http://e/b>"),
        ];
        for (a, b) in pairs {
            assert!(!same_query(a, b), "{a}\n  and\n{b}");
            assert!(!same_query(b, a), "{b}\n  and\n{a}");
        }
        // (spargebra rewrites ADD, MOVE and COPY into other operations and drops their
        // SILENT, so that one difference is invisible to the check; the printers keep it)
        let updates: &[(&str, &str)] = &[
            ("LOAD SILENT <http://e/d>", "LOAD <http://e/d>"),
            (
                "CLEAR SILENT GRAPH <http://e/g>",
                "CLEAR GRAPH <http://e/g>",
            ),
            ("DROP SILENT ALL", "DROP ALL"),
            ("DROP NAMED", "DROP DEFAULT"),
            (
                "CREATE SILENT GRAPH <http://e/g>",
                "CREATE GRAPH <http://e/g>",
            ),
            (
                "LOAD <http://e/d> INTO GRAPH <http://e/g>",
                "LOAD <http://e/d> INTO GRAPH <http://e/h>",
            ),
            (
                "MOVE <http://e/a> TO <http://e/b>",
                "COPY <http://e/a> TO <http://e/b>",
            ),
            (
                "WITH <http://e/g> DELETE { ?s ?p ?o } WHERE { ?s ?p ?o }",
                "DELETE { ?s ?p ?o } WHERE { ?s ?p ?o }",
            ),
            (
                "DELETE { ?s ?p ?o } USING <http://e/g> WHERE { ?s ?p ?o }",
                "DELETE { ?s ?p ?o } USING NAMED <http://e/g> WHERE { ?s ?p ?o }",
            ),
            (
                "INSERT DATA { <http://e/a> <http://e/b> 1 } ; CLEAR ALL",
                "CLEAR ALL ; INSERT DATA { <http://e/a> <http://e/b> 1 }",
            ),
            (
                "INSERT DATA { <http://e/a> <http://e/b> 1 }",
                "DELETE DATA { <http://e/a> <http://e/b> 1 }",
            ),
        ];
        for (a, b) in updates {
            assert!(!same_update(a, b), "{a}\n  and\n{b}");
            assert!(!same_update(b, a), "{b}\n  and\n{a}");
        }
    }

    #[test]
    fn hidden_variables_are_not_user_variables() {
        // an aggregate's made-up variable never matches a variable the input spells
        assert!(!same_query(
            "SELECT (COUNT(?o) AS ?c) WHERE { ?s ?p ?o }",
            "SELECT (COUNT(?o) AS ?d) WHERE { ?s ?p ?o }",
        ));
        // a renamed variable that the input does not spell is still a difference when the
        // input spells the original
        assert!(!same_query(
            "SELECT ?x WHERE { ?x ?p ?o }",
            "SELECT ?y WHERE { ?y ?p ?o }",
        ));
    }
}
