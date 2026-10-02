//! Full-text search with per-language analyzers (`languages` in `text.json`), and the
//! rank output of `text:query`.
#![cfg(feature = "text")]

use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::{QueryOptions, query};
use sparkles::store::{Store, StoreOptions};
use sparkles::text::{Analyzer, Languages, TextConfig};

const DATA: &str = r#"
@prefix ex: <http://example.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:a rdfs:label "Running quickly"@en .
ex:b rdfs:label "He runs home"@en-GB .
ex:c rdfs:label "The runner"@en .
ex:d rdfs:label "running"@en ; rdfs:comment "running" .
ex:e rdfs:label "Ada and the fox"@en .
ex:f rdfs:label "Les chevaux du roi"@fr .
ex:g rdfs:label "Un cheval"@fr .
ex:h rdfs:label "Die Häuser"@de .
ex:i rdfs:label "Running water"@nl .
"#;

const P: &str = "PREFIX ex: <http://example.org/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> PREFIX text: <http://jena.apache.org/text#> ";

fn load(s: &Store) {
    s.load(&[Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
}

fn langs(tags: &[&str]) -> TextConfig {
    TextConfig {
        languages: Languages::from_tags(tags).unwrap(),
        ..Default::default()
    }
}

fn mem(cfg: TextConfig) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    s.enable_text(cfg).unwrap();
    s
}

fn rows(s: &Store, q: &str) -> Vec<String> {
    let r = query(s.snapshot(), &format!("{P}{q}"), &QueryOptions::default())
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    let mut v: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| {
                    t.map_or("-".into(), |t| match t {
                        oxrdf::Term::NamedNode(n) => {
                            n.as_str().rsplit('/').next().unwrap().to_string()
                        }
                        oxrdf::Term::Literal(l) => l.value().to_string(),
                        t => t.to_string(),
                    })
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    v.sort();
    v
}

/// The subjects a search for `q` finds (sorted).
fn hits(s: &Store, q: &str) -> Vec<String> {
    rows(s, &format!("SELECT ?s {{ ?s text:query ({q}) }}"))
}

fn err(s: &Store, q: &str) -> String {
    query(s.snapshot(), &format!("{P}{q}"), &QueryOptions::default())
        .err()
        .unwrap_or_else(|| panic!("{q}: no error"))
        .to_string()
}

#[test]
fn a_language_search_is_stemmed_and_the_plain_one_is_not() {
    let s = mem(langs(&["en", "fr", "de"]));
    // without a language: the standard analyzer, no stemming
    assert_eq!(hits(&s, "rdfs:label \"run\""), Vec::<String>::new());
    assert_eq!(hits(&s, "rdfs:label \"running\""), ["a", "d", "i"]);
    // lang:en: stemmed on both sides, and en matches en-GB
    assert_eq!(hits(&s, "rdfs:label \"run\" \"lang:en\""), ["a", "b", "d"]);
    assert_eq!(
        hits(&s, "rdfs:label \"running\" \"lang:en\""),
        ["a", "b", "d"]
    );
    // the query string's language tag acts as lang:
    assert_eq!(hits(&s, "rdfs:label \"runs\"@en"), ["a", "b", "d"]);
    assert_eq!(hits(&s, "rdfs:label \"runs\"@en-gb"), ["b"]);
    // other languages
    assert_eq!(hits(&s, "\"cheval\" \"lang:fr\""), ["f", "g"]);
    assert_eq!(hits(&s, "\"cheval\""), ["g"]);
    assert_eq!(hits(&s, "\"haus\" \"lang:de\""), ["h"]);
    // a German prefix loses its umlauts as the German stems do
    assert_eq!(hits(&s, "\"häu*\" \"lang:de\""), ["h"]);
    // a language without an analyzer in this index: the standard text, filtered
    assert_eq!(hits(&s, "\"running\" \"lang:nl\""), ["i"]);
    assert_eq!(hits(&s, "\"run\" \"lang:nl\""), Vec::<String>::new());
    // another predicate's literal of the same language stays out
    assert_eq!(
        hits(&s, "rdfs:comment \"run\" \"lang:en\""),
        Vec::<String>::new()
    );
}

#[test]
fn lucene_forms_on_the_stemmed_text() {
    let s = mem(langs(&["en"]));
    // prefixes, wildcards, fuzzy words, regular expressions and ranges are not stemmed:
    // they match the stemmed terms, as in Lucene ("running" was indexed as "run")
    assert_eq!(hits(&s, "\"runn*\" \"lang:en\""), ["c"]);
    assert_eq!(hits(&s, "\"run*\" \"lang:en\""), ["a", "b", "c", "d"]);
    assert_eq!(hits(&s, "\"r?n\" \"lang:en\""), ["a", "b", "d"]);
    assert_eq!(hits(&s, "\"rnu~1\" \"lang:en\""), ["a", "b", "d"]);
    assert_eq!(hits(&s, "\"/ru.+r/\" \"lang:en\""), ["c"]);
    assert_eq!(hits(&s, "\"[quick TO quickz]\" \"lang:en\""), ["a"]);
    // stop words are dropped from the query, and phrases keep their gaps
    assert_eq!(hits(&s, "\"the runner\" \"lang:en\""), ["c"]);
    assert_eq!(hits(&s, "\"+the +runner\" \"lang:en\""), ["c"]);
    assert_eq!(hits(&s, "\"\\\"ada and the fox\\\"\" \"lang:en\""), ["e"]);
    assert_eq!(
        hits(&s, "\"\\\"ada fox\\\"\" \"lang:en\""),
        Vec::<String>::new()
    );
    assert_eq!(hits(&s, "\"\\\"ada fox\\\"~2\" \"lang:en\""), ["e"]);
    // a query of stop words only has no word to search for
    assert!(
        err(&s, "SELECT ?s { ?s text:query (\"the\" \"lang:en\") }")
            .contains("no word to search for")
    );
    // highlighting marks the words the stemmed query matched
    assert_eq!(
        rows(
            &s,
            "SELECT ?lit { (?s ?sc ?lit) text:query (rdfs:label \"runs\" \"lang:en\" \"highlight:s:[|e:]\") }"
        ),
        ["He [runs] home", "[Running] quickly", "[running]"]
    );
}

#[test]
fn languages_are_configured_per_index() {
    // "all": every language Tantivy has a stemmer for
    let s = mem(TextConfig {
        languages: Languages::All,
        ..Default::default()
    });
    assert_eq!(hits(&s, "\"run\" \"lang:en\""), ["a", "b", "d"]);
    assert_eq!(hits(&s, "\"running\" \"lang:nl\""), ["i"]);
    assert_eq!(hits(&s, "\"haus\" \"lang:de\""), ["h"]);
    // a map names the analyzer of each tag
    let cfg: TextConfig =
        serde_json::from_str(r#"{"languages": {"en": "english", "gl": "Portuguese"}}"#).unwrap();
    let Languages::Only(m) = &cfg.languages else {
        panic!()
    };
    assert_eq!(m.get("gl"), Some(&Analyzer::Portuguese));
    assert_eq!(
        serde_json::to_value(&cfg).unwrap()["languages"],
        serde_json::json!({"en": "english", "gl": "portuguese"})
    );
    for bad in [
        r#"{"languages": "some"}"#,
        r#"{"languages": ["xx"]}"#,
        r#"{"languages": ["en-GB"]}"#,
        r#"{"languages": {"en-GB": "english"}}"#,
        r#"{"languages": {"en": "klingon"}}"#,
    ] {
        assert!(serde_json::from_str::<TextConfig>(bad).is_err(), "{bad}");
    }
    let cfg: TextConfig = serde_json::from_str(r#"{"languages": ["EN", "nb"]}"#).unwrap();
    assert_eq!(cfg.languages.resolve().len(), 2);
    // no languages: the configuration serializes as before (its hash is unchanged)
    assert!(
        !serde_json::to_string(&TextConfig::default())
            .unwrap()
            .contains("languages")
    );
    // changing the languages rebuilds: a store without them stems nothing
    let s = mem(TextConfig::default());
    assert_eq!(hits(&s, "\"run\" \"lang:en\""), Vec::<String>::new());
    s.enable_text(langs(&["en"])).unwrap();
    assert_eq!(hits(&s, "\"run\" \"lang:en\""), ["a", "b", "d"]);
}

#[test]
fn stemmed_fields_persist_and_follow_updates() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("db");
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        load(&s);
        s.enable_text(langs(&["en"])).unwrap();
        assert_eq!(hits(&s, "\"run\" \"lang:en\""), ["a", "b", "d"]);
    }
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    let status = s.text_status().unwrap();
    assert_eq!(status.state, "ready");
    assert_eq!(
        status.config.languages,
        Languages::from_tags(&["en"]).unwrap()
    );
    assert_eq!(hits(&s, "\"run\" \"lang:en\""), ["a", "b", "d"]);
    let o = QueryOptions::default();
    sparkles::sparql::update::update(
        &s,
        &format!("{P} INSERT DATA {{ ex:z rdfs:label \"She ran and runs\"@en }} ; DELETE DATA {{ ex:a rdfs:label \"Running quickly\"@en }}"),
        &o,
    )
    .unwrap();
    assert_eq!(hits(&s, "\"run\" \"lang:en\""), ["b", "d", "z"]);
}

#[test]
fn text_query_ranks_its_hits() {
    let s = mem(TextConfig::default());
    // the rank slot follows Jena's five; tied scores share a rank
    let r = rows(
        &s,
        "SELECT ?s ?rank { (?s ?sc ?lit ?g ?p ?rank) text:query (rdfs:label \"running OR runs OR cheval\") }",
    );
    assert_eq!(r.len(), 5, "{r:?}");
    // the rank is one more than the number of hits with a higher score
    let q = "SELECT ?s ?sc ?rank { (?s ?sc [] [] [] ?rank) text:query (rdfs:label \"running OR runs OR cheval\") }";
    let res = query(s.snapshot(), &format!("{P}{q}"), &QueryOptions::default()).unwrap();
    let mut scored: Vec<(f64, i64)> = res
        .rows()
        .into_iter()
        .map(|row| {
            let num = |t: &Option<oxrdf::Term>| match t {
                Some(oxrdf::Term::Literal(l)) => l.value().parse::<f64>().unwrap(),
                t => panic!("{t:?}"),
            };
            (num(&row[1]), num(&row[2]) as i64)
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (score, rank) in &scored {
        let better = scored.iter().filter(|(s, _)| s > score).count() as i64;
        assert_eq!(*rank, better + 1, "{scored:?}");
    }
    assert!(
        scored.windows(2).any(|w| w[0].1 == w[1].1),
        "a tie: {scored:?}"
    );
    // a limit cuts the ranks it returns
    let top = rows(
        &s,
        "SELECT ?rank { (?s ?sc ?lit ?g ?p ?rank) text:query (rdfs:label \"running OR runs OR cheval\" 1) }",
    );
    assert_eq!(top, ["1"]);
    // a constant in the rank slot is refused, as in the other slots
    assert!(
        err(
            &s,
            "SELECT ?s { (?s ?sc ?lit ?g ?p 1) text:query \"running\" }"
        )
        .contains("must be a variable")
    );
    assert!(
        err(
            &s,
            "SELECT ?s { (?s ?sc ?lit ?g ?p ?r ?x) text:query \"running\" }"
        )
        .contains("malformed")
    );
}
