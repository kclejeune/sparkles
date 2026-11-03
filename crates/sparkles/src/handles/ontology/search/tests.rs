#![allow(clippy::single_range_in_vec_init)]

use super::*;
use crate::Dataset;
use crate::access::{GraphAccess, GraphRule, Graphs};
use crate::branch::BranchOptions;
use oxrdf::{GraphName, Literal, NamedNode, Quad};

const PREFIXES: &str = "PREFIX : <http://ex.org/>
PREFIX owl: <http://www.w3.org/2002/07/owl#>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX skos: <http://www.w3.org/2004/02/skos/core#>
PREFIX skosxl: <http://www.w3.org/2008/05/skos-xl#>
PREFIX oio: <http://www.geneontology.org/formats/oboInOwl#>
PREFIX obo: <http://purl.obolibrary.org/obo/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
";

fn update(ds: &Dataset, body: &str) {
    ds.update(&format!("{PREFIXES} {body}")).unwrap();
}

fn dataset(body: &str) -> Dataset {
    let ds = Dataset::memory();
    update(&ds, &format!("INSERT DATA {{ {body} }}"));
    ds
}

fn search_with(ds: &Dataset, s: &SearchRequest) -> SearchResults {
    ds.ontology()
        .search(&OntologyRequest::default(), s, &Default::default())
        .unwrap()
}

fn search(ds: &Dataset, q: &str) -> SearchResults {
    search_with(ds, &SearchRequest::new(q))
}

fn top(q: &str, limit: usize) -> SearchRequest {
    SearchRequest {
        limit,
        ..SearchRequest::new(q)
    }
}

fn iris(r: &SearchResults) -> Vec<String> {
    r.hits
        .iter()
        .map(|h| h.iri.trim_start_matches("http://ex.org/").to_string())
        .collect()
}

fn dump(ds: &Dataset) -> Vec<String> {
    ds.state()
        .entity_search
        .index
        .read()
        .as_ref()
        .expect("built")
        .dump()
}

/// The dump of a fresh index of the dataset's head.
fn rebuilt(ds: &Dataset) -> Vec<String> {
    Index::build(&ds.store().snapshot(), &|| Ok(()))
        .unwrap()
        .dump()
}

#[test]
fn tiers_rank_in_the_documented_order() {
    let ds = dataset(
        ":a a owl:Class ; rdfs:label \"acid\" .
         :b a owl:Class ; rdfs:label \"acid phosphatase\" .
         :c a owl:Class ; rdfs:label \"sour thing\" ; oio:hasExactSynonym \"acid\" .
         :d a owl:Class ; rdfs:label \"base\" ; skos:altLabel \"acidic compound\" .
         :e a owl:Class ; rdfs:label \"amino acid\" .
         :f a owl:Class ; rdfs:label \"lipid\" ; oio:hasRelatedSynonym \"fatty acid\" .
         :g a owl:Class ; rdfs:label \"acif\" .
         :h a owl:Class ; rdfs:label \"oxoanion\" ; obo:IAO_0000115 \"an anion of an acid\" .
         :i a owl:Class ; rdfs:label \"unrelated\" .",
    );
    let r = search_with(&ds, &top("Acid", 20));
    assert_eq!(iris(&r), ["a", "b", "c", "d", "e", "f", "g", "h"]);
    let tiers: Vec<u8> = r.hits.iter().map(|h| h.matched.tier).collect();
    assert_eq!(tiers, [0, 1, 2, 3, 4, 5, 6, 7]);
    let kinds: Vec<MatchKind> = r.hits.iter().map(|h| h.matched.kind).collect();
    use MatchKind::*;
    assert_eq!(
        kinds,
        [Exact, Prefix, Exact, Prefix, Words, Words, Fuzzy, Words]
    );
    assert_eq!(r.hits[6].matched.distance, 1);
    assert_eq!(r.hits[2].label.as_deref(), Some("sour thing"));
    assert_eq!(r.hits[2].matched.field, Field::Synonym);
    assert_eq!(r.hits[7].matched.field, Field::Definition);
    assert!(!r.more);
    // the same request gives the same answer
    assert_eq!(search_with(&ds, &top("Acid", 20)).hits, r.hits);
    // no fuzzy tier and no definitions
    let r = search_with(
        &ds,
        &SearchRequest {
            fuzzy: false,
            definitions: false,
            ..top("acid", 20)
        },
    );
    assert_eq!(iris(&r), ["a", "b", "c", "d", "e", "f"]);
}

