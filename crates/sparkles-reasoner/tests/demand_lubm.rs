//! Derivation on demand over LUBM-style data stays proportional to the query (spec F17,
//! roadmap item 911). The session proves the schema of the shipped profiles complete, so
//! a query derives the facts its patterns need rather than the whole type closure, which
//! it did while the completeness proof gave up on LUBM's schema.

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::query::QueryReasoner;
use sparkles_reasoner::{Extras, Inputs, Profile};

#[allow(dead_code)]
#[path = "../examples/common/lubm.rs"]
mod lubm;

const QUERIES: &str = include_str!("../../../scripts/bench-lubm/queries.txt");

fn lubm_query(name: &str) -> String {
    let mut text = None::<String>;
    for line in QUERIES.lines() {
        if let Some(n) = line.strip_prefix("## ") {
            if text.is_some() {
                break;
            }
            if n.trim() == name {
                text = Some(String::new());
            }
        } else if let Some(t) = &mut text {
            t.push_str(line);
            t.push('\n');
        }
    }
    text.unwrap_or_else(|| panic!("no LUBM query {name}"))
}

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[
        Source::from_bytes(lubm::ontology().into_bytes(), RdfFormat::Turtle, None),
        Source::from_bytes(lubm::university(0).into_bytes(), RdfFormat::NTriples, None),
    ])
    .unwrap();
    s
}

/// The rows and the derived facts of LUBM query `name` under `reasoning=demand` with
/// `profile`, with the demand tables bypassed.
fn demand(s: &Store, profile: Profile, name: &str) -> (usize, u64) {
    let r = QueryReasoner::new(profile, &Extras::default(), Inputs::default()).unwrap();
    let opts = QueryOptions {
        no_cache: true,
        reasoning: Some(r.provider()),
        ..Default::default()
    };
    let result = query(s.snapshot(), &lubm_query(name), &opts).unwrap();
    let derived = result.ctx.reasoning.as_ref().unwrap().report.derived;
    (result.rows().len(), derived)
}

#[test]
fn lubm_demand_derives_what_the_query_needs() {
    let s = store();
    for (profile, name, rows, most) in [
        (Profile::Rdfs, "q1", 3, 1_000),
        (Profile::Rdfs, "q14", 8_010, 20_000),
        (Profile::OwlRl, "q1", 3, 5_000),
        (Profile::OwlRl, "q14", 8_010, 40_000),
    ] {
        let (n, derived) = demand(&s, profile.clone(), name);
        eprintln!("{} {name}: {n} rows, {derived} derived", profile.name());
        assert_eq!(n, rows, "{} {name}", profile.name());
        assert!(
            derived <= most,
            "{} {name} derived {derived} facts, more than {most}",
            profile.name()
        );
    }
}
