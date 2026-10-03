//! Blank nodes that a query mints (`BNODE()`, CONSTRUCT templates) belong to that one
//! result. Their labels (`_:q…`) never name a stored node (`_:b…`), in a later request or
//! in an INSERT of the same one. A stored node is named only by its label exactly as the
//! store writes it.

use oxrdf::{BlankNode, GraphNameRef, NamedNode, NamedOrBlankNode, Quad, Term, Triple};
use sparkles_core::Dataset;
use sparkles_core::io::RdfFormat;
use sparkles_core::sparql::QueryOptions;
use sparkles_core::store::parse_bnode_label;

const PREFIX: &str = "PREFIX ex: <http://ex.org/> ";

/// A dataset with two stored blank nodes.
fn ds() -> Dataset {
    let ds = Dataset::memory();
    ds.load_str(
        "@prefix ex: <http://ex.org/> . _:a ex:p 1 . _:b ex:p 2 . ex:s ex:p 3 .",
        RdfFormat::Turtle,
    )
    .unwrap();
    ds
}

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("http://ex.org/{local}"))
}

fn label(t: &Term) -> &str {
    match t {
        Term::BlankNode(b) => b.as_str(),
        t => panic!("not a blank node: {t}"),
    }
}

fn opts(bindings: &[(&str, &Term)]) -> QueryOptions {
    QueryOptions {
        initial_bindings: bindings
            .iter()
            .map(|(n, t)| (n.to_string(), (*t).clone()))
            .collect(),
        ..Default::default()
    }
}

/// The values of `var` in the solutions of a SELECT.
fn column(ds: &Dataset, q: &str, var: &str, o: &QueryOptions) -> Vec<Term> {
    let r = ds.query_with(&format!("{PREFIX}{q}"), o).unwrap();
    let i = r.vars.iter().position(|v| v == var).unwrap();
    r.rows()
        .into_iter()
        .map(|row| row[i].clone().unwrap())
        .collect()
}

fn ask(ds: &Dataset, q: &str, o: &QueryOptions) -> bool {
    ds.query_with(&format!("{PREFIX}{q}"), o).unwrap().boolean
}

fn stored_nodes(ds: &Dataset) -> Vec<Term> {
    column(
        ds,
        "SELECT ?s { ?s ex:p ?o FILTER isBlank(?s) }",
        "s",
        &opts(&[]),
    )
}

#[test]
fn minted_labels_do_not_look_like_stored_ones() {
    let ds = ds();
    let stored = stored_nodes(&ds);
    assert_eq!(stored.len(), 2);
    for s in &stored {
        assert!(label(s).starts_with('b'), "{s}");
        assert!(parse_bnode_label(label(s)).is_some(), "{s}");
    }
    let minted = column(
        &ds,
        "SELECT (BNODE() AS ?n) (BNODE(STR(?o)) AS ?m) { ?s ex:p ?o }",
        "n",
        &opts(&[]),
    );
    assert_eq!(minted.len(), 3);
    for m in &minted {
        assert!(label(m).starts_with('q'), "{m}");
        assert!(parse_bnode_label(label(m)).is_none(), "{m}");
        assert!(!stored.contains(m));
    }
}

#[test]
fn a_minted_label_from_an_earlier_result_is_a_new_node() {
    let ds = ds();
    let earlier = column(&ds, "SELECT (BNODE() AS ?b) {}", "b", &opts(&[]));
    let earlier = &earlier[0];
    // bound in a later query, it is not the node that query mints first …
    let o = opts(&[("x", earlier)]);
    let same = column(
        &ds,
        "SELECT ?n { BIND(BNODE() AS ?n) FILTER(sameTerm(?n, ?x)) }",
        "n",
        &o,
    );
    assert!(same.is_empty(), "{same:?}");
    // … nor any stored node, but it is one node however often it is bound
    assert!(!ask(&ds, "ASK { ?x ?p ?o }", &o));
    assert!(!ask(&ds, "ASK { ?s ?p ?o FILTER(sameTerm(?s, ?x)) }", &o));
    let twice = opts(&[("x", earlier), ("y", earlier)]);
    assert!(ask(&ds, "ASK { FILTER(sameTerm(?x, ?y)) }", &twice));
    // the dataset API finds and removes nothing with it
    let subject = NamedOrBlankNode::BlankNode(BlankNode::new_unchecked(label(earlier)));
    assert!(
        ds.find(None, Some(&subject), None, None)
            .unwrap()
            .is_empty()
    );
    let quad = Quad::new(
        subject,
        ex("p"),
        Term::from(oxrdf::Literal::from(1)),
        oxrdf::GraphName::DefaultGraph,
    );
    assert!(!ds.remove(quad.as_ref()).unwrap());
    assert_eq!(ds.len(), 3);
}

