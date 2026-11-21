//! The head against the full summary on generated ontologies (roadmap item 735).
//!
//! Each case is a small random ontology that mixes ontology nodes, `owl:Axiom` and
//! `owl:Annotation` nodes, nested ones, blank axiom types and other roles on those nodes,
//! declared and custom annotation properties, terminology elsewhere, several graphs and
//! both modes. The head must answer what the full summary answers, opening the ontology
//! when it cannot show that its fragment is closed. `SPARKLES_HEAD_GEN_CASES` sets the
//! number of cases and `SPARKLES_HEAD_GEN_SEED` the first seed.

use super::head_tests::{dataset, head_of};
use super::wire;
use crate::sparql::QueryOptions;
use serde_json::{Value as J, json};

/// A small seeded generator (splitmix64), so that the test needs no new dependency and a
/// failing seed reproduces.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// True with probability `p`.
    fn p(&mut self, p: f64) -> bool {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[(self.next() % xs.len() as u64) as usize]
    }
}

/// One generated case: the statements by graph and the request's members.
struct Case {
    statements: Statements,
    args: J,
}

impl Case {
    fn data(&self) -> String {
        let mut out = String::new();
        for (g, s) in &self.statements {
            match g {
                None => out.push_str(&format!("{s}\n")),
                Some(g) => out.push_str(&format!("GRAPH <http://ex.org/{g}> {{ {s} }}\n")),
            }
        }
        out
    }
}

/// A triple a reifier can name, as source, property and target.
type Triple = (String, String, String);

/// The statements of a case by graph.
type Statements = Vec<(Option<&'static str>, String)>;

/// The default graph most of the time, otherwise `g1` or `g2`.
fn graph(r: &mut Rng) -> Option<&'static str> {
    if r.p(0.85) {
        None
    } else if r.p(0.6) {
        Some("g1")
    } else {
        Some("g2")
    }
}

/// Add the statement `s` to a graph.
fn add(r: &mut Rng, st: &mut Statements, s: String) {
    let g = graph(r);
    st.push((g, s));
}

/// Add the triple `s p o` to a graph, as one a reifier can name.
fn triple(
    r: &mut Rng,
    st: &mut Statements,
    reifiable: &mut Vec<Triple>,
    s: &str,
    p: &str,
    o: &str,
) {
    add(r, st, format!("{s} {p} {o} ."));
    reifiable.push((s.to_string(), p.to_string(), o.to_string()));
}

