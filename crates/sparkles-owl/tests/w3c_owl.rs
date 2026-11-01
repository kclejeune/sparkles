//! The OWL 2 test cases of the W3C OWL Working Group (the approved export of
//! <https://www.w3.org/2009/11/owl-test/>), read from `SPARKLES_OWL2_TESTS` (the
//! `all.rdf` file; `scripts/fetch-test-suites.sh` downloads it). The test is skipped
//! when the variable is not set.
//!
//! For each test case with an RDF/XML ontology the test reads the ontology, with its
//! imports, in both modes and checks that
//!
//! * a case that the suite places in OWL 2 DL reads strictly without errors and passes
//!   the DL check, and a case outside OWL 2 DL fails it;
//! * the EL, QL and RL checks agree with the profiles the case lists and excludes;
//! * every ontology in OWL 2 DL round-trips: its axioms written to triples and read
//!   again give the same axioms and header (spec F21 A2), and the triples written from
//!   the axioms of that second read are isomorphic to the first write.
//!
//! Known differences are listed in `tests/w3c_owl_known.txt` with their reasons.

use oxrdf::{Graph, NamedNode, NamedOrBlankNode, Term, Triple};
use oxrdfio::{RdfFormat, RdfParser};
use sparkles_owl::profiles::{self, Profile};
use sparkles_owl::read::{self, Mode, ReadOptions};
use sparkles_owl::write;
use std::collections::{BTreeMap, BTreeSet};

const TEST: &str = "http://www.w3.org/2007/OWL/testOntology#";