#[test]
fn ties_break_by_deprecation_language_length_and_iri() {
    let ds = dataset(
        ":w1 a owl:Class ; rdfs:label \"water\"@en .
         :w2 a owl:Class ; rdfs:label \"water\" .
         :w3 a owl:Class ; rdfs:label \"water\"@fr ; rdfs:label \"eau\"@fr .
         :w0 a owl:Class ; rdfs:label \"water\" ; owl:deprecated true .
         :p1 a owl:Class ; rdfs:label \"waterlily\" .
         :p2 a owl:Class ; rdfs:label \"waterfall\" .
         :p3 a owl:Class ; rdfs:label \"waterbed\" .",
    );
    let r = search_with(&ds, &top("water", 20));
    // untagged and English, then other languages, deprecated last; prefixes by length
    assert_eq!(iris(&r), ["w1", "w2", "w3", "w0", "p3", "p2", "p1"]);
    assert!(r.hits[3].deprecated);
    let r = search_with(
        &ds,
        &SearchRequest {
            languages: vec!["fr".into()],
            ..top("water", 3)
        },
    );
    assert_eq!(iris(&r), ["w3", "w2", "w1"]);
    // the label shown is the one in the caller's language
    assert_eq!(r.hits[0].label.as_deref(), Some("eau"));
    assert_eq!(r.hits[0].label_lang.as_deref(), Some("fr"));
    // a regional tag prefers its language over untagged values
    let r = search_with(
        &ds,
        &SearchRequest {
            languages: vec!["en-GB".into()],
            ..top("water", 2)
        },
    );
    assert_eq!(iris(&r), ["w1", "w2"]);
    assert!(r.more);
}

#[test]
fn pages_follow_one_order() {
    let mut body = String::new();
    for i in 0..30 {
        body.push_str(&format!(
            ":c{i:02} a owl:Class ; rdfs:label \"kinase {i:02}\" .\n"
        ));
    }
    let ds = dataset(&body);
    let all = search_with(&ds, &top("kinase", 30));
    assert_eq!(all.hits.len(), 30);
    assert!(!all.more);
    let mut paged = Vec::new();
    for page in 0..3 {
        let r = search_with(
            &ds,
            &SearchRequest {
                offset: page * 10,
                ..top("kinase", 10)
            },
        );
        assert_eq!(r.more, page < 2);
        paged.extend(r.hits);
    }
    assert_eq!(paged, all.hits);
    let bad = ds.ontology().search(
        &OntologyRequest::default(),
        &top("x", 0),
        &Default::default(),
    );
    assert!(bad.is_err());
}

#[test]
fn kinds_filter_the_hits() {
    let ds = dataset(
        ":partOf a owl:ObjectProperty ; rdfs:label \"part of\" .
         :Part a owl:Class ; rdfs:label \"part\" .
         :partName a owl:DatatypeProperty ; rdfs:label \"part name\" .
         :c1 a skos:Concept ; skos:prefLabel \"part\"@en .
         :i1 a owl:NamedIndividual ; rdfs:label \"part 7\" .
         :notAnEntity rdfs:label \"part\" .",
    );
    let r = search(&ds, "part");
    assert!(!iris(&r).contains(&"notAnEntity".to_string()));
    assert_eq!(r.hits.len(), 5);
    let only = |k: &[Kind]| {
        iris(&search_with(
            &ds,
            &SearchRequest {
                kinds: k.to_vec(),
                ..SearchRequest::new("part")
            },
        ))
    };
    assert_eq!(only(&[Kind::Class]), ["Part"]);
    assert_eq!(only(&[Kind::Concept]), ["c1"]);
    assert_eq!(only(&[Kind::Individual]), ["i1"]);
    assert_eq!(
        only(&Kind::parse_filter("property").unwrap()),
        ["partOf", "partName"]
    );
    assert_eq!(r.hits[0].kinds.len(), 1);
}

