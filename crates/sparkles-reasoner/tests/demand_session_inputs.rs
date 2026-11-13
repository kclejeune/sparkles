//! A schema pass whose rules read `now` or make blank nodes can give each session its own
//! schema facts, an explanation's pass gives other facts than a query's, and a session
//! prepared again holds the facts of its earlier derivations, so a schema completeness
//! proof made in one session does not hold in another (spec F17, item 911). The tests empty the process-wide table cache, so they
//! have a binary of their own and run one at a time.

use oxsdatatypes::{DateTime, DayTimeDuration};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::reasoning::{Demand, DemandTerm, SessionRequest};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::explain::{DemandInput, ExplainOptions, Explained, explain_demand};
use sparkles_reasoner::query::QueryReasoner;
use sparkles_reasoner::{Extras, Inputs, Profile, tables};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The tests clear the table cache, so they do not run beside each other.
static SERIAL: Mutex<()> = Mutex::new(());

/// The rules after `conditional`: the domain fact follows from a derived instance fact,
/// which the schema pass does not read, and gives the answer.
const UNCONDITIONAL: &str = "
     [instance: (ex:s ex:seed ex:o) -> (ex:s ex:derived ex:o)]
     [schema: (ex:s ex:derived ex:o) -> (ex:p rdfs:domain ex:C)]
     [answer: (ex:p rdfs:domain ex:C) -> (ex:answer ex:value ex:yes)]";

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        b"<http://ex.org/s> <http://ex.org/seed> <http://ex.org/o> .".to_vec(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    s
}

fn reasoner(conditional: &str) -> QueryReasoner {
    let rules = format!(
        "@prefix ex: <http://ex.org/> .
         @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
         {conditional}
         {UNCONDITIONAL}"
    );
    QueryReasoner::new(Profile::Rules(rules), &Extras::default(), Inputs::default()).unwrap()
}

/// The answer, which the rules entail whatever the conditional rule does. `pad`
/// adds a union branch with a constant the store lacks, which the session interns before
/// its schema pass.
fn answer(s: &Store, qr: &QueryReasoner, pad: bool, when: &str) {
    let options = QueryOptions {
        reasoning: Some(qr.provider()),
        no_cache: true,
        ..Default::default()
    };
    let q = if pad {
        "SELECT ?v { { <http://ex.org/answer> <http://ex.org/value> ?v }
                     UNION { <urn:pad> <http://ex.org/value> ?v } }"
    } else {
        "SELECT ?v { <http://ex.org/answer> <http://ex.org/value> ?v }"
    };
    let r = query(s.snapshot(), q, &options).unwrap_or_else(|e| panic!("{when}: {e}"));
    assert_eq!(r.len(), 1, "{when}: the rules entail the answer");
}

/// Before a cutoff, a rule that reads `now` gives the schema fact the schema pass holds,
/// which lets the proof close `rdfs:domain`. After the cutoff the fact follows only from a
/// derived instance fact, which the pass does not read, so a session whose pass comes
/// after the cutoff must prove again rather than reuse the earlier proof, whether its
/// schema comes from a pass or from the table that pass keeps. The first session must
/// end before the cutoff for the test to show anything, so a slow run tries again with
/// a later cutoff.
#[test]
fn a_proof_over_a_schema_that_reads_now_is_not_reused() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let after = |secs: u64| {
        DateTime::now()
            .checked_add_day_time_duration(
                DayTimeDuration::try_from(Duration::from_secs(secs)).unwrap(),
            )
            .unwrap()
    };
    for margin in [2, 5, 15] {
        let cutoff = after(margin);
        let qr = reasoner(&format!(
            "[early: now(?t), lessThan(?t, '{cutoff}'^^xsd:dateTime) -> (ex:p rdfs:domain ex:C)]"
        ));
        let s = store();
        answer(&s, &qr, false, "before the cutoff");
        if DateTime::now() >= cutoff {
            continue;
        }
        while DateTime::now() <= cutoff {
            std::thread::sleep(Duration::from_millis(50));
        }
        // the schema's table goes, as eviction would take it, so the next session runs
        // a pass
        tables::clear();
        answer(&s, &qr, false, "a pass after the cutoff");
        answer(&s, &qr, false, "the table of a pass after the cutoff");
        return;
    }
    panic!("no first session ended before its cutoff");
}

