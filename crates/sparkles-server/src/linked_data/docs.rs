//! Self-contained ontology documentation over authorized, asserted facts.

use super::html::{HtmlWriter, percent_encode_param};
use super::publication::{Fact, Field, Term, Terms};
use base64::Engine as _;
use sha2::{Digest, Sha256};
use sparkles::task::Control;

const SECTIONS: &[(Field, &str)] = &[
    (Field::Definition, "Definitions"),
    (Field::Synonym, "Synonyms"),
    (Field::Parent, "Hierarchy"),
    (Field::Equivalent, "Equivalent terms"),
    (Field::Usage, "Usage"),
    (Field::Example, "Examples"),
    (Field::Type, "Types"),
    (Field::Identifier, "Identifiers"),
    (Field::Metadata, "Ontology metadata"),
];

pub(super) fn render(
    w: &mut HtmlWriter<'_>,
    control: &Control,
    title: &str,
    terms: &Terms,
    focus: Option<&str>,
    download: bool,
) -> sparkles::Result<()> {
    w.push_str("<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\n        <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>")?;
    w.push_escaped(title)?;
    w.push_str("</title><style>        body{font:1rem/1.55 system-ui,sans-serif;color:#202630;max-width:68rem;margin:2rem auto;padding:0 1rem}        a{color:#1957a5}code{overflow-wrap:anywhere}h2,h3{color:#263951}        article{border-top:1px solid #cdd7e4;padding:1rem 0}dt{font-weight:bold}        </style></head><body>\n<header><h1>")?;
    w.push_escaped(title)?;
    w.push_str("</h1>")?;
    w.push_str("</header><main>\n")?;

    if let Some(iri) = focus {
        let links = if download { Links::None } else { Links::Term };
        term_section(w, control, terms, iri, &terms.entries[iri], links)?;
        write_children(w, control, terms, iri, links)?;
    } else {
        let mut labelled = 0usize;
        let mut defined = 0usize;
        for term in terms.entries.values() {
            control.check()?;
            labelled += usize::from(term.values(Field::Label).any(|fact| !fact.iri));
            defined += usize::from(term.values(Field::Definition).any(|fact| !fact.iri));
        }
        w.push_str("<section><h2>Annotation coverage</h2><dl><dt>Documented terms</dt><dd>")?;
        w.push_str(&terms.entries.len().to_string())?;
        w.push_str("</dd><dt>With labels</dt><dd>")?;
        w.push_str(&labelled.to_string())?;
        w.push_str("</dd><dt>With definitions</dt><dd>")?;
        w.push_str(&defined.to_string())?;
        w.push_str("</dd></dl></section>\n<nav aria-label=\"Term index\"><h2>Term index</h2><ul>")?;
        for (iri, term) in &terms.entries {
            control.check()?;
            w.push_str("<li>")?;
            term_link(w, iri, term.label(iri, None), Links::Anchor)?;
            w.push_str("</li>")?;
        }
        w.push_str("</ul></nav>\n")?;
        for (iri, term) in terms.entries.iter().filter(|(_, term)| term.is_ontology()) {
            term_section(w, control, terms, iri, term, Links::Anchor)?;
        }
        for (iri, term) in terms.entries.iter().filter(|(_, term)| !term.is_ontology()) {
            term_section(w, control, terms, iri, term, Links::Anchor)?;
        }
    }
    w.push_str("</main></body></html>\n")?;
    Ok(())
}

#[derive(Clone, Copy)]
enum Links {
    Anchor,
    Term,
    None,
}

fn term_link(w: &mut HtmlWriter<'_>, iri: &str, label: &str, links: Links) -> sparkles::Result<()> {
    if matches!(links, Links::None) {
        return w.push_escaped(label);
    }
    w.push_str("<a href=\"")?;
    match links {
        Links::Anchor => {
            w.push_str("#")?;
            w.push_str(&anchor(iri))?;
        }
        Links::Term => {
            w.push_str("?iri=")?;
            w.push_escaped(&percent_encode_param(iri))?;
        }
        Links::None => unreachable!("returned above"),
    }
    w.push_str("\">")?;
    w.push_escaped(label)?;
    w.push_str("</a>")
}

fn anchor(iri: &str) -> String {
    format!(
        "term-{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(iri))
    )
}

fn write_fact(
    w: &mut HtmlWriter<'_>,
    terms: &Terms,
    fact: &Fact,
    links: Links,
) -> sparkles::Result<()> {
    if fact.iri
        && let Some(term) = terms.entries.get(&fact.value)
    {
        term_link(w, &fact.value, term.label(&fact.value, None), links)?;
    } else {
        w.push_escaped(&fact.value)?;
    }
    if let Some(language) = &fact.language {
        w.push_str(" <small>(")?;
        w.push_escaped(language)?;
        w.push_str(")</small>")?;
    }
    Ok(())
}

fn term_section(
    w: &mut HtmlWriter<'_>,
    control: &Control,
    terms: &Terms,
    iri: &str,
    term: &Term,
    links: Links,
) -> sparkles::Result<()> {
    control.check()?;
    w.push_str("<article id=\"")?;
    w.push_str(&anchor(iri))?;
    w.push_str("\"><h2>")?;
    w.push_escaped(term.label(iri, None))?;
    w.push_str("</h2><p><code>")?;
    w.push_escaped(iri)?;
    w.push_str("</code></p>")?;
    for &(field, title) in SECTIONS {
        let mut facts = term.values(field).peekable();
        if facts.peek().is_none() {
            continue;
        }
        w.push_str("<section><h3>")?;
        w.push_str(title)?;
        w.push_str("</h3><ul>")?;
        for fact in facts {
            control.check()?;
            w.push_str("<li>")?;
            if fact.iri || field == Field::Metadata {
                w.push_str("<code>")?;
                w.push_escaped(super::publication::local_name(fact.predicate))?;
                w.push_str("</code> ")?;
            }
            write_fact(w, terms, fact, links)?;
            w.push_str("</li>")?;
        }
        w.push_str("</ul></section>")?;
    }
    w.push_str("</article>\n")
}

fn write_children(
    w: &mut HtmlWriter<'_>,
    control: &Control,
    terms: &Terms,
    iri: &str,
    links: Links,
) -> sparkles::Result<()> {
    let mut started = false;
    for child in terms.children(iri) {
        control.check()?;
        let term = &terms.entries[child];
        if !started {
            w.push_str("<section><h2>Children</h2><ul>")?;
            started = true;
        }
        w.push_str("<li>")?;
        term_link(w, child, term.label(child, None), links)?;
        w.push_str("</li>")?;
    }
    if started {
        w.push_str("</ul></section>")?;
    }
    Ok(())
}