#[test]
fn typos_are_tolerated_within_the_edits() {
    let ds = dataset(
        ":g a owl:Class ; rdfs:label \"glucose\" .
         :g6 a owl:Class ; rdfs:label \"glucose 6-phosphate\" .
         :f a owl:Class ; rdfs:label \"fructose\" .
         :c a owl:Class ; rdfs:label \"cholesterol\" .",
    );
    // a transposition counts once
    let r = search(&ds, "glucsoe");
    assert_eq!(iris(&r)[0], "g");
    assert_eq!(r.hits[0].matched.distance, 1);
    // a typo in a later word
    assert_eq!(iris(&search(&ds, "glucose 6-phosphote"))[0], "g6");
    // two typos from eight characters typed
    assert_eq!(iris(&search(&ds, "cholestrel")), ["c"]);
    // a short prefix takes no edits
    assert!(search(&ds, "glx").hits.is_empty());
    // one edit from four characters
    assert_eq!(iris(&search(&ds, "fruc")), ["f"]);
    assert_eq!(iris(&search(&ds, "frux")), ["f"]);
}

#[test]
fn highlights_cover_the_matched_text() {
    let ds = dataset(
        ":a a owl:Class ; rdfs:label \"Éthyl alcohol\" .
         :b a owl:Class ; rdfs:label \"methyl alcohol\" .",
    );
    let r = search(&ds, "ethyl");
    let h = &r.hits[0].matched;
    assert_eq!(h.text, "Éthyl alcohol");
    assert_eq!(h.highlight, [0..6]);
    assert_eq!(r.hits[1].matched.kind, MatchKind::Fuzzy);
    let r = search(&ds, "alco");
    assert_eq!(r.hits[0].matched.kind, MatchKind::Words);
    assert_eq!(r.hits[0].matched.highlight, [7..11]);
}

#[test]
fn short_forms_iris_and_prefixed_names() {
    let ds = dataset(
        "obo:CHEBI_15377 a owl:Class ; rdfs:label \"water\" ; oio:id \"CHEBI:15377\" .
         obo:CHEBI_15378 a owl:Class ; rdfs:label \"hydron\" .
         :hasPart a owl:ObjectProperty .",
    );
    ds.set_prefix("obo", "http://purl.obolibrary.org/obo/")
        .unwrap();
    let r = search(&ds, "CHEBI:15377");
    assert_eq!(r.hits[0].iri, "http://purl.obolibrary.org/obo/CHEBI_15377");
    assert_eq!(r.hits[0].matched.field, Field::ShortForm);
    assert_eq!(r.hits[0].matched.kind, MatchKind::Exact);
    assert_eq!(r.hits[0].short_form.as_deref(), Some("CHEBI:15377"));
    // the number alone matches a word of the short form
    assert_eq!(search(&ds, "15377").hits[0].matched.kind, MatchKind::Words);
    // a prefixed name is expanded to the IRI
    let r = search(&ds, "obo:CHEBI_1537");
    assert_eq!(r.hits.len(), 2);
    assert_eq!(r.hits[0].matched.property, None);
    assert_eq!(r.hits[1].short_form.as_deref(), Some("obo:CHEBI_15378"));
    // the local name's camel-case words
    let r = search(&ds, "part");
    assert_eq!(iris(&r), ["hasPart"]);
    assert_eq!(r.hits[0].label, None);
    assert_eq!(r.hits[0].short_form.as_deref(), Some("hasPart"));
}