/// A blank node that `makeSkolem` makes is labelled by the order the session met its
/// terms in, so a rule that tests its label gives the schema fact in one session and not
/// in another. One of the two orders below starts with the session whose label matches,
/// and the session after it must prove again rather than reuse that session's proof.
#[test]
fn a_proof_over_a_schema_that_makes_blank_nodes_is_not_reused() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for first_pad in [false, true] {
        let qr = reasoner(
            "[conditional: makeSkolem(?b), regex(?b, 'r[0-9a-f]*[02468ace]')
                -> (ex:p rdfs:domain ex:C)]",
        );
        let s = store();
        answer(&s, &qr, first_pad, "the first session");
        tables::clear();
        answer(&s, &qr, !first_pad, "a pass of the other label");
        answer(&s, &qr, !first_pad, "the table of that pass");
    }
}

/// A session prepared and derived twice holds, at its second schema acquisition, the
/// domain fact its first derivation gave. A proof over that graph closes `rdfs:domain`,
/// and a fresh session that reads the schema's table, which lacks the fact, must not use
/// it. The literal-subject fact keeps the first pass's proof from the table's readers.
#[test]
fn a_proof_of_a_session_prepared_again_is_not_reused() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let qr = reasoner("[invalid: (?s ex:literal ?x) -> (?x rdfs:subClassOf ex:C)]");
    let s = store();
    s.load(&[Source::from_bytes(
        b"<http://ex.org/a> <http://ex.org/literal> \"lit\" .".to_vec(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    let mut session = qr
        .provider()
        .start(SessionRequest {
            snap: s.snapshot(),
            readable: None,
            stop: Arc::new(|| Ok(())),
            max_memory: None,
            no_cache: true,
        })
        .unwrap();
    let iri = |i: &str| DemandTerm::Const(oxrdf::NamedNode::new_unchecked(i).into());
    let value = Demand {
        s: iri("http://ex.org/answer"),
        p: iri("http://ex.org/value"),
        o: DemandTerm::Var,
    };
    for round in 0..2 {
        session
            .prepare(std::slice::from_ref(&value), false)
            .unwrap();
        session.derive(vec![(0, Vec::new())]).unwrap();
        assert!(session.report().derived > 0 || round > 0, "round {round}");
    }
    drop(session);
    answer(&s, &qr, false, "a fresh session that reads the table");
}

/// An explanation's session keeps linear recursion as the rules state it, so its lower
/// stages give intermediate facts a query's session does not, and its schema pass can
/// derive schema facts from them. A query's session must not use a proof made over the
/// explanation's schema, in either order.
#[test]
fn a_proof_does_not_pass_between_explanation_and_query_schemas() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let rules = "@prefix ex: <http://ex.org/> .
         [exit: (?x ex:edge ?y) -> (?x ex:path ?y)]
         [rec: (?x ex:edge ?y), (?y ex:path ?z) -> (?x ex:path ?z)]
         [lower: noValue(ex:a, ex:path) -> (ex:t rdfs:domain ex:D)]
         [schema: (?x ex:path ex:d) -> (?x rdfs:subClassOf ex:C)]
         [answer: (ex:b rdfs:subClassOf ex:C) -> (ex:answer ex:value ex:yes)]";
    let iri = |i: &str| oxrdf::Term::from(oxrdf::NamedNode::new_unchecked(i));
    let target = [
        iri("http://ex.org/answer"),
        iri("http://ex.org/value"),
        iri("http://ex.org/yes"),
    ];
    for explanation_first in [true, false] {
        let qr = QueryReasoner::new(
            Profile::Rules(rules.into()),
            &Extras::default(),
            Inputs::default(),
        )
        .unwrap();
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            b"<http://ex.org/a> <http://ex.org/edge> <http://ex.org/b> .
              <http://ex.org/b> <http://ex.org/edge> <http://ex.org/c> .
              <http://ex.org/c> <http://ex.org/edge> <http://ex.org/d> ."
                .to_vec(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let explain = || {
            let e = explain_demand(
                &DemandInput {
                    reasoner: &qr,
                    snap: s.snapshot(),
                    readable: None,
                },
                &target,
                &ExplainOptions::default(),
            )
            .unwrap();
            assert!(
                !matches!(e, Explained::NotFound),
                "the explanation finds the answer"
            );
        };
        if explanation_first {
            explain();
            tables::clear();
            answer(&s, &qr, false, "a query's pass after an explanation");
            answer(&s, &qr, false, "the table of that pass");
        } else {
            answer(&s, &qr, false, "a query's pass");
            tables::clear();
            explain();
            answer(&s, &qr, false, "a query after an explanation's pass");
        }
    }
}
