//! A schema pass whose rules read `now` gives each session its own schema facts, so a
//! schema completeness proof made in one session does not hold in another (spec F17,
//! item 911). The test empties the process-wide table cache, so it has a binary of its
//! own.

use oxsdatatypes::{DateTime, DayTimeDuration};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::query::QueryReasoner;
use sparkles_reasoner::{Extras, Inputs, Profile, tables};
use std::time::Duration;

/// Before a cutoff, a rule that reads `now` gives the schema fact the schema pass holds,
/// which lets the proof close `rdfs:domain`. After the cutoff the fact follows only from a
/// derived instance fact, which the pass does not read, so a session whose pass comes
/// after the cutoff must prove again rather than reuse the earlier proof, whether its
/// schema comes from a pass or from the table that pass keeps.
#[test]
fn a_proof_over_a_schema_that_reads_now_is_not_reused() {
    let after = |secs: u64| {
        DateTime::now()
            .checked_add_day_time_duration(
                DayTimeDuration::try_from(Duration::from_secs(secs)).unwrap(),
            )
            .unwrap()
    };
    let cutoff = after(2);
    let rules = format!(
        "@prefix ex: <http://ex.org/> .
         @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
         [early: now(?t), lessThan(?t, '{cutoff}'^^xsd:dateTime) -> (ex:p rdfs:domain ex:C)]
         [instance: (ex:s ex:seed ex:o) -> (ex:s ex:derived ex:o)]
         [schema: (ex:s ex:derived ex:o) -> (ex:p rdfs:domain ex:C)]
         [answer: (ex:p rdfs:domain ex:C) -> (ex:answer ex:value ex:yes)]"
    );
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        b"<http://ex.org/s> <http://ex.org/seed> <http://ex.org/o> .".to_vec(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    let qr =
        QueryReasoner::new(Profile::Rules(rules), &Extras::default(), Inputs::default()).unwrap();
    let options = QueryOptions {
        reasoning: Some(qr.provider()),
        no_cache: true,
        ..Default::default()
    };
    let q = "SELECT ?v { <http://ex.org/answer> <http://ex.org/value> ?v }";
    let answer = |when: &str| {
        let r = query(s.snapshot(), q, &options).unwrap_or_else(|e| panic!("{when}: {e}"));
        assert_eq!(r.len(), 1, "{when}: the answer follows from ex:seed");
    };
    answer("before the cutoff");
    while DateTime::now() <= cutoff {
        std::thread::sleep(Duration::from_millis(50));
    }
    // the schema's table goes, as eviction would take it, so the next session runs a pass
    tables::clear();
    answer("a pass after the cutoff");
    answer("the table of a pass after the cutoff");
}