#[test]
fn skos_xl_labels_follow_their_nodes() {
    let ds = dataset(
        ":c a skos:Concept ; skosxl:prefLabel :l1 ; skosxl:altLabel _:l2 .
         :l1 skosxl:literalForm \"Gewürz\"@de .
         _:l2 skosxl:literalForm \"spice\"@en .",
    );
    let r = search(&ds, "gewurz");
    assert_eq!(iris(&r), ["c"]);
    assert_eq!(r.hits[0].label.as_deref(), Some("Gewürz"));
    assert_eq!(r.hits[0].matched.field, Field::Label);
    assert_eq!(iris(&search(&ds, "spice")), ["c"]);
    // a new form of a known node, then a new link to a new node
    update(&ds, "INSERT DATA { :l1 skosxl:literalForm \"Würze\"@de }");
    assert_eq!(iris(&search(&ds, "wurze")), ["c"]);
    update(
        &ds,
        "INSERT DATA { :c skosxl:hiddenLabel :l3 . :l3 skosxl:literalForm \"spyce\" }",
    );
    assert_eq!(search(&ds, "spyce").hits[0].matched.kind, MatchKind::Exact);
    assert_eq!(dump(&ds), rebuilt(&ds));
    // removing the link removes its forms' values; a form goes with its triple
    update(&ds, "DELETE DATA { :c skosxl:prefLabel :l1 }");
    assert!(search(&ds, "gewurz").hits.is_empty());
    update(&ds, "DELETE DATA { :l3 skosxl:literalForm \"spyce\" }");
    // what is left is the one-edit match of "spice"
    let r = search(&ds, "spyce");
    assert_eq!(r.hits.len(), 1);
    assert_eq!(r.hits[0].matched.text, "spice");
    assert_eq!(dump(&ds), rebuilt(&ds));
}

#[test]
fn graphs_and_grants_limit_the_hits() {
    let ds = dataset(
        ":a a owl:Class ; rdfs:label \"apple\" .
         GRAPH :g { :b a owl:Class ; rdfs:label \"apricot\" . :a rdfs:label \"apfel\"@de }",
    );
    let req = |g: &str| OntologyRequest {
        graphs: vec![GraphRef::parse(g).unwrap()],
        ..Default::default()
    };
    let s = SearchRequest::new("ap");
    let found = |req: &OntologyRequest, o: &QueryOptions| {
        let r = ds.ontology().search(req, &s, o).unwrap();
        let mut v = iris(&r);
        v.sort();
        v
    };
    assert_eq!(found(&req("default"), &Default::default()), ["a"]);
    assert_eq!(found(&req("http://ex.org/g"), &Default::default()), ["b"]);
    let both = OntologyRequest {
        graphs: vec![
            GraphRef::Default,
            GraphRef::parse("http://ex.org/g").unwrap(),
        ],
        ..Default::default()
    };
    assert_eq!(found(&both, &Default::default()), ["a", "b"]);
    // a caller who may read the default graph alone
    let g = Graphs::Only(GraphRule::new(["default"], &[]));
    let o = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::graphs(g.clone(), g))),
        ..Default::default()
    };
    assert_eq!(found(&both, &o), ["a"]);
    // the German label in the other graph is not shown to that caller
    let r = ds
        .ontology()
        .search(
            &both,
            &SearchRequest {
                languages: vec!["de".into()],
                ..SearchRequest::new("apple")
            },
            &o,
        )
        .unwrap();
    assert_eq!(r.hits[0].label.as_deref(), Some("apple"));
    // history is refused
    let at = OntologyRequest {
        at: Some(crate::history::At::Commit(1)),
        ..Default::default()
    };
    assert!(ds.ontology().search(&at, &s, &Default::default()).is_err());
}

/// A deterministic sequence of edits over a small vocabulary of entities, labels,
/// graphs and SKOS-XL nodes.
fn edits(seed: u64, n: usize) -> Vec<String> {
    let mut x = seed;
    let mut next = |m: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % m
    };
    let words = [
        "alpha", "beta", "gamma", "delta", "alphabet", "gamut", "dolta",
    ];
    let types = [
        "owl:Class",
        "rdfs:Class",
        "owl:ObjectProperty",
        "skos:Concept",
    ];
    let props = [
        "rdfs:label",
        "skos:prefLabel",
        "skos:altLabel",
        "oio:hasExactSynonym",
        "skos:definition",
        "oio:id",
    ];
    let mut out = Vec::new();
    for _ in 0..n {
        let e = format!(":e{}", next(6));
        let g = match next(3) {
            0 => None,
            k => Some(format!(":g{k}")),
        };
        let triple = match next(5) {
            0 => format!("{e} a {}", types[next(4) as usize]),
            1 => format!("{e} owl:deprecated true"),
            2 => format!("{e} skosxl:prefLabel :n{}", next(3)),
            3 => format!(
                ":n{} skosxl:literalForm \"{}\"@en",
                next(3),
                words[next(7) as usize]
            ),
            _ => format!(
                "{e} {} \"{} {}\"{}",
                props[next(6) as usize],
                words[next(7) as usize],
                next(3),
                if next(2) == 0 { "@en" } else { "" }
            ),
        };
        let quad = match g {
            Some(g) => format!("GRAPH {g} {{ {triple} }}"),
            None => triple,
        };
        let op = if next(3) == 0 { "DELETE" } else { "INSERT" };
        out.push(format!("{op} DATA {{ {quad} }}"));
    }
    out
}