fn generate(seed: u64) -> Case {
    let mut r = Rng(seed);
    let mut st: Statements = Vec::new();
    let mut reifiable: Vec<Triple> = Vec::new();

    // the ontology nodes
    let mut onodes: Vec<&str> = Vec::new();
    if r.p(0.85) {
        onodes.push(":o");
    }
    if r.p(0.3) {
        onodes.push("_:b");
    }
    if r.p(0.15) {
        onodes.push(":o2");
    }
    for n in &onodes {
        triple(
            &mut r,
            &mut st,
            &mut reifiable,
            n,
            "rdf:type",
            "owl:Ontology",
        );
        if r.p(0.1) {
            // typed in a second graph too
            st.push((Some("g2"), format!("{n} a owl:Ontology .")));
        }
        if r.p(0.6) {
            triple(&mut r, &mut st, &mut reifiable, n, "rdfs:label", "\"t\"");
        }
        if r.p(0.4) {
            triple(&mut r, &mut st, &mut reifiable, n, ":n", "\"x\"");
        }
        if r.p(0.2) {
            triple(&mut r, &mut st, &mut reifiable, n, ":q", "\"y\"");
        }
        if r.p(0.4) {
            triple(&mut r, &mut st, &mut reifiable, n, "owl:versionIRI", ":v1");
        }
        if r.p(0.15) {
            triple(&mut r, &mut st, &mut reifiable, n, "owl:versionIRI", ":v2");
        }
        if r.p(0.08) {
            triple(
                &mut r,
                &mut st,
                &mut reifiable,
                n,
                "owl:versionIRI",
                "\"v\"",
            );
        }
        if r.p(0.35) {
            triple(&mut r, &mut st, &mut reifiable, n, "owl:imports", ":i");
        }
        if r.p(0.15) {
            triple(&mut r, &mut st, &mut reifiable, n, "rdfs:seeAlso", "_:v");
            if r.p(0.5) {
                add(&mut r, &mut st, "_:v rdfs:label \"v\" .".into());
            }
            if r.p(0.25) {
                add(
                    &mut r,
                    &mut st,
                    "_:v a owl:AllDisjointClasses ; owl:members (:A :B) .".into(),
                );
            }
        }
        if r.p(0.1) {
            triple(&mut r, &mut st, &mut reifiable, n, "rdfs:seeAlso", ":A");
        }
        // other roles of the node
        if r.p(0.07) {
            add(
                &mut r,
                &mut st,
                format!("{n} a owl:AllDisjointClasses ; owl:members (:A :B) ."),
            );
        }
        if r.p(0.05) {
            add(&mut r, &mut st, format!("{n} a owl:Class ."));
        }
        if r.p(0.05) {
            add(&mut r, &mut st, format!("{n} rdfs:subClassOf :B ."));
        }
        if r.p(0.04) {
            triple(&mut r, &mut st, &mut reifiable, n, "rdf:type", ":Custom");
        }
        if r.p(0.05) {
            add(&mut r, &mut st, format!(":C rdfs:subClassOf {n} ."));
        }
    }
    if onodes.len() > 1 && r.p(0.3) {
        add(
            &mut r,
            &mut st,
            format!("{} owl:priorVersion {} .", onodes[0], onodes[1]),
        );
    }

    // an ontology node typed with another one, which points to it when the full read
    // reads the triple
    if onodes.len() > 1 && r.p(0.4) {
        let (a, b) = (onodes[0], onodes[1]);
        let (a, b) = if r.p(0.5) { (a, b) } else { (b, a) };
        triple(&mut r, &mut st, &mut reifiable, a, "rdf:type", b);
    }
    // vocabulary in other roles
    if r.p(0.02) {
        add(&mut r, &mut st, "rdf:type a owl:ObjectProperty .".into());
    }
    if r.p(0.03) {
        add(&mut r, &mut st, "rdfs:label a owl:ObjectProperty .".into());
    }
    if r.p(0.04) && !onodes.is_empty() {
        add(
            &mut r,
            &mut st,
            "xsd:minLength a owl:AnnotationProperty .".into(),
        );
        let n = onodes[0];
        triple(&mut r, &mut st, &mut reifiable, n, "xsd:minLength", "\"1\"");
    }

    // declarations, some of them counted through reifications
    if r.p(0.7) {
        triple(
            &mut r,
            &mut st,
            &mut reifiable,
            ":n",
            "rdf:type",
            "owl:AnnotationProperty",
        );
    }
    if r.p(0.08) {
        add(&mut r, &mut st, ":n a owl:ObjectProperty .".into());
    }
    if r.p(0.08) {
        add(&mut r, &mut st, ":n rdfs:subPropertyOf rdfs:label .".into());
    }
    if r.p(0.6) {
        triple(
            &mut r,
            &mut st,
            &mut reifiable,
            ":A",
            "rdf:type",
            "owl:Class",
        );
    }
    if r.p(0.4) {
        triple(
            &mut r,
            &mut st,
            &mut reifiable,
            ":B",
            "rdf:type",
            "owl:Class",
        );
    } else if r.p(0.4) {
        triple(
            &mut r,
            &mut st,
            &mut reifiable,
            ":B",
            "rdf:type",
            "rdfs:Class",
        );
    }
    if r.p(0.05) {
        add(&mut r, &mut st, ":A a owl:Restriction .".into());
    }
    if r.p(0.4) {
        triple(
            &mut r,
            &mut st,
            &mut reifiable,
            ":pp",
            "rdf:type",
            "owl:ObjectProperty",
        );
    }
    if r.p(0.15) {
        triple(
            &mut r,
            &mut st,
            &mut reifiable,
            ":pp",
            "rdf:type",
            "owl:TransitiveProperty",
        );
    }
    if r.p(0.15) {
        triple(&mut r, &mut st, &mut reifiable, ":A", "rdfs:label", "\"a\"");
    }

    // terminology elsewhere
    if r.p(0.5) {
        triple(
            &mut r,
            &mut st,
            &mut reifiable,
            ":A",
            "rdfs:subClassOf",
            ":B",
        );
    }
    if r.p(0.25) {
        add(
            &mut r,
            &mut st,
            ":C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :pp ; owl:someValuesFrom :A ] ."
                .into(),
        );
    }
    if r.p(0.15) {
        add(&mut r, &mut st, ":pp rdfs:range :A .".into());
    }

    // other declaring types and OWL 1 forms
    for (s_, ty, p_) in [
        (":op", "owl:OntologyProperty", 0.1),
        (":dp", "owl:DatatypeProperty", 0.2),
        (":dt", "rdfs:Datatype", 0.15),
        (":dr", "owl:DataRange", 0.05),
        (":ni", "owl:NamedIndividual", 0.15),
        (":fp", "owl:FunctionalProperty", 0.1),
        (":sp", "owl:SymmetricProperty", 0.1),
        (":ifp", "owl:InverseFunctionalProperty", 0.08),
    ] {
        if r.p(p_) {
            triple(&mut r, &mut st, &mut reifiable, s_, "rdf:type", ty);
        }
    }
    if r.p(0.1) && !onodes.is_empty() {
        triple(&mut r, &mut st, &mut reifiable, onodes[0], ":op", "\"o\"");
    }
    if r.p(0.02) {
        add(
            &mut r,
            &mut st,
            "rdfs:subClassOf a owl:ObjectProperty .".into(),
        );
    }
    if r.p(0.04) && !onodes.is_empty() {
        add(
            &mut r,
            &mut st,
            format!("{} a _:c . _:c a owl:Class .", onodes[0]),
        );
    }
    if r.p(0.1) && !onodes.is_empty() {
        triple(
            &mut r,
            &mut st,
            &mut reifiable,
            onodes[0],
            "owl:imports",
            "<http://ex.org/g2>",
        );
    }

    // reifiers, which may name each other
    let reifiers = (r.next() % 4) as usize;
    for k in 0..reifiers {
        let node = if r.p(0.05) {
            format!(":r{k}")
        } else {
            format!("_:r{k}")
        };
        let on_header: Vec<Triple> = reifiable
            .iter()
            .filter(|t| onodes.contains(&t.0.as_str()) && t.2 != "owl:Ontology")
            .cloned()
            .collect();
        let (s, p, o) = if !on_header.is_empty() && r.p(0.5) {
            r.pick(&on_header).clone()
        } else if !reifiable.is_empty() && r.p(0.85) {
            r.pick(&reifiable).clone()
        } else {
            let s = r.pick(&[":o", "_:b", ":A", "_:r0"]).to_string();
            let p = r.pick(&["rdfs:label", ":n", ":q", "rdf:type"]).to_string();
            (s, p, "\"t\"".to_string())
        };
        let g = graph(&mut r);
        let mut parts = Vec::new();
        if r.p(0.9) {
            parts.push(format!(
                "a {}",
                if r.p(0.5) {
                    "owl:Annotation"
                } else {
                    "owl:Axiom"
                }
            ));
        }
        if r.p(0.04) {
            parts.push("a owl:Annotation, owl:Axiom".into());
        }
        parts.push(format!("owl:annotatedSource {s}"));
        parts.push(format!("owl:annotatedProperty {p}"));
        if r.p(0.97) {
            parts.push(format!("owl:annotatedTarget {o}"));
        }
        if r.p(0.03) {
            parts.push("owl:annotatedTarget \"other\"".into());
        }
        if r.p(0.03) {
            parts.push("owl:annotatedSource :A".into());
        }
        if r.p(0.1) {
            // the reifier's triples split across graphs
            let last = parts.pop().unwrap();
            st.push((g, format!("{node} {} .", parts.join(" ; "))));
            add(&mut r, &mut st, format!("{node} {last} ."));
        } else {
            st.push((g, format!("{node} {} .", parts.join(" ; "))));
        }
        if r.p(0.7) {
            let c = format!("\"c{k}\"");
            st.push((g, format!("{node} rdfs:comment {c} .")));
            reifiable.push((node.clone(), "rdfs:comment".into(), c));
        }
        if r.p(0.25) {
            let d = format!("\"d{k}\"");
            st.push((g, format!("{node} :n {d} .")));
            reifiable.push((node.clone(), ":n".into(), d));
        }
        if r.p(0.3) {
            add(
                &mut r,
                &mut st,
                format!("{node} a owl:AllDisjointClasses ; owl:members (:A :B) ."),
            );
        }
        if r.p(0.04) {
            add(
                &mut r,
                &mut st,
                format!(
                    "{node} a owl:NegativePropertyAssertion ; owl:sourceIndividual :a ; \
                     owl:assertionProperty :pp ; owl:targetIndividual :c ."
                ),
            );
        }
        if r.p(0.04) {
            add(&mut r, &mut st, format!("{node} a owl:Class ."));
        }
        if r.p(0.04) {
            add(
                &mut r,
                &mut st,
                format!("{node} a owl:AllDifferent ; owl:distinctMembers (:a :c) ."),
            );
        }
        if r.p(0.04) {
            add(
                &mut r,
                &mut st,
                format!("{node} a owl:AllDisjointProperties ; owl:members (:pp :dp) ."),
            );
        }
        if r.p(0.04) {
            add(&mut r, &mut st, format!(":C rdfs:subClassOf {node} ."));
        }
        if r.p(0.03) {
            add(&mut r, &mut st, format!(":x a {node} ."));
        }
    }

    let args = match r.next() % 7 {
        0..=2 => json!({}),
        3 => json!({ "mode": "strict" }),
        4 => json!({ "graph": "http://ex.org/g1" }),
        5 => json!({ "graph": ["http://ex.org/g1", "http://ex.org/g2"], "imports": false }),
        _ => json!({ "graph": ["http://ex.org/g2", "http://ex.org/g1"], "mode": "strict" }),
    };
    Case {
        statements: st,
        args,
    }
}

