//! Write previews (dry runs) under write-time SHACL validation: a dry run reports the
//! summary the write gets and leaves the guard as it was.

use sparkles::Error;
use sparkles::guard::{GuardMode, Severity, WriteOptions};
use sparkles::io::{RdfFormat, Source};
use sparkles::preview::{self, DryRun};
use sparkles::sparql::QueryOptions;
use sparkles::sparql::update::{UpdateStats, update};
use sparkles::store::{Store, StoreOptions};
use sparkles_shacl::guard::{
    self, BaselinePolicy, DataGraphSel, SetOutcome, ShaclGuard, ShapesSource, ValidationConfig,
};
use std::sync::Arc;

const SHAPES: &str = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex.org/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:minCount 1 ; sh:datatype xsd:string ] ;
  sh:property [ sh:path ex:age ; sh:maxInclusive 150 ; sh:severity sh:Warning ] .
"#;
const P: &str = "PREFIX ex: <http://ex.org/> ";

fn cfg(mode: GuardMode, grandfather: bool) -> ValidationConfig {
    ValidationConfig {
        format: 1,
        language: None,
        mode,
        shapes: ShapesSource {
            graphs: Some(vec!["urn:shapes".into()]),
            ..Default::default()
        },
        data_graph: DataGraphSel::Named("default".into()),
        include_inferences: false,
        threshold: Severity::Violation,
        baseline: if grandfather {
            BaselinePolicy::Grandfather
        } else {
            Default::default()
        },
        timeout_seconds: 10.0,
        report_limit: 100,
        updated: None,
    }
}

fn store_with_shapes() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        SHAPES.as_bytes().to_vec(),
        RdfFormat::Turtle,
        Some(oxrdf::NamedNode::new("urn:shapes").unwrap()),
    )])
    .unwrap();
    s
}

fn upd(s: &Store, u: &str) -> sparkles::Result<UpdateStats> {
    update(s, &format!("{P}{u}"), &QueryOptions::default())
}

fn dry(s: &Store, u: &str) -> preview::Preview {
    let opts = QueryOptions {
        write: WriteOptions {
            dry_run: Some(DryRun::default()),
            ..Default::default()
        },
        ..Default::default()
    };
    preview::catch(update(s, &format!("{P}{u}"), &opts)).unwrap()
}

/// Remove the times, which differ from run to run.
fn strip(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(m) => {
            for k in ["millis", "lastFullMillis", "time"] {
                m.remove(k);
            }
            m.values_mut().for_each(strip);
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(strip),
        _ => {}
    }
}

fn status(g: &ShaclGuard) -> serde_json::Value {
    let mut v = serde_json::to_value(g.status()).unwrap();
    strip(&mut v);
    v
}

/// Whether a write was rejected, and its summary.
fn outcome(r: sparkles::Result<UpdateStats>) -> (bool, serde_json::Value) {
    let (rejected, s) = match r {
        Ok(st) => (false, st.commit.unwrap().validation.map(|v| (*v).clone())),
        Err(Error::Rejected(r)) => (true, Some(r.summary)),
        Err(e) => panic!("{e}"),
    };
    let mut v = serde_json::to_value(s).unwrap();
    strip(&mut v);
    (rejected, v)
}

/// A store that previews each write before making it ends where a store that only makes
/// the writes ends, with the same summaries on the way, in strict reject, warn and
/// grandfather mode. Writes the guard skips follow previews too, which would apply a
/// preview's state if the guard kept it.
#[test]
fn dry_runs_report_the_write_and_leave_the_guard_alone() {
    let writes = [
        "INSERT DATA { ex:p1 a ex:Person ; ex:name \"P1\" }",
        "INSERT DATA { ex:p2 a ex:Person }",
        "INSERT DATA { GRAPH <urn:other> { ex:z ex:q 1 } }",
        "INSERT DATA { ex:p1 ex:age 200 }",
        "DELETE DATA { ex:p1 ex:name \"P1\" }",
        "INSERT DATA { ex:p3 a ex:Person ; ex:name \"P3\" }",
        "INSERT DATA { GRAPH <urn:shapes> { ex:PersonShape <http://www.w3.org/ns/shacl#property> [ <http://www.w3.org/ns/shacl#path> ex:email ; <http://www.w3.org/ns/shacl#minCount> 1 ] } }",
        "DELETE WHERE { ex:p2 ?p ?o }",
        "INSERT DATA { GRAPH <urn:other> { ex:z ex:q 2 } }",
        "INSERT DATA { ex:p4 a ex:Person ; ex:name 4 }",
        "INSERT DATA { ex:p1 ex:name \"P1\" ; ex:email \"p1@ex.org\" }",
    ];
    for (mode, grandfather) in [
        (GuardMode::Reject, false),
        (GuardMode::Warn, false),
        (GuardMode::Reject, true),
    ] {
        let stores = [store_with_shapes(), store_with_shapes()];
        let mut guards: Vec<Arc<ShaclGuard>> = Vec::new();
        for s in &stores {
            if grandfather {
                upd(s, "INSERT DATA { ex:p0 a ex:Person }").unwrap();
            }
            match guard::set_config(s, Some(cfg(mode, grandfather))).unwrap() {
                SetOutcome::Installed(g, _) => guards.push(g),
                _ => panic!("installed"),
            }
        }
        for (i, w) in writes.iter().enumerate() {
            let before = status(&guards[0]);
            let p = dry(&stores[0], w);
            dry(&stores[0], writes[(i + 1) % writes.len()]);
            assert_eq!(status(&guards[0]), before, "{mode:?} {w}");
            let a = outcome(upd(&stores[0], w));
            let b = outcome(upd(&stores[1], w));
            assert_eq!(a, b, "{mode:?} {w}");
            let mut pv = serde_json::to_value(p.validation.as_deref()).unwrap();
            strip(&mut pv);
            assert_eq!((p.rejected(), pv), a, "{mode:?} {w}");
            assert_eq!(status(&guards[0]), status(&guards[1]), "{mode:?} {w}");
        }
    }
}