#[test]
fn a_stored_node_is_named_only_by_its_exact_label() {
    let ds = ds();
    let stored = stored_nodes(&ds);
    let first = stored
        .iter()
        .find(|t| label(t) == "b0")
        .expect("the first stored blank node is _:b0");
    assert!(ask(&ds, "ASK { ?x ex:p ?o }", &opts(&[("x", first)])));
    // other spellings of the same number, or one wider than an id, name nothing
    for other in ["b00", "B0", "b1000000000000000", "q0", "x0"] {
        let t = Term::BlankNode(BlankNode::new_unchecked(other));
        assert!(
            !ask(&ds, "ASK { ?x ex:p ?o }", &opts(&[("x", &t)])),
            "{other}"
        );
        let s = NamedOrBlankNode::BlankNode(BlankNode::new_unchecked(other));
        assert!(
            ds.find(None, Some(&s), None, None).unwrap().is_empty(),
            "{other}"
        );
        assert!(
            ds.find(Some(GraphNameRef::DefaultGraph), None, None, Some(&t))
                .unwrap()
                .is_empty(),
            "{other}"
        );
    }
}

#[test]
fn bnode_in_insert_where_makes_new_stored_nodes() {
    let ds = ds();
    let before = stored_nodes(&ds);
    let stats = ds
        .update(&format!(
            "{PREFIX}INSERT {{ ?b ex:n ?n . ?b ex:m ?n . ex:s ex:has ?b }} \
             WHERE {{ VALUES ?n {{ 1 2 }} BIND(BNODE() AS ?b) }}"
        ))
        .unwrap();
    assert_eq!(stats.inserted, 6);
    let made = column(
        &ds,
        "SELECT ?b ?n { ex:s ex:has ?b . ?b ex:n ?n ; ex:m ?n }",
        "b",
        &opts(&[]),
    );
    assert_eq!(made.len(), 2);
    assert_ne!(made[0], made[1]);
    for b in &made {
        assert!(parse_bnode_label(label(b)).is_some(), "{b}");
        assert!(!before.contains(b));
    }
}

#[test]
fn minted_nodes_in_triple_terms_become_stored_nodes() {
    let ds = ds();
    ds.update(&format!(
        "{PREFIX}INSERT {{ ex:s ex:says <<( ?b ex:q ex:o )>> }} WHERE {{ BIND(BNODE() AS ?b) }} ;
         INSERT {{ ex:t ex:says ?t }} WHERE {{ BIND(TRIPLE(BNODE(), ex:q, ex:o) AS ?t) }} ;
         INSERT {{ ex:hit ex:is ex:yes }} WHERE {{
           ex:s ex:says <<( ?x ex:q ex:o )>> BIND(BNODE() AS ?n) FILTER(sameTerm(?x, ?n))
         }}"
    ))
    .unwrap();
    // an operation later in the same request does not mint the stored node again
    assert!(!ask(&ds, "ASK { ex:hit ?p ?o }", &opts(&[])));
    for s in ["s", "t"] {
        let inner = column(
            &ds,
            &format!("SELECT ?x {{ ex:{s} ex:says <<( ?x ex:q ex:o )>> }}"),
            "x",
            &opts(&[]),
        );
        assert_eq!(inner.len(), 1, "{s}");
        assert!(parse_bnode_label(label(&inner[0])).is_some(), "{inner:?}");
    }
    assert!(!ask(
        &ds,
        "ASK { ex:s ex:says <<( ?x ex:q ex:o )>> BIND(BNODE() AS ?n) FILTER(sameTerm(?x, ?n)) }",
        &opts(&[])
    ));
    // the stored triple term is found through the dataset API by its stored label
    let inner = column(
        &ds,
        "SELECT ?x { ex:s ex:says <<( ?x ex:q ex:o )>> }",
        "x",
        &opts(&[]),
    );
    let b = BlankNode::new_unchecked(label(&inner[0]));
    let tt = Term::Triple(Box::new(Triple::new(b, ex("q"), ex("o"))));
    assert_eq!(
        ds.find(None, None, Some(&ex("says")), Some(&tt))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn construct_template_nodes_belong_to_their_result() {
    let ds = ds();
    let stored = stored_nodes(&ds);
    let triples = ds
        .construct(&format!(
            "{PREFIX}CONSTRUCT {{ ?s ex:copy _:c }} WHERE {{ ?s ex:p ?o }}"
        ))
        .unwrap();
    assert_eq!(triples.len(), 3);
    let made: Vec<Term> = triples.iter().map(|t| t.object.clone()).collect();
    for m in &made {
        assert!(label(m).starts_with('q'), "{m}");
        assert!(parse_bnode_label(label(m)).is_none(), "{m}");
        assert!(!stored.contains(m));
        // not found by the dataset API, and in a later query a new node
        assert!(
            ds.find(None, None, None, Some(m)).unwrap().is_empty(),
            "{m}"
        );
        let o = opts(&[("x", m)]);
        assert!(!ask(
            &ds,
            "ASK { BIND(BNODE() AS ?n) FILTER(sameTerm(?n, ?x)) }",
            &o
        ));
    }
    // written back, they become new stored nodes
    let n = ds
        .extend(
            triples
                .iter()
                .map(|t| t.as_ref().in_graph(GraphNameRef::DefaultGraph)),
        )
        .unwrap();
    assert_eq!(n, 3);
    let copies = column(&ds, "SELECT ?c { ?s ex:copy ?c }", "c", &opts(&[]));
    assert_eq!(copies.len(), 3);
    for c in &copies {
        assert!(parse_bnode_label(label(c)).is_some(), "{c}");
        assert!(!stored.contains(c));
    }
}