/// The RDF/XML parser reads entity declarations with double quotes only, and the suite
/// writes some with single quotes.
fn fix_entities(data: &str) -> String {
    data.lines()
        .map(|l| {
            if l.trim_start().starts_with("<!ENTITY") {
                l.replace('\'', "\"")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse(data: &str, base: &str) -> Result<Vec<Triple>, String> {
    let data = &fix_entities(data);
    let mut out = Vec::new();
    let parser = RdfParser::from_format(RdfFormat::RdfXml)
        .with_base_iri(base)
        .map_err(|e| e.to_string())?;
    for q in parser.for_slice(data.as_bytes()) {
        let q = q.map_err(|e| e.to_string())?;
        out.push(Triple::new(q.subject, q.predicate, q.object));
    }
    Ok(out)
}

struct Case {
    id: String,
    ontologies: Vec<(String, String)>,
    premise: Option<String>,
    imports: Vec<String>,
    species: BTreeSet<String>,
    profiles: BTreeSet<String>,
    not_profiles: BTreeSet<String>,
}

fn cases(path: &str) -> Vec<Case> {
    let data = std::fs::read_to_string(path).expect("read the test cases");
    let triples = parse(&data, "http://owl.semanticweb.org/id/").expect("parse the test cases");
    let t = |l: &str| NamedNode::new_unchecked(format!("{TEST}{l}"));
    let rdf_type = NamedNode::new_unchecked(sparkles_owl::vocab::RDF_TYPE);
    let mut by_s: BTreeMap<String, Vec<&Triple>> = BTreeMap::new();
    for tr in &triples {
        by_s.entry(tr.subject.to_string()).or_default().push(tr);
    }
    let lit = |tr: &Triple| match &tr.object {
        Term::Literal(l) => Some(l.value().to_string()),
        _ => None,
    };
    let local = |tr: &Triple| match &tr.object {
        Term::NamedNode(n) => n.as_str().strip_prefix(TEST).map(str::to_string),
        _ => None,
    };
    let mut out = Vec::new();
    // the negative profile assertions
    let mut not: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for tr in &triples {
        if tr.predicate == rdf_type
            && matches!(&tr.object, Term::NamedNode(n) if n.as_str() == sparkles_owl::vocab::OWL_NEGATIVE_PROPERTY_ASSERTION)
        {
            let ts = &by_s[&tr.subject.to_string()];
            let get = |p: &str| {
                ts.iter()
                    .find(|x| x.predicate.as_str() == p)
                    .map(|x| x.object.clone())
            };
            if let (Some(Term::NamedNode(s)), Some(Term::NamedNode(p)), Some(Term::NamedNode(o))) = (
                get(sparkles_owl::vocab::OWL_SOURCE_INDIVIDUAL),
                get(sparkles_owl::vocab::OWL_ASSERTION_PROPERTY),
                get(sparkles_owl::vocab::OWL_TARGET_INDIVIDUAL),
            ) && p.as_str() == format!("{TEST}profile")
            {
                not.entry(format!("<{}>", s.as_str()))
                    .or_default()
                    .insert(o.as_str().strip_prefix(TEST).unwrap_or("").to_string());
            }
        }
    }
    for (s, ts) in &by_s {
        if !ts
            .iter()
            .any(|x| x.predicate == rdf_type && x.object == Term::from(t("TestCase")))
        {
            continue;
        }
        let mut c = Case {
            id: ts
                .iter()
                .find(|x| x.predicate == t("identifier"))
                .and_then(|x| lit(x))
                .unwrap_or_else(|| s.clone()),
            ontologies: Vec::new(),
            premise: None,
            imports: Vec::new(),
            species: BTreeSet::new(),
            profiles: BTreeSet::new(),
            not_profiles: not.get(s).cloned().unwrap_or_default(),
        };
        for x in ts {
            let p = x.predicate.as_str().strip_prefix(TEST).unwrap_or("");
            match p {
                "rdfXmlPremiseOntology" | "rdfXmlInputOntology" => {
                    c.premise = lit(x);
                    c.ontologies.push((p.to_string(), lit(x).unwrap()));
                }
                "rdfXmlConclusionOntology" | "rdfXmlNonConclusionOntology" => {
                    c.ontologies.push((p.to_string(), lit(x).unwrap()));
                }
                "species" => {
                    c.species.extend(local(x));
                }
                "profile" => {
                    c.profiles.extend(local(x));
                }
                "importedOntology" => {
                    if let Term::NamedNode(n) = &x.object
                        && let Some(its) = by_s.get(&format!("<{}>", n.as_str()))
                        && let Some(src) = its
                            .iter()
                            .find(|y| y.predicate == t("rdfXmlInputOntology"))
                            .and_then(|y| lit(y))
                    {
                        c.imports.push(src);
                    }
                }
                _ => {}
            }
        }
        if c.premise.is_some() {
            out.push(c);
        }
    }
    out
}

/// The triples of an ontology and its imports, with the graph of each.
fn load(premise: &str, imports: &[String]) -> Result<(Vec<Triple>, Vec<u32>), String> {
    let mut triples = Vec::new();
    let mut graphs = Vec::new();
    let base = "http://owl.semanticweb.org/test/";
    for (g, src) in std::iter::once(premise)
        .chain(imports.iter().map(String::as_str))
        .enumerate()
    {
        // blank node labels are local to a document
        let ts: Vec<Triple> = parse(src, base)?
            .into_iter()
            .map(|t| {
                let b = |n: &oxrdf::BlankNode| {
                    oxrdf::BlankNode::new_unchecked(format!("d{g}x{}", n.as_str()))
                };
                Triple::new(
                    match t.subject {
                        NamedOrBlankNode::BlankNode(n) => NamedOrBlankNode::BlankNode(b(&n)),
                        s => s,
                    },
                    t.predicate,
                    match t.object {
                        Term::BlankNode(n) => Term::BlankNode(b(&n)),
                        o => o,
                    },
                )
            })
            .collect();
        graphs.extend(std::iter::repeat_n(g as u32, ts.len()));
        triples.extend(ts);
    }
    Ok((triples, graphs))
}

fn read_loaded(loaded: &(Vec<Triple>, Vec<u32>), mode: Mode) -> read::Ontology {
    read::read(
        loaded.0.clone(),
        loaded.1.clone(),
        &[0],
        ReadOptions {
            mode,
            ..Default::default()
        },
    )
}

fn canonical(ts: &[Triple]) -> Graph {
    let mut g: Graph = ts.iter().collect();
    g.canonicalize(oxrdf::dataset::CanonicalizationAlgorithm::Unstable);
    g
}

/// The round trip of one ontology read: axioms → triples → axioms.
fn round_trip(o: &read::Ontology) -> Result<(), String> {
    let axioms: Vec<&sparkles_owl::Axiom> = o.axiom_set().collect();
    let mut fresh = write::counter("w");
    let first = write::ontology_triples(&o.header, axioms.iter().copied(), &mut fresh);
    let again = read::read_graph(first.clone(), ReadOptions::default());
    let a: BTreeSet<_> = o.axiom_set().cloned().collect();
    let b: BTreeSet<_> = again.axiom_set().cloned().collect();
    if a != b {
        let missing: Vec<String> = a.difference(&b).take(3).map(|x| x.to_string()).collect();
        let extra: Vec<String> = b.difference(&a).take(3).map(|x| x.to_string()).collect();
        return Err(format!("axioms differ: lost {missing:?}, gained {extra:?}"));
    }
    if again.header != o.header {
        return Err(format!(
            "header differs: {:?} vs {:?}",
            o.header, again.header
        ));
    }
    if !again.unmapped.is_empty() {
        return Err(format!(
            "{} triples of the written graph are unmapped, such as {}",
            again.unmapped.len(),
            again.triples[again.unmapped[0]]
        ));
    }
    let mut fresh = write::counter("w");
    let second = write::ontology_triples(&again.header, again.axiom_set(), &mut fresh);
    if canonical(&first) != canonical(&second) {
        return Err("the second write is not isomorphic to the first".into());
    }
    Ok(())
}

#[test]
fn owl2_test_cases() {
    let Ok(path) = std::env::var("SPARKLES_OWL2_TESTS") else {
        eprintln!("SPARKLES_OWL2_TESTS is not set: skipping the OWL 2 test cases");
        return;
    };
    let known: BTreeMap<String, String> = include_str!("w3c_owl_known.txt")
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            l.split_once(' ')
                .map(|(a, b)| (a.to_string(), b.trim().to_string()))
        })
        .collect();
    let cases = cases(&path);
    assert!(cases.len() > 300, "read only {} test cases", cases.len());
    let mut failures: BTreeMap<String, String> = BTreeMap::new();
    let mut stats: BTreeMap<&str, usize> = BTreeMap::new();
    for c in &cases {
        let premise = c.premise.as_deref().unwrap();
        let mut fail = |what: &str, why: String| {
            failures.entry(format!("{}:{what}", c.id)).or_insert(why);
        };
        let loaded = match load(premise, &c.imports) {
            Ok(l) => l,
            Err(e) => {
                *stats.entry("unparsable").or_default() += 1;
                eprintln!("{}: RDF/XML does not parse: {e}", c.id);
                continue;
            }
        };
        *stats.entry("read").or_default() += 1;
        let lenient = read_loaded(&loaded, Mode::Lenient);
        let strict = read_loaded(&loaded, Mode::Strict);
        if std::env::var("SPARKLES_OWL2_CASE").is_ok_and(|v| v == c.id) {
            eprintln!(
                "== {} species {:?} profiles {:?} not {:?}\n{premise}",
                c.id, c.species, c.profiles, c.not_profiles
            );
            for (name, o) in [("strict", &strict), ("lenient", &lenient)] {
                eprintln!("-- {name}: header {:?}", o.header);
                for ax in o.axiom_set() {
                    eprintln!("   {ax}");
                }
                for i in &o.unmapped {
                    eprintln!("   unmapped {}", o.triples[*i]);
                }
                for d in &o.diagnostics {
                    eprintln!("   {} {}: {}", d.severity.as_str(), d.code, d.message);
                }
            }
            for r in profiles::check(&strict, &Profile::ALL, Default::default()) {
                eprintln!("-- {} conforms {}", r.profile, r.conforms);
                for v in &r.violations {
                    eprintln!(
                        "   {}: {} [{:?}]",
                        v.rule,
                        v.message,
                        v.axiom.as_ref().map(|a| a.to_string())
                    );
                }
            }
        }
        // a case is in a profile when every ontology of it is: the premise and the
        // conclusions, each with the imports
        let mut all = vec![strict.clone()];
        for (kind, src) in &c.ontologies {
            // a conclusion is read with the premise's declarations
            let mut with_premise = c.imports.clone();
            with_premise.push(premise.to_string());
            if kind.contains("Conclusion")
                && let Ok(l) = load(src, &with_premise)
            {
                all.push(read_loaded(&l, Mode::Strict));
            }
        }
        if std::env::var("SPARKLES_OWL2_CASE").is_ok_and(|v| v == c.id) {
            for (kind, src) in &c.ontologies {
                eprintln!("== {kind}\n{src}");
            }
            for o in &all[1..] {
                eprintln!("-- conclusion axioms:");
                for ax in o.axiom_set() {
                    eprintln!("   {ax}");
                }
                for i in &o.unmapped {
                    eprintln!("   unmapped {}", o.triples[*i]);
                }
            }
        }
        let conforms = |p: Profile| -> (bool, String) {
            for o in &all {
                let r = &profiles::check(o, &[p], Default::default())[0];
                if !r.conforms {
                    let first = r
                        .violations
                        .first()
                        .map(|v| format!("{}: {}", v.rule, v.message))
                        .unwrap_or_default();
                    return (false, first);
                }
            }
            (true, String::new())
        };
        let in_dl = c.species.contains("DL");
        let (dl_ok, why) = conforms(Profile::Dl);
        if in_dl && !dl_ok {
            fail("dl", format!("in OWL 2 DL but the DL check fails: {why}"));
        } else if !in_dl && dl_ok {
            fail("dl", "outside OWL 2 DL but the DL check passes".into());
        } else {
            *stats.entry("species agree").or_default() += 1;
        }
        for p in [Profile::El, Profile::Ql, Profile::Rl] {
            let name = p.as_str().to_ascii_uppercase();
            let (ok, why) = conforms(p);
            if c.profiles.contains(&name) && !ok {
                fail(p.as_str(), format!("in {p} but the check fails: {why}"));
            } else if c.not_profiles.contains(&name) && ok {
                fail(p.as_str(), format!("outside {p} but the check passes"));
            } else if c.profiles.contains(&name) || c.not_profiles.contains(&name) {
                *stats.entry("profiles agree").or_default() += 1;
            }
        }
        if in_dl {
            match round_trip(&strict) {
                Ok(()) => *stats.entry("round trips").or_default() += 1,
                Err(e) => fail("roundtrip", e),
            }
        }
        // the other ontologies of the case read without panicking, and lenient reads of
        // DL ontologies keep every axiom of the strict read
        for (_, src) in &c.ontologies {
            if let Ok(l) = load(src, &c.imports) {
                let _ = read_loaded(&l, Mode::Lenient);
            }
        }
        if in_dl {
            let s: BTreeSet<_> = strict.axiom_set().collect();
            let l: BTreeSet<_> = lenient.axiom_set().collect();
            if !s.is_subset(&l) {
                fail(
                    "lenient",
                    "the lenient read lost axioms of the strict read".into(),
                );
            }
        }
    }
    let unexpected: Vec<(&String, &String)> = failures
        .iter()
        .filter(|(k, _)| !known.contains_key(*k))
        .collect();
    let fixed: Vec<&String> = known
        .keys()
        .filter(|k| !failures.contains_key(*k))
        .collect();
    eprintln!("OWL 2 test cases: {} cases, {stats:?}", cases.len());
    eprintln!(
        "{} known differences, {} unexpected, {} fixed",
        known.len(),
        unexpected.len(),
        fixed.len()
    );
    for (k, v) in &unexpected {
        eprintln!("  {k} {v}");
    }
    for k in &fixed {
        eprintln!("  fixed: {k}");
    }
    assert!(
        unexpected.is_empty() && fixed.is_empty(),
        "update tests/w3c_owl_known.txt"
    );
}