/// Apply `edits` to `ds` a few at a time, searching between batches so the index is
/// updated in place, and compare it with a rebuild after each batch.
fn check_against_rebuilds(ds: &Dataset, edits: &[String]) -> u64 {
    search(ds, "alpha");
    for batch in edits.chunks(3) {
        for e in batch {
            update(ds, e);
        }
        search(ds, "alpha");
        assert_eq!(dump(ds), rebuilt(ds), "after {batch:?}");
    }
    ds.ontology().search_stats().updates
}

#[test]
fn commits_keep_the_index_equal_to_a_rebuild() {
    for seed in [7, 11, 13] {
        let ds = Dataset::memory();
        let updates = check_against_rebuilds(&ds, &edits(seed, 150));
        assert!(
            updates > 10,
            "the index was updated in place {updates} times"
        );
    }
}

#[test]
fn a_persistent_dataset_keeps_the_index_across_commits_and_reopening() {
    let dir = tempfile::tempdir().unwrap();
    {
        let ds = Dataset::open(dir.path()).unwrap();
        let updates = check_against_rebuilds(&ds, &edits(5, 120));
        assert!(updates > 10, "updated in place {updates} times");
    }
    let ds = Dataset::open(dir.path()).unwrap();
    assert_eq!(ds.ontology().search_stats().commit, None);
    search(&ds, "alpha");
    assert_eq!(ds.ontology().search_stats().builds, 1);
    assert_eq!(dump(&ds), rebuilt(&ds));
    check_against_rebuilds(&ds, &edits(9, 30));
}

#[test]
fn branches_have_their_own_index() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    update(
        &ds,
        "INSERT DATA { :a a owl:Class ; rdfs:label \"anchor\" }",
    );
    assert_eq!(iris(&search(&ds, "anchor")), ["a"]);
    ds.create_branch("work", &BranchOptions::default()).unwrap();
    let work = ds.branch("work").unwrap();
    update(
        &work,
        "INSERT DATA { :b a owl:Class ; rdfs:label \"anchovy\" }",
    );
    update(&work, "DELETE DATA { :a rdfs:label \"anchor\" }");
    assert_eq!(iris(&search(&work, "anch")), ["b"]);
    assert_eq!(iris(&search(&ds, "anch")), ["a"]);
    assert_eq!(dump(&work), rebuilt(&work));
    // a merge is a commit on main like any other
    let r = search(&ds, "anch");
    assert_eq!(r.index, IndexUse::Current);
    ds.merge("work", "main", &Default::default()).unwrap();
    let r = search(&ds, "anch");
    assert_eq!(iris(&r), ["b"]);
    assert_ne!(r.index, IndexUse::Current);
    assert_eq!(dump(&ds), rebuilt(&ds));
    // and so is the revert of the merge
    let head = ds.store().head_commit().seq;
    ds.revert("main", head, &Default::default()).unwrap();
    assert_eq!(iris(&search(&ds, "anch")), ["a"]);
    assert_eq!(dump(&ds), rebuilt(&ds));
}

#[test]
fn an_aborted_transaction_changes_nothing() {
    let ds = dataset(":a a owl:Class ; rdfs:label \"anchor\" .");
    let before = search(&ds, "anchor");
    let q = Quad::new(
        NamedNode::new_unchecked("http://ex.org/b"),
        NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label"),
        Literal::new_simple_literal("anchorage"),
        GraphName::DefaultGraph,
    );
    let r: Result<()> = ds.transaction(|tx| {
        tx.insert(q.as_ref())?;
        Err(Error::invalid("abort"))
    });
    assert!(r.is_err());
    let after = search(&ds, "anchor");
    assert_eq!(after.commit, before.commit);
    assert_eq!(after.hits, before.hits);
    assert_eq!(after.index, IndexUse::Current);
}

