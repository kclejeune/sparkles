//! Completion of prefixes (spec C20 §8): prefix declarations after `PREFIX` or `@prefix`,
//! and prefix names where a prefixed name starts, with the declaration a completed name
//! needs. Everything here works on byte offsets; the caller turns them into positions.

use sparkles_fmt::Language;
use std::collections::{BTreeMap, BTreeSet};

/// A replacement of bytes `start..end` with `insert`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub insert: String,
}

/// One completion: the edit at the cursor and, when the document lacks the prefix's
/// declaration, the edit that adds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub iri: String,
    pub edit: Edit,
    pub declare: Option<Edit>,
}

/// Whether completion of prefixes applies to `lang`.
pub fn completes(lang: Language) -> bool {
    matches!(lang, Language::Sparql | Language::Turtle | Language::TriG)
}

/// The completions at byte `at` of `text`, from the `known` prefixes.
pub fn complete(
    text: &str,
    at: usize,
    lang: Language,
    known: &BTreeMap<String, String>,
) -> Vec<Item> {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    let line_start = text[..at].rfind(['\n', '\r']).map_or(0, |i| i + 1);
    let before = &text[line_start..at];
    if !in_code(before) {
        return Vec::new();
    }
    let word_len = before.len()
        - before
            .trim_end_matches(|c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            .len();
    let word_start = at - word_len;
    let word = &text[word_start..at];
    let head = &before[..before.len() - word_len];
    let declared = declared(text);
    if let Some(turtle) = declaration_keyword(head) {
        // `PREFIX |` or `@prefix |`: whole declarations of the prefixes not declared yet
        return known
            .iter()
            .filter(|(name, _)| !declared.contains(name.as_str()))
            .map(|(name, iri)| Item {
                label: format!("{name}:"),
                iri: iri.clone(),
                edit: Edit {
                    start: word_start,
                    end: at,
                    insert: if turtle {
                        format!("{name}: <{iri}> .")
                    } else {
                        format!("{name}: <{iri}>")
                    },
                },
                declare: None,
            })
            .collect();
    }
    // a prefixed name starts with a letter, after something that is not part of a term
    if !word.starts_with(|c: char| c.is_ascii_alphabetic())
        || head.ends_with(['?', '$', ':', '_', '@', '^'])
    {
        return Vec::new();
    }
    // the declaration goes after the prologue, and offsets after it shift by its length
    let style = Style::of(text, lang);
    let insert_at = prologue_end(text);
    known
        .iter()
        .map(|(name, iri)| Item {
            label: format!("{name}:"),
            iri: iri.clone(),
            edit: Edit {
                start: word_start,
                end: at,
                insert: format!("{name}:"),
            },
            declare: (!declared.contains(name.as_str()))
                .then(|| declaration(text, insert_at, style, name, iri)),
        })
        .collect()
}

/// The edit that declares `name` for the lint finding `undefined-prefix`, or `None` when
/// the prefix is not known or already declared.
pub fn declare(
    text: &str,
    lang: Language,
    name: &str,
    known: &BTreeMap<String, String>,
) -> Option<Edit> {
    let iri = known.get(name)?;
    if declared(text).contains(name) {
        return None;
    }
    Some(declaration(
        text,
        prologue_end(text),
        Style::of(text, lang),
        name,
        iri,
    ))
}

/// How a document writes its declarations.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Style {
    /// `PREFIX name: <iri>`
    Sparql,
    /// `@prefix name: <iri> .`
    Turtle,
}

impl Style {
    /// `@prefix` in a Turtle or TriG document that uses it, else `PREFIX`.
    fn of(text: &str, lang: Language) -> Style {
        let uses_at = lines(text).any(|l| {
            l.trim_start()
                .get(..7)
                .is_some_and(|k| k.eq_ignore_ascii_case("@prefix"))
        });
        if matches!(lang, Language::Turtle | Language::TriG) && uses_at {
            Style::Turtle
        } else {
            Style::Sparql
        }
    }
}

fn declaration(text: &str, at: usize, style: Style, name: &str, iri: &str) -> Edit {
    let line = match style {
        Style::Sparql => format!("PREFIX {name}: <{iri}>"),
        Style::Turtle => format!("@prefix {name}: <{iri}> ."),
    };
    // at the end of a last line without a line break, the break goes first
    let insert = if at > 0 && !text[..at].ends_with(['\n', '\r']) {
        format!("\n{line}\n")
    } else {
        format!("{line}\n")
    };
    Edit {
        start: at,
        end: at,
        insert,
    }
}