/// The summary's answer and the head's for `case`, as results, and whether the head
/// opened the ontology.
fn answers(case: &Case) -> (Result<J, String>, Result<J, String>, bool) {
    let ds = dataset(&case.data());
    let o = QueryOptions::default();
    let full = wire::call(&ds, "summary", &case.args, &o)
        .map(|j| head_of(&j))
        .map_err(|e| e.to_string());
    let mut args = case.args.clone();
    args["view"] = json!("head");
    let head = wire::call(&ds, "summary", &args, &o).map_err(|e| e.to_string());
    let opened = wire::request_of(case.args.as_object().unwrap())
        .and_then(|req| ds.ontology().head(&req, &o))
        .is_ok_and(|h| h.opened);
    (full, head, opened)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[test]
fn the_head_answers_what_the_summary_does_on_generated_ontologies() {
    let cases = env_u64("SPARKLES_HEAD_GEN_CASES", 6000);
    let first = env_u64("SPARKLES_HEAD_GEN_SEED", 1);
    let mut read = 0u64;
    for seed in first..first + cases {
        let case = generate(seed);
        let (full, head, opened) = answers(&case);
        assert_eq!(
            head,
            full,
            "seed {seed}, args {}, data:\n{}",
            case.args,
            case.data()
        );
        if !opened {
            read += 1;
        }
    }
    // the head must still answer a fair share of the cases from its own reads
    assert!(
        read * 5 >= cases,
        "the head answered only {read} of {cases} cases without opening the ontology"
    );
}