#[test]
fn many_changes_at_once_keep_the_index_right() {
    let ds = dataset(":a a owl:Class ; rdfs:label \"anchor\" .");
    search(&ds, "anchor");
    let mut body = String::new();
    for i in 0..50 {
        body.push_str(&format!(
            ":c{i} a owl:Class ; rdfs:label \"anchor {i}\" .\n"
        ));
    }
    update(&ds, &format!("INSERT DATA {{ {body} }}"));
    let r = search_with(&ds, &top("anchor", 100));
    assert_eq!(r.hits.len(), 51);
    assert_eq!(dump(&ds), rebuilt(&ds));
    let s = ds.ontology().search_stats();
    assert_eq!(s.entities, 51);
    assert!(s.bytes > 0);
    update(&ds, "DELETE WHERE { ?c a owl:Class ; rdfs:label ?l }");
    assert!(search(&ds, "anchor").hits.is_empty());
    assert_eq!(dump(&ds), rebuilt(&ds));
}

#[test]
fn a_skos_xl_form_in_a_hidden_graph_stays_hidden() {
    // the link is in the default graph, the literal form in :secret
    let ds = dataset(
        ":c a skos:Concept ; skosxl:prefLabel :l ; rdfs:label \"public\" .
         GRAPH :secret { :l skosxl:literalForm \"classified\" }",
    );
    let req = |graphs: Vec<GraphRef>| OntologyRequest {
        graphs,
        ..Default::default()
    };
    let default_only = req(vec![GraphRef::Default]);
    let r = ds
        .ontology()
        .search(
            &default_only,
            &SearchRequest::new("classified"),
            &Default::default(),
        )
        .unwrap();
    assert!(r.hits.is_empty(), "{:?}", r.hits);
    let r = ds
        .ontology()
        .search(
            &default_only,
            &SearchRequest::new("public"),
            &Default::default(),
        )
        .unwrap();
    assert_eq!(r.hits[0].label.as_deref(), Some("public"));
    // a caller whose grants hide the graph, with every graph asked for
    let both = req(vec![
        GraphRef::Default,
        GraphRef::parse("http://ex.org/secret").unwrap(),
    ]);
    let g = Graphs::Only(GraphRule::new(["default"], &[]));
    let o = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::graphs(g.clone(), g))),
        ..Default::default()
    };
    let r = ds
        .ontology()
        .search(&both, &SearchRequest::new("classified"), &o)
        .unwrap();
    assert!(r.hits.is_empty(), "{:?}", r.hits);
    // with both graphs readable the form is found
    let r = ds
        .ontology()
        .search(
            &both,
            &SearchRequest::new("classified"),
            &Default::default(),
        )
        .unwrap();
    assert_eq!(iris(&r), ["c"]);
}

#[test]
fn a_node_linked_twice_is_released_once() {
    let ds = dataset(
        ":c a skos:Concept ; skosxl:prefLabel :l ; skosxl:altLabel :l .
         :l skosxl:literalForm \"twice\" .",
    );
    assert_eq!(iris(&search(&ds, "twice")), ["c"]);
    update(&ds, "DELETE DATA { :c a skos:Concept }");
    assert!(search(&ds, "twice").hits.is_empty());
    // two new nodes must not share the freed slot
    update(
        &ds,
        "INSERT DATA { :d a skos:Concept ; skosxl:prefLabel :m . :m skosxl:literalForm \"first\" .
                       :e a skos:Concept ; skosxl:prefLabel :n . :n skosxl:literalForm \"second\" . }",
    );
    assert_eq!(iris(&search(&ds, "first")), ["d"]);
    assert_eq!(iris(&search(&ds, "second")), ["e"]);
    update(&ds, "DELETE DATA { :m skosxl:literalForm \"first\" }");
    assert!(search(&ds, "first").hits.is_empty());
    assert_eq!(iris(&search(&ds, "second")), ["e"]);
    assert_eq!(dump(&ds), rebuilt(&ds));
}