/// Whether the end of `before`, the line up to the cursor, is outside comments, strings
/// and IRIs. An IRI is a `<` with no space or `>` after it, which tells it from the
/// less-than operator.
fn in_code(before: &str) -> bool {
    let mut quote: Option<char> = None;
    let mut iri = false;
    let mut chars = before.chars();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == '\\' {
                chars.next();
            } else if c == q {
                quote = None;
            }
            continue;
        }
        if iri {
            if c == '>' || c.is_whitespace() {
                iri = false;
            }
            continue;
        }
        match c {
            '#' => return false,
            '"' | '\'' => quote = Some(c),
            '<' => iri = true,
            _ => {}
        }
    }
    quote.is_none() && !iri
}

/// `Some(turtle)` when `head` ends with `PREFIX` or `@prefix` and spaces, where
/// `turtle` says whether it is `@prefix`.
fn declaration_keyword(head: &str) -> Option<bool> {
    let t = head.trim_end();
    if t.len() == head.len() {
        return None;
    }
    let start = t
        .rfind(|c: char| c.is_whitespace() || matches!(c, '{' | '}' | ';' | '.'))
        .map_or(0, |i| i + 1);
    let k = &t[start..];
    if k.eq_ignore_ascii_case("prefix") {
        Some(false)
    } else if k.eq_ignore_ascii_case("@prefix") {
        Some(true)
    } else {
        None
    }
}

/// The lines of `text`, without their breaks.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split(['\n', '\r'])
}

/// The names that `text` declares, by `PREFIX` or `@prefix`.
pub fn declared(text: &str) -> BTreeSet<&str> {
    let mut out = BTreeSet::new();
    let lower = text.to_ascii_lowercase();
    let b = text.as_bytes();
    let mut i = 0;
    while let Some(j) = find_keyword(&lower, i) {
        // the keyword ends at j, then come spaces, the name and `:`
        let rest = &text[j..];
        let name_start = j + (rest.len() - rest.trim_start().len());
        let mut k = name_start;
        while k < b.len() && (b[k].is_ascii_alphanumeric() || matches!(b[k], b'_' | b'-' | b'.')) {
            k += 1;
        }
        if b.get(k) == Some(&b':') {
            out.insert(&text[name_start..k]);
        }
        i = j;
    }
    out
}

/// The end of the next `prefix` keyword of `lower`, the document in lower case, at or
/// after `from`, which stands as a word (`@prefix` included) outside a comment.
fn find_keyword(lower: &str, from: usize) -> Option<usize> {
    let mut i = from;
    while let Some(p) = lower[i..].find("prefix") {
        let s = i + p;
        let e = s + 6;
        i = e;
        let before = lower[..s].chars().next_back();
        let after = lower[e..].chars().next();
        let word_before =
            matches!(before, Some(c) if c.is_ascii_alphanumeric() || c == '_' || c == ':');
        if word_before || !matches!(after, Some(c) if c.is_whitespace()) {
            continue;
        }
        let line_start = lower[..s].rfind(['\n', '\r']).map_or(0, |x| x + 1);
        if lower[line_start..s].contains('#') {
            continue;
        }
        return Some(e);
    }
    None
}