#[test]
fn local_name_parts_match_past_the_postings_shortcut() {
    // `has` narrows the candidates to hasPart, while many words start with `pa`
    let mut body = String::from(":hasPart a owl:ObjectProperty .\n");
    for i in 0..200 {
        body.push_str(&format!(":c{i} a owl:Class ; rdfs:label \"pa{i:03}x\" .\n"));
    }
    let ds = dataset(&body);
    let r = search_with(
        &ds,
        &SearchRequest {
            fuzzy: false,
            ..SearchRequest::new("has pa")
        },
    );
    assert_eq!(iris(&r), ["hasPart"]);
}

#[test]
fn hidden_iris_do_not_use_up_the_iri_prefix_cap() {
    // more hidden IRIs than the cap sort before the one the caller may see
    let mut body = String::from("GRAPH :hidden {\n");
    for i in 0..query::MAX_IRI_PREFIX {
        body.push_str(&format!(":a{i:05} a owl:Class .\n"));
    }
    body.push_str("}\n:b a owl:Class .\n");
    let ds = dataset(&body);
    let g = Graphs::Only(GraphRule::new(["default"], &[]));
    let o = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::graphs(g.clone(), g))),
        ..Default::default()
    };
    let req = OntologyRequest {
        graphs: vec![
            GraphRef::Default,
            GraphRef::parse("http://ex.org/hidden").unwrap(),
        ],
        ..Default::default()
    };
    let r = ds
        .ontology()
        .search(&req, &SearchRequest::new("http://ex.org/"), &o)
        .unwrap();
    assert_eq!(iris(&r), ["b"]);
    assert!(!r.more);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    // a reader of every graph sees the cap and a warning
    let r = ds
        .ontology()
        .search(&req, &top("http://ex.org/", 100), &Default::default())
        .unwrap();
    assert_eq!(r.hits.len(), 100);
    assert!(!r.warnings.is_empty());
}

#[test]
fn many_equal_labels_keep_the_page_bounded_and_ordered() {
    let mut body = String::new();
    for i in 0..3000 {
        body.push_str(&format!(":e{i:04} a owl:Class ; rdfs:label \"same\" .\n"));
    }
    let ds = dataset(&body);
    let ten = search_with(&ds, &top("same", 10));
    let hundred = search_with(&ds, &top("same", 100));
    assert_eq!(iris(&ten), iris(&hundred)[..10]);
    let want: Vec<String> = (0..100).map(|i| format!("e{i:04}")).collect();
    assert_eq!(iris(&hundred), want);
    // a later page continues the same order
    let page = search_with(
        &ds,
        &SearchRequest {
            offset: 2990,
            limit: 20,
            ..SearchRequest::new("same")
        },
    );
    let want: Vec<String> = (2990..3000).map(|i| format!("e{i:04}")).collect();
    assert_eq!(iris(&page), want);
    assert!(!page.more);
}

#[test]
fn a_search_past_its_deadline_builds_nothing() {
    let ds = dataset(":a a owl:Class ; rdfs:label \"anchor\" .");
    let o = QueryOptions {
        deadline: Some(std::time::Instant::now() - std::time::Duration::from_millis(1)),
        ..Default::default()
    };
    let r = ds.ontology().search(
        &OntologyRequest::default(),
        &SearchRequest::new("anchor"),
        &o,
    );
    assert!(matches!(r, Err(Error::Timeout)), "{r:?}");
    assert_eq!(ds.ontology().search_stats().builds, 0);
    assert_eq!(iris(&search(&ds, "anchor")), ["a"]);
}

#[test]
fn equal_values_of_one_entity_rank_as_a_rebuild_does() {
    let ds = dataset(":w a owl:Class ; rdfs:label \"water\" .");
    assert_eq!(iris(&search(&ds, "water")), ["w"]);
    update(&ds, "INSERT DATA { :w skos:prefLabel \"water\" }");
    let updated = search(&ds, "water");
    assert_eq!(updated.index, IndexUse::Updated);
    let fresh = Dataset::memory();
    update(
        &fresh,
        "INSERT DATA { :w a owl:Class ; rdfs:label \"water\" ; skos:prefLabel \"water\" }",
    );
    let built = search(&fresh, "water");
    assert_eq!(updated.hits, built.hits);
}