/// The byte offset after the document's prologue: after its last line that declares a
/// prefix or a base among the leading lines of declarations, blanks and comments, else
/// the start of the document.
pub fn prologue_end(text: &str) -> usize {
    let mut end = 0;
    let mut pos = 0;
    while pos < text.len() {
        let rest = &text[pos..];
        let len = rest.find(['\n', '\r']).unwrap_or(rest.len());
        let mut next = pos + len;
        if text[next..].starts_with("\r\n") {
            next += 2;
        } else if next < text.len() {
            next += 1;
        }
        let line = rest[..len].trim_start();
        let keyword = line
            .split(|c: char| c.is_whitespace())
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(keyword.as_str(), "prefix" | "base" | "@prefix" | "@base") {
            end = next;
        } else if !(line.is_empty() || line.starts_with('#')) {
            break;
        }
        pos = next;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> BTreeMap<String, String> {
        [
            ("kclj", "https://kclj.io/sparkles/"),
            ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
    }

    fn at(text: &str) -> (String, usize) {
        let i = text.find('|').unwrap();
        (text.replace('|', ""), i)
    }

    fn apply(text: &str, item: &Item) -> String {
        let mut out = text.to_string();
        let mut edits = vec![item.edit.clone()];
        edits.extend(item.declare.clone());
        edits.sort_by_key(|e| std::cmp::Reverse(e.start));
        for e in edits {
            out.replace_range(e.start..e.end, &e.insert);
        }
        out
    }

    fn pick<'a>(items: &'a [Item], label: &str) -> &'a Item {
        items.iter().find(|i| i.label == label).unwrap()
    }

    #[test]
    fn a_prefixed_name_gets_its_declaration() {
        let (t, i) = at(
            "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>\nSELECT * { ?s kc| ?o }\n",
        );
        let items = complete(&t, i, Language::Sparql, &known());
        assert_eq!(
            apply(&t, pick(&items, "kclj:")),
            "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>\nPREFIX kclj: <https://kclj.io/sparkles/>\nSELECT * { ?s kclj: ?o }\n"
        );
        // a declared prefix inserts nothing
        assert_eq!(pick(&items, "rdf:").declare, None);
        // no prologue: at the top
        let (t, i) = at("SELECT * { ?s k| ?o }");
        let items = complete(&t, i, Language::Sparql, &known());
        assert_eq!(
            apply(&t, pick(&items, "kclj:")),
            "PREFIX kclj: <https://kclj.io/sparkles/>\nSELECT * { ?s kclj: ?o }"
        );
    }

    #[test]
    fn declarations_after_the_keyword() {
        let (t, i) = at("PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>\nPREFIX |");
        let items = complete(&t, i, Language::Sparql, &known());
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(
            apply(&t, &items[0]),
            "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>\nPREFIX kclj: <https://kclj.io/sparkles/>"
        );
        let (t, i) = at("@prefix kc|");
        let items = complete(&t, i, Language::Turtle, &known());
        assert_eq!(
            apply(&t, pick(&items, "kclj:")),
            "@prefix kclj: <https://kclj.io/sparkles/> ."
        );
    }

    #[test]
    fn turtle_keeps_its_style() {
        let (t, i) =
            at("@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\n<a> kc| <b> .\n");
        let items = complete(&t, i, Language::Turtle, &known());
        assert_eq!(
            apply(&t, pick(&items, "kclj:")),
            "@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n@prefix kclj: <https://kclj.io/sparkles/> .\n\n<a> kclj: <b> .\n"
        );
        // Turtle without `@prefix` gets the SPARQL form
        let (t, i) = at("<a> kc| <b> .");
        let items = complete(&t, i, Language::Turtle, &known());
        assert!(
            pick(&items, "kclj:")
                .declare
                .as_ref()
                .unwrap()
                .insert
                .starts_with("PREFIX ")
        );
    }

    #[test]
    fn nothing_inside_terms_comments_and_strings() {
        for q in [
            "SELECT ?k| {}",
            "SELECT * { <http://ex/k| }",
            "SELECT * { ?s ?p \"k|",
            "# k|",
            "SELECT * { ?s rdf:ty| }",
            "SELECT * { ?s ?p 1 }|",
            "SELECT * { ?s ?p \"x\"@e| }",
        ] {
            let (t, i) = at(q);
            assert!(
                complete(&t, i, Language::Sparql, &known()).is_empty(),
                "{q}"
            );
        }
        // less-than is not an IRI
        let (t, i) = at("SELECT * { ?s ?p ?o FILTER(?o < k| }");
        assert!(!complete(&t, i, Language::Sparql, &known()).is_empty());
    }

    #[test]
    fn declared_names_and_the_prologue() {
        let t = "# header\nprefix a: <x:>\n@prefix b.c: <y:> .\nPREFIX : <z:>\nBASE <q:>\nSELECT * { ?s a:prefix ?o } # PREFIX d: <w:>\n";
        assert_eq!(declared(t), ["", "a", "b.c"].into_iter().collect());
        assert_eq!(
            &t[prologue_end(t)..],
            "SELECT * { ?s a:prefix ?o } # PREFIX d: <w:>\n"
        );
        assert_eq!(prologue_end("SELECT"), 0);
        // a last declaration without a line break
        let t = "PREFIX a: <x:>";
        let e = declaration(t, prologue_end(t), Style::Sparql, "b", "y:");
        assert_eq!(e.insert, "\nPREFIX b: <y:>\n");
        assert_eq!(
            declare(
                "SELECT * { ?s kclj:x ?o }",
                Language::Sparql,
                "kclj",
                &known()
            )
            .unwrap()
            .insert,
            "PREFIX kclj: <https://kclj.io/sparkles/>\n"
        );
        assert_eq!(
            declare("SELECT * {}", Language::Sparql, "nope", &known()),
            None
        );
    }
}
