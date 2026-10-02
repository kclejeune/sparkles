use super::*;
use crate::{FileResolver, Status};
use oxrdf::{Literal, NamedNode, Term};
use proptest::prelude::{Strategy as Gen, *};
use sparkles::Error;
use sparkles::guard::Strategy;
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::QueryOptions;
use sparkles::sparql::update::{UpdateStats, update};
use sparkles::store::StoreOptions;

const P: &str = "PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> ";

/// The acceptance example's schema.
const SCHEMA: &str = "PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
start = @ex:Person
ex:Person EXTRA a { a [ex:Person] ; foaf:name xsd:string ;
  foaf:age xsd:integer MAXINCLUSIVE 150 ? ; foaf:knows @ex:Person * }
ex:Org CLOSED { a [ex:Org] ; foaf:name . ; ex:city [\"Paris\" \"Kyoto\"] }";

const DATA: &str = "@prefix ex: <http://ex.org/> . @prefix foaf: <http://xmlns.com/foaf/0.1/> .
ex:alice a ex:Person ; foaf:name \"Alice\" ; foaf:age 30 ; foaf:knows ex:bob .
ex:bob a ex:Person ; foaf:name \"Bob\" ; foaf:knows ex:alice .
ex:carol a ex:Person ; foaf:age 200 .
ex:acme a ex:Org ; foaf:name \"ACME\" ; ex:city \"Paris\" ; ex:mayor ex:bob .";

const MAP: &str = "{FOCUS a ex:Person}@ex:Person";

fn config(j: serde_json::Value) -> Result<ShexValidationConfig> {
    let c: ShexValidationConfig = serde_json::from_value(j)?;
    c.check()?;
    Ok(c)
}

fn cfg(mode: &str, schema: &str, map: &str) -> ShexValidationConfig {
    config(serde_json::json!({"language": "shex", "mode": mode,
        "schema": {"inline": schema, "format": "shexc"}, "shapeMap": map}))
    .unwrap()
}

fn upd(s: &Store, u: &str) -> sparkles::Result<UpdateStats> {
    update(s, &format!("{P}{u}"), &QueryOptions::default())
}

fn summary(st: &UpdateStats) -> ValidationSummary {
    st.commit
        .as_ref()
        .and_then(|c| c.validation.as_deref())
        .cloned()
        .expect("a validated commit")
}

fn open(root: &Path) -> Store {
    Store::open(root, StoreOptions::default()).unwrap()
}

fn loaded(root: &Path) -> Store {
    let s = open(root);
    s.load(&[Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn installed(o: SetOutcome) -> (Arc<ShexGuard>, ValidationSummary) {
    match o {
        SetOutcome::Installed(g, s) => (g, s),
        SetOutcome::NotConforming(s) => panic!("not conforming: {:?}", s.results),
        SetOutcome::Removed => panic!("removed"),
    }
}

fn exists(root: &Path, f: &str) -> bool {
    root.join(f).exists()
}

fn stored_config(root: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(root.join(CONFIG_FILE)).unwrap()).unwrap()
}

#[test]
fn configuration_fields() {
    let c = config(serde_json::json!({
        "format": 2, "language": "shex", "mode": "reject",
        "schema": {"file": "validation-schema.shex", "format": "shexc",
                   "source": "/abs/s.shex", "sha256": "00"},
        "shapeMap": "{FOCUS a <http://ex.org/Person>}@<http://ex.org/Person>",
    }))
    .unwrap();
    assert_eq!(c.data_graph, DataGraphSel::default());
    assert_eq!((c.timeout_seconds, c.report_limit), (10.0, 100));
    let back = serde_json::to_value(&c).unwrap();
    assert_eq!(back["language"], "shex");
    assert_eq!(back["schema"]["file"], "validation-schema.shex");
    assert!(back.get("updated").is_none());
    assert!(back["schema"].get("prefixes").is_none());
    // JSON shape maps
    let c = config(serde_json::json!({
        "language": "shex", "mode": "warn", "schema": {"inline": "<http://ex.org/S> {}"},
        "shapeMap": [{"node": "<http://ex.org/a>", "shape": "<http://ex.org/S>"}],
    }))
    .unwrap();
    assert_eq!(c.format, 2);
    assert!(matches!(c.shape_map, MapSource::Json(_)));
    // the inline schema is not written back
    assert!(
        serde_json::to_value(&c).unwrap()["schema"]
            .get("inline")
            .is_none()
    );
}

#[test]
fn rejected_configurations() {
    let base = || {
        serde_json::json!({"format": 2, "language": "shex", "mode": "reject",
            "schema": {"inline": "<http://ex.org/S> {}"}, "shapeMap": "<http://ex.org/a>@START"})
    };
    let with = |k: &str, v: serde_json::Value| {
        let mut j = base();
        j[k] = v;
        config(j)
    };
    assert!(config(base()).is_ok());
    assert!(with("format", 1.into()).is_err());
    assert!(with("language", "shacl".into()).is_err());
    // no severities in ShEx
    assert!(with("threshold", "warning".into()).is_err());
    assert!(with("reportLimit", 0.into()).is_err());
    assert!(with("timeoutSeconds", 0.into()).is_err());
    assert!(with("dataGraph", "other".into()).is_err());
    assert!(with("schema", serde_json::json!({})).is_err());
}

/// Schemas and maps that cannot be used are refused when the configuration is set,
/// and nothing is installed.
#[test]
fn unusable_schemas_and_maps_are_refused() {
    let store = Store::in_memory(StoreOptions::default());
    let refused = |schema: &str, map: &str| {
        let e = set_config(&store, Some(cfg("reject", schema, map)), &NoImports)
            .err()
            .unwrap_or_else(|| panic!("accepted: {schema} / {map}"));
        format!("{e:#}")
    };
    let ok = "PREFIX ex: <http://ex.org/> ex:S { ex:p . }";
    assert!(refused("ex:S {", "<urn:a>@START").starts_with("schema: line 1"));
    assert!(refused(ok, "{FOCUS a ex:C}@ex:Other").contains("does not define"));
    assert!(refused(ok, "<urn:a>@START").contains("START"));
    assert!(refused(ok, "SPARQL '''SELECT ?focus {}'''@ex:S").contains("SPARQL"));
    assert!(refused(ok, "").starts_with("shapeMap"));
    // EXTERNAL without a definition, imports nothing resolves
    assert!(
        refused("PREFIX ex: <http://ex.org/> ex:S EXTERNAL", "<urn:a>@ex:S").contains("schema")
    );
    assert!(
        refused(
            "PREFIX ex: <http://ex.org/> IMPORT <http://ex.org/other> ex:S { ex:p . }",
            "<urn:a>@ex:S"
        )
        .contains("import")
    );
    let mut c = cfg("reject", ok, "<urn:a>@ex:S");
    c.schema.format = Some("turtle".into());
    assert!(set_config(&store, Some(c), &NoImports).is_err());
    assert!(store.guard().is_none() && !store.guard_required());
}

/// The acceptance example: `reject` is refused while carol does not conform; once she
/// is fixed the guard rejects a person without a name, and skips writes it cannot be
/// affected by.
#[test]
fn reject_mode_over_the_acceptance_example() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = loaded(&root);
    let o = set_config(&s, Some(cfg("reject", SCHEMA, MAP)), &NoImports).unwrap();
    let SetOutcome::NotConforming(sum) = o else {
        panic!("carol does not conform")
    };
    assert_eq!((sum.blocking, sum.total), (1, 3));
    assert_eq!(sum.results[0]["node"]["value"], "http://ex.org/carol");
    assert!(!exists(&root, CONFIG_FILE) && s.guard().is_none());

    upd(
        &s,
        "DELETE DATA { ex:carol foaf:age 200 } ; INSERT DATA { ex:carol foaf:name \"Carol\" }",
    )
    .unwrap();
    let (g, sum) = installed(set_config(&s, Some(cfg("reject", SCHEMA, MAP)), &NoImports).unwrap());
    assert!(sum.conforms && sum.blocking == 0 && sum.total == 3);
    assert_eq!(sum.language, GuardLanguage::Shex);
    // the copy and the configuration
    assert_eq!(
        std::fs::read_to_string(root.join(SHEX_SCHEMA_SHEXC_FILE)).unwrap(),
        SCHEMA
    );
    let j = stored_config(&root);
    assert_eq!(
        (j["format"].as_u64(), j["language"].as_str()),
        (Some(2), Some("shex"))
    );
    assert_eq!(j["schema"]["file"], SHEX_SCHEMA_SHEXC_FILE);
    assert_eq!(j["schema"]["format"], "shexc");
    assert_eq!(j["schema"]["sha256"], sha256_hex(SCHEMA.as_bytes()));
    assert!(j["schema"].get("inline").is_none() && j.get("updated").is_some());
    let st = g.status();
    assert_eq!((st.shape_count, st.associations), (2, Some(3)));
    assert_eq!(st.baseline.as_ref().unwrap().conforms, Some(true));

    // a person without a name
    let Err(Error::Rejected(r)) = upd(&s, "INSERT DATA { ex:dave a ex:Person }") else {
        panic!("dave was accepted")
    };
    let v = &r.summary;
    assert_eq!(
        (v.language, v.status),
        (GuardLanguage::Shex, GuardStatus::Rejected)
    );
    assert_eq!((v.blocking, v.total, v.by_severity.violation), (1, 4, 1));
    assert_eq!(v.results.len(), 1);
    assert_eq!(v.results[0]["status"], "nonconformant");
    assert_eq!(v.results[0]["node"]["value"], "http://ex.org/dave");
    assert_eq!(v.results[0]["shape"]["value"], "http://ex.org/Person");
    assert!(
        v.results[0]["reason"]
            .as_str()
            .is_some_and(|r| !r.is_empty())
    );
    assert!(
        v.header().contains("lang=shex, blocking=1"),
        "{}",
        v.header()
    );
    assert!(
        r.to_string()
            .starts_with("ShEx validation failed: 1 nonconformant association;")
    );
    // a conforming write is validated, on the associations it affects
    let sum = summary(
        &upd(
            &s,
            "INSERT DATA { ex:dave a ex:Person ; foaf:name \"Dave\" }",
        )
        .unwrap(),
    );
    assert_eq!(
        (sum.status, sum.strategy, sum.total),
        (GuardStatus::Passed, Strategy::Incremental, 4)
    );
    // a named graph outside the data graph: skipped
    let sum = summary(&upd(&s, "INSERT DATA { GRAPH ex:g { ex:erin a ex:Person } }").unwrap());
    assert_eq!(
        (sum.status, sum.strategy),
        (GuardStatus::Skipped, Strategy::None)
    );
    assert!(sum.header().contains("strategy=none, lang=shex"));
    // a predicate no triple constraint mentions: validated all the same, since ex:Org is
    // CLOSED (its nodes may not have it)
    let sum = summary(&upd(&s, "INSERT DATA { ex:dave ex:nickname \"D\" }").unwrap());
    assert_eq!(sum.status, GuardStatus::Passed);
    // the inferred graph is not part of the data graph unless asked
    let sum = summary(
        &upd(
            &s,
            "INSERT DATA { GRAPH <urn:x-sparkles:inferred> { ex:erin a ex:Person } }",
        )
        .unwrap(),
    );
    assert_eq!(sum.status, GuardStatus::Skipped);
    let st = g.status();
    assert_eq!(
        (
            st.counters.rejected,
            st.counters.passed,
            st.counters.skipped
        ),
        (1, 2, 2)
    );
    assert_eq!(st.baseline.unwrap().commit, s.head_commit().seq);
}

#[test]
fn warn_mode_and_bulk_loads() {
    let dir = tempfile::tempdir().unwrap();
    let s = loaded(&dir.path().join("db"));
    // the head does not conform: warn is accepted
    let (g, sum) = installed(set_config(&s, Some(cfg("warn", SCHEMA, MAP)), &NoImports).unwrap());
    assert_eq!((sum.status, sum.blocking), (GuardStatus::Warned, 1));
    let sum = summary(&upd(&s, "INSERT DATA { ex:dave a ex:Person }").unwrap());
    assert_eq!((sum.status, sum.blocking), (GuardStatus::Warned, 2));
    assert_eq!(g.status().baseline.unwrap().conforms, Some(false));
    // a bulk load is validated too (anything may have changed)
    let r = s
        .load_as(
            &[Source::from_bytes(
                b"<http://ex.org/fay> a <http://ex.org/Person> .".to_vec(),
                RdfFormat::Turtle,
                None,
            )],
            CommitKind::Load,
        )
        .unwrap();
    assert_eq!(r.validation.unwrap().blocking, 3);
    // the report limit cuts the results, not the counts
    let mut c = cfg("warn", SCHEMA, MAP);
    c.report_limit = 1;
    let (_, sum) = installed(set_config(&s, Some(c), &NoImports).unwrap());
    assert_eq!(
        (sum.blocking, sum.results.len(), sum.truncated),
        (3, 1, true)
    );
}

#[test]
fn reject_mode_rejects_bulk_loads() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(&dir.path().join("db"));
    installed(set_config(&s, Some(cfg("reject", SCHEMA, MAP)), &NoImports).unwrap());
    let e = s.load(&[Source::from_bytes(
        b"<http://ex.org/fay> a <http://ex.org/Person> .".to_vec(),
        RdfFormat::Turtle,
        None,
    )]);
    assert!(matches!(e, Err(Error::Rejected(_))), "{e:?}");
    assert_eq!(s.snapshot().len(), 0);
}

/// Reopened, the guard is installed from the copy, without resolving anything: an
/// import resolved when the configuration was set is merged into a ShExJ copy, which
/// keeps the schema's prefixes for the shape map.
#[test]
fn imports_are_resolved_once_and_the_copy_reinstalls() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = loaded(&root);
    upd(&s, "INSERT DATA { ex:carol foaf:name \"Carol\" }").unwrap();
    upd(&s, "DELETE DATA { ex:carol foaf:age 200 }").unwrap();
    let common = crate::parse_schema(
        "PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
         <http://ex.org/Named> { foaf:name xsd:string }",
        None,
        None,
    )
    .unwrap();
    let resolver = FileResolver {
        inline: [("http://ex.org/common".to_string(), common)].into(),
        ..Default::default()
    };
    let schema = "PREFIX ex: <http://ex.org/> IMPORT <http://ex.org/common>
        ex:Person @ex:Named AND EXTRA a { a [ex:Person] }";
    // the map uses the schema's prefixes
    let (_, sum) = installed(set_config(&s, Some(cfg("reject", schema, MAP)), &resolver).unwrap());
    assert_eq!(sum.total, 3);
    assert!(!exists(&root, SHEX_SCHEMA_SHEXC_FILE) && exists(&root, SHEX_SCHEMA_SHEXJ_FILE));
    let j = stored_config(&root);
    assert_eq!(j["schema"]["file"], SHEX_SCHEMA_SHEXJ_FILE);
    assert_eq!(j["schema"]["format"], "shexj");
    assert_eq!(j["schema"]["prefixes"]["ex"], "http://ex.org/");
    assert_eq!(j["schema"]["sha256"], sha256_hex(schema.as_bytes()));
    let copy = std::fs::read_to_string(root.join(SHEX_SCHEMA_SHEXJ_FILE)).unwrap();
    assert!(copy.contains("http://ex.org/Named") && !copy.contains("imports"));
    drop(s);

    let s = open(&root);
    assert!(s.guard_required());
    assert!(matches!(
        upd(&s, "INSERT DATA { ex:x a ex:Person }"),
        Err(Error::GuardMissing(_))
    ));
    let g = install(&s).unwrap().unwrap();
    assert_eq!(g.status().shape_count, 2);
    assert!(matches!(
        upd(&s, "INSERT DATA { ex:x a ex:Person }"),
        Err(Error::Rejected(_))
    ));
    upd(&s, "INSERT DATA { ex:x a ex:Person ; foaf:name \"X\" }").unwrap();
    // no shape is CLOSED: a predicate no triple constraint mentions is not validated
    let sum = summary(&upd(&s, "INSERT DATA { ex:x ex:nickname \"X\" }").unwrap());
    assert_eq!(
        (sum.status, sum.strategy),
        (GuardStatus::Skipped, Strategy::None)
    );
    assert!(matches!(
        upd(&s, "INSERT DATA { ex:x foaf:name \"Y\" }"),
        Err(Error::Rejected(_))
    ));
    // the configuration given back without the text keeps the copy
    let mut c: ShexValidationConfig = serde_json::from_value(stored_config(&root)).unwrap();
    c.mode = GuardMode::Warn;
    let (g, _) = installed(set_config(&s, Some(c), &NoImports).unwrap());
    assert_eq!(g.config().schema.prefixes["ex"], "http://ex.org/");
    assert_eq!(stored_config(&root)["mode"], "warn");
    // a file outside the copies is never read
    let mut c = g.config().clone();
    c.schema.file = Some("../secret.shex".into());
    assert!(set_config(&s, Some(c), &NoImports).is_err());

    // a clone keeps the configuration and the copy
    let copy = dir.path().join("clone");
    s.clone_to(&copy, &Default::default()).unwrap();
    assert!(exists(&copy, SHEX_SCHEMA_SHEXJ_FILE));
    let c = open(&copy);
    assert_eq!(install(&c).unwrap().unwrap().config().mode, GuardMode::Warn);
}

#[test]
fn shexj_schemas_are_copied_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = loaded(&root);
    let shexj = r#"{"@context": "http://www.w3.org/ns/shex.jsonld", "type": "Schema",
      "shapes": [{"id": "http://ex.org/Org", "type": "Shape",
        "expression": {"type": "TripleConstraint", "predicate": "http://ex.org/city"}}]}"#;
    let c = config(serde_json::json!({"language": "shex", "mode": "reject",
        "schema": {"inline": shexj}, "shapeMap": [{"node": "{FOCUS a <http://ex.org/Org>}",
        "shape": "<http://ex.org/Org>"}]}))
    .unwrap();
    let (g, sum) = installed(set_config(&s, Some(c), &NoImports).unwrap());
    assert_eq!(sum.total, 1);
    assert_eq!(
        std::fs::read_to_string(root.join(SHEX_SCHEMA_SHEXJ_FILE)).unwrap(),
        shexj
    );
    assert_eq!(g.config().schema.format.as_deref(), Some("shexj"));
    assert!(g.config().schema.prefixes.is_empty());
    drop(s);
    let s = open(&root);
    install(&s).unwrap().unwrap();
    assert!(matches!(
        upd(&s, "INSERT DATA { ex:acme2 a ex:Org }"),
        Err(Error::Rejected(_))
    ));
}

/// Setting a ShEx configuration removes a SHACL one's files; turning validation off
/// removes every file, and a reopened store has no guard.
#[test]
fn switching_languages_and_turning_off() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = loaded(&root);
    std::fs::write(root.join(sparkles::guard::config::SHACL_SHAPES_FILE), "").unwrap();
    std::fs::write(
        root.join(CONFIG_FILE),
        r#"{"format":1,"mode":"warn","shapes":{"file":"validation-shapes.ttl"}}"#,
    )
    .unwrap();
    installed(set_config(&s, Some(cfg("warn", SCHEMA, MAP)), &NoImports).unwrap());
    assert!(!exists(&root, sparkles::guard::config::SHACL_SHAPES_FILE));
    assert!(exists(&root, SHEX_SCHEMA_SHEXC_FILE));
    assert_eq!(
        sparkles::guard::config::config_language(&root).unwrap(),
        Some(GuardLanguage::Shex)
    );
    assert!(matches!(
        set_config(&s, None, &NoImports).unwrap(),
        SetOutcome::Removed
    ));
    for f in sparkles::guard::config::FILES {
        assert!(!exists(&root, f), "{f}");
    }
    assert!(s.guard().is_none() && !s.guard_required());
    drop(s);
    let s = open(&root);
    assert!(install(&s).unwrap().is_none());
    assert!(!s.guard_required());
    upd(&s, "INSERT DATA { ex:dave a ex:Person }").unwrap();
}

#[test]
fn in_memory_stores_keep_no_files() {
    let s = Store::in_memory(StoreOptions::default());
    let (g, sum) = installed(set_config(&s, Some(cfg("reject", SCHEMA, MAP)), &NoImports).unwrap());
    assert_eq!(sum.total, 0);
    assert!(
        g.status()
            .warnings
            .iter()
            .any(|w| w.contains("selects no nodes"))
    );
    assert!(matches!(
        upd(&s, "INSERT DATA { ex:dave a ex:Person }"),
        Err(Error::Rejected(_))
    ));
    // the copy of an earlier configuration exists only on disk
    let mut c = g.config().clone();
    c.mode = GuardMode::Warn;
    assert!(set_config(&s, Some(c), &NoImports).is_err());
}

#[test]
fn closed_shapes_read_every_predicate() {
    let schema = crate::compile(
        &crate::parse_schema(SCHEMA, None, None).unwrap(),
        &NoImports,
    )
    .unwrap();
    let map = parse_map(&MapSource::Compact(MAP.into()), &schema).unwrap();
    // ex:Org is CLOSED
    assert_eq!(read_predicates(schema.ir(), &map), None);
    let open = crate::compile(
        &crate::parse_schema(
            "PREFIX ex: <http://ex.org/> ex:S { ex:p @ex:T ; ^ex:q . } ex:T { ex:r . }",
            None,
            None,
        )
        .unwrap(),
        &NoImports,
    )
    .unwrap();
    let map = parse_map(
        &MapSource::Compact("{FOCUS ex:s _}@ex:S, <http://ex.org/a>@ex:T".into()),
        &open,
    )
    .unwrap();
    let ex = |l: &str| format!("http://ex.org/{l}");
    assert_eq!(
        read_predicates(open.ir(), &map),
        Some(vec![ex("p"), ex("q"), ex("r"), ex("s")])
    );
}

// ---------------------------------------------------- the predicate skip ---------

const EX: &str = "http://ex.org/";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// A schema with recursion, negation, inverse constraints, value sets and a node that
/// is never in the base data (`ex:n9`); its triple constraints read `ex:p`, `ex:q` and
/// `rdf:type`, the map's selectors `rdf:type`, `ex:q` and `ex:s`.
const SKIP_SCHEMA: &str =
    "PREFIX ex: <http://ex.org/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
ex:S { ex:p @ex:T * ; ^ex:q . ? ; a [ex:C] ? }
ex:T { ex:p [ex:n1 ex:n2 ex:n9] ? ; ex:q xsd:integer * }
ex:U NOT @ex:S
ex:V [ex:n0 ex:n3 ex:n9]";
const SKIP_MAP: &str = "{FOCUS a ex:C}@ex:S, ex:n0@ex:T, ex:n9@ex:V, ex:n9@ex:U, \
     {FOCUS ex:q _}@ex:U, {FOCUS ex:s _}@ex:U, {ex:n1 ex:s FOCUS}@ex:T, ex:n4@ex:S";

/// Nodes `n0`…`n4` and `n9`; predicates `p`, `q`, `rdf:type`, `s` (read) and `r` (not
/// read); objects are nodes, integers, or `ex:C`.
fn node(i: u8) -> Term {
    let n = if i == 5 { 9 } else { i };
    NamedNode::new_unchecked(format!("{EX}n{n}")).into()
}

fn pred(i: u8) -> NamedNode {
    match i {
        0 => NamedNode::new_unchecked(format!("{EX}p")),
        1 => NamedNode::new_unchecked(format!("{EX}q")),
        2 => NamedNode::new_unchecked(RDF_TYPE),
        3 => NamedNode::new_unchecked(format!("{EX}s")),
        _ => NamedNode::new_unchecked(format!("{EX}r")),
    }
}

fn object(i: u8) -> Term {
    match i {
        0..=5 => node(i),
        6 => NamedNode::new_unchecked(format!("{EX}C")).into(),
        _ => Literal::from(i64::from(i) - 7).into(),
    }
}

type Triple = (u8, u8, u8);

fn triple() -> impl Gen<Value = Triple> {
    (0u8..6, 0u8..5, 0u8..10)
}

/// A change: insert or delete, mostly with a predicate the schema does not read.
fn change() -> impl Gen<Value = (bool, Triple)> {
    let pred = prop_oneof![3 => Just(4u8), 2 => 0u8..4];
    (any::<bool>(), (0u8..6, pred, 0u8..10))
}

fn write(s: &Store, changes: &[(bool, Triple)]) -> sparkles::Result<sparkles::commit::Receipt> {
    let mut t = s.write();
    for &(ins, (a, b, c)) in changes {
        let q = [
            t.intern(&node(a))?,
            t.intern(&pred(b).into())?,
            t.intern(&object(c))?,
            Id::DEFAULT_GRAPH,
        ];
        if ins {
            t.insert(q)?;
        } else {
            t.delete(q)?;
        }
    }
    t.commit()
}

/// (node, shape, status) of every association.
fn verdicts(g: &ShexGuard, snap: &Arc<Snapshot>) -> Vec<(Term, crate::ShapeLabel, Status)> {
    crate::validate(snap, &g.schema, &g.map, &ValidateOptions::default())
        .unwrap()
        .results
        .into_iter()
        .map(|r| (r.node, r.shape, r.status))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// A write the guard skips because no changed predicate is read never changes a
    /// verdict, and a write of only unread predicates is always skipped.
    #[test]
    fn the_predicate_skip_never_changes_a_verdict(
        base in proptest::collection::vec(triple(), 0..24),
        changes in proptest::collection::vec(change(), 1..6),
    ) {
        let s = Store::in_memory(StoreOptions::default());
        write(&s, &base.iter().map(|t| (true, *t)).collect::<Vec<_>>()).unwrap();
        let (g, _) = installed(
            set_config(&s, Some(cfg("warn", SKIP_SCHEMA, SKIP_MAP)), &NoImports).unwrap(),
        );
        prop_assert!(g.reads.is_some());
        let before = verdicts(&g, &s.snapshot());
        let r = write(&s, &changes).unwrap();
        let Some(v) = r.validation else {
            // no net change: nothing to validate
            return Ok(());
        };
        let unread = changes.iter().all(|(_, (_, p, _))| *p == 4);
        if unread {
            prop_assert_eq!(v.status, GuardStatus::Skipped);
        }
        if v.status == GuardStatus::Skipped {
            prop_assert_eq!(verdicts(&g, &s.snapshot()), before);
        }
    }
}

/// A ShExR schema is copied as ShExJ, with the Turtle's prefixes kept for the shape map,
/// and the copy reinstalls.
#[test]
fn shexr_schemas_are_copied_as_shexj() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = loaded(&root);
    upd(
        &s,
        "DELETE DATA { ex:carol foaf:age 200 } ; INSERT DATA { ex:carol foaf:name \"Carol\" }",
    )
    .unwrap();
    let turtle = crate::Schema::parse_shexc(SCHEMA, None)
        .unwrap()
        .to_shexr_turtle();
    assert!(turtle.contains("PREFIX ex: <http://ex.org/>"), "{turtle}");
    let c = config(serde_json::json!({"language": "shex", "mode": "reject",
        "schema": {"inline": turtle, "format": "shexr"}, "shapeMap": MAP}))
    .unwrap();
    // the map's ex: is the Turtle's
    let (g, sum) = installed(set_config(&s, Some(c), &NoImports).unwrap());
    assert!(sum.conforms && sum.total == 3, "{:?}", sum.results);
    assert!(!exists(&root, SHEX_SCHEMA_SHEXC_FILE) && exists(&root, SHEX_SCHEMA_SHEXJ_FILE));
    let j = stored_config(&root);
    assert_eq!(j["schema"]["format"], "shexj");
    assert_eq!(j["schema"]["prefixes"]["ex"], "http://ex.org/");
    assert_eq!(
        j["schema"]["prefixes"]["foaf"],
        "http://xmlns.com/foaf/0.1/"
    );
    assert_eq!(j["schema"]["sha256"], sha256_hex(turtle.as_bytes()));
    assert_eq!(g.status().shape_count, 2);
    // the copy is the schema
    let copy = std::fs::read_to_string(root.join(SHEX_SCHEMA_SHEXJ_FILE)).unwrap();
    let from_c = crate::Schema::parse_shexc(SCHEMA, None).unwrap();
    assert_eq!(
        crate::Schema::from_shexj(&copy).unwrap().to_shexj(),
        from_c.to_shexj()
    );
    drop(s);

    let s = open(&root);
    install(&s).unwrap().unwrap();
    assert!(matches!(
        upd(&s, "INSERT DATA { ex:dave a ex:Person }"),
        Err(Error::Rejected(_))
    ));
    upd(
        &s,
        "INSERT DATA { ex:dave a ex:Person ; foaf:name \"Dave\" }",
    )
    .unwrap();
    // a ShExR text that is not a schema is refused, and nothing changes
    let c = config(serde_json::json!({"language": "shex", "mode": "reject",
        "schema": {"inline": "<http://ex.org/a> <http://ex.org/b> 1 .", "format": "shexr"},
        "shapeMap": MAP}))
    .unwrap();
    let e = set_config(&s, Some(c), &NoImports).err().unwrap();
    assert!(format!("{e:#}").contains("sx:Schema"), "{e:#}");
    assert_eq!(stored_config(&root)["schema"]["format"], "shexj");
}

/// SPARQL selectors, which every validated write would run, are refused when the
/// configuration is set (compact or JSON maps, whatever the query), and nothing is
/// installed or written.
#[test]
fn sparql_selectors_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = loaded(&root);
    let selector = "SPARQL '''SELECT ?focus { ?focus a <http://ex.org/Person> }'''";
    for map in [
        serde_json::json!(format!("{selector}@ex:Person")),
        serde_json::json!(format!("ex:alice@ex:Person, {selector}@ex:Person")),
        serde_json::json!([{"node": selector, "shape": "http://ex.org/Person"}]),
    ] {
        let c = config(serde_json::json!({"language": "shex", "mode": "warn",
            "schema": {"inline": SCHEMA, "format": "shexc"}, "shapeMap": map}))
        .unwrap();
        let e = set_config(&s, Some(c), &NoImports).err().unwrap();
        let e = format!("{e:#}");
        assert!(
            e.contains("SPARQL node selectors are not allowed in write-time validation"),
            "{map}: {e}"
        );
    }
    // a selector that is not a SELECT query is a syntax error of the map
    let e = set_config(
        &s,
        Some(cfg("warn", SCHEMA, "SPARQL 'ASK {}'@ex:Person")),
        &NoImports,
    )
    .err()
    .unwrap();
    assert!(format!("{e:#}").starts_with("shapeMap: line 1"), "{e:#}");
    assert!(s.guard().is_none() && !s.guard_required());
    assert!(!exists(&root, CONFIG_FILE));
}

// ------------------------------------------------- incremental validation --------

/// Recursion through `ex:s` and an inverse reference, negation, value sets, and a node
/// that is never in the base data (`ex:n9`).
const INC_SCHEMA: &str =
    "PREFIX ex: <http://ex.org/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
ex:S { ex:p @ex:T * ; ^ex:q @ex:S ? ; a [ex:C] ? }
ex:T { ex:p [ex:n1 ex:n2 ex:n9] ? ; ex:q xsd:integer * ; ex:s @ex:S ? }
ex:U NOT @ex:S
ex:V [ex:n0 ex:n3 ex:n9]";

fn label_key(l: &crate::ShapeLabel) -> String {
    match l {
        crate::ShapeLabel::Iri(s) | crate::ShapeLabel::BNode(s) => s.clone(),
        crate::ShapeLabel::Start => "null".into(),
    }
}

fn term_key(t: &Term) -> String {
    match t {
        Term::NamedNode(n) => n.as_str().to_string(),
        Term::BlankNode(b) => b.as_str().to_string(),
        Term::Literal(l) => l.value().to_string(),
        Term::Triple(t) => t.to_string(),
    }
}

/// The nonconformant associations of a state, as `node|shape`.
fn nonconformant(v: &[(Term, crate::ShapeLabel, Status)]) -> Vec<String> {
    let mut out: Vec<String> = v
        .iter()
        .filter(|(_, _, s)| *s == Status::Nonconformant)
        .map(|(n, l, _)| format!("{}|{}", term_key(n), label_key(l)))
        .collect();
    out.sort();
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Every write's counts are those of a full validation, and every association the
    /// write makes nonconformant is listed, whether the guard validated incrementally
    /// or in full. In grandfather `reject` mode a write is rejected exactly when it
    /// introduces a nonconformant association, and `introduced` counts them.
    #[test]
    fn incremental_counts_equal_full_validation(
        base in proptest::collection::vec(triple(), 0..24),
        writes in proptest::collection::vec(proptest::collection::vec(change(), 1..5), 1..8),
        closed in any::<bool>(),
        grandfather in any::<bool>(),
    ) {
        let s = Store::in_memory(StoreOptions::default());
        write(&s, &base.iter().map(|t| (true, *t)).collect::<Vec<_>>()).unwrap();
        // a CLOSED shape makes every predicate read
        let schema = if closed {
            format!("{INC_SCHEMA}\nex:W CLOSED {{ ex:p . * }}")
        } else {
            INC_SCHEMA.to_string()
        };
        let map = format!("{SKIP_MAP}, ex:n2@ex:W");
        let map = if closed { map.as_str() } else { SKIP_MAP };
        let mut c = cfg(if grandfather { "reject" } else { "warn" }, &schema, map);
        if grandfather {
            c.baseline = BaselinePolicy::Grandfather;
        }
        let (g, _) = installed(set_config(&s, Some(c), &NoImports).unwrap());
        for w in &writes {
            let before = verdicts(&g, &s.snapshot());
            let head = s.head_commit().seq;
            let r = write(&s, w);
            let after = verdicts(&g, &s.snapshot());
            let (nb, na) = (nonconformant(&before), nonconformant(&after));
            let r = match r {
                Err(Error::Rejected(rej)) => {
                    prop_assert!(grandfather);
                    prop_assert_eq!(s.head_commit().seq, head);
                    prop_assert_eq!(&after, &before);
                    let v = rej.summary;
                    prop_assert!(v.introduced.unwrap() > 0);
                    // the first result listed is new
                    let first = &v.results[0];
                    let x = format!(
                        "{}|{}",
                        first["node"]["value"].as_str().unwrap_or_default(),
                        first["shape"]["value"].as_str().unwrap_or("null")
                    );
                    prop_assert!(!nb.contains(&x), "{} was not new", x);
                    continue;
                }
                r => r.unwrap(),
            };
            let Some(v) = r.validation else { continue };
            if v.status == GuardStatus::Skipped {
                prop_assert_eq!(&after, &before);
                continue;
            }
            prop_assert_eq!(v.blocking as usize, na.len());
            prop_assert_eq!(v.total as usize, after.len());
            prop_assert_eq!(g.status().baseline.unwrap().blocking as usize, na.len());
            if grandfather {
                prop_assert_eq!(v.introduced, Some(0));
                let new: Vec<&String> = na.iter().filter(|x| !nb.contains(x)).collect();
                prop_assert!(new.is_empty(), "committed with new {:?}", new);
            }
            let listed: Vec<String> = v
                .results
                .iter()
                .map(|r| {
                    format!(
                        "{}|{}",
                        r["node"]["value"].as_str().unwrap_or_default(),
                        r["shape"]["value"].as_str().unwrap_or("null")
                    )
                })
                .collect();
            for x in &listed {
                prop_assert!(na.contains(x), "listed {} is not nonconformant", x);
            }
            for x in na.iter().filter(|x| !nb.contains(x)) {
                prop_assert!(listed.contains(x), "new {} not listed ({:?})", x, v.strategy);
            }
        }
    }
}

/// Grandfather mode: `reject` is enabled while carol does not conform, writes that
/// leave her as she is pass, a write that adds a nonconformant person is rejected, and
/// the setting survives a restart.
#[test]
fn grandfather_mode_blocks_only_new_associations() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = loaded(&root);
    let mut c = cfg("reject", SCHEMA, MAP);
    c.baseline = BaselinePolicy::Grandfather;
    let (_, sum) = installed(set_config(&s, Some(c), &NoImports).unwrap());
    assert_eq!((sum.blocking, sum.introduced), (1, Some(0)));
    assert_eq!(stored_config(&root)["baseline"], "grandfather");
    let v = summary(
        &upd(
            &s,
            "INSERT DATA { ex:dave a ex:Person ; foaf:name \"Dave\" }",
        )
        .unwrap(),
    );
    assert_eq!(
        (v.status, v.strategy, v.blocking, v.introduced),
        (GuardStatus::Passed, Strategy::Incremental, 1, Some(0))
    );
    // carol's own write keeps her nonconformant: nothing new
    let v = summary(&upd(&s, "INSERT DATA { ex:carol foaf:knows ex:dave }").unwrap());
    assert_eq!((v.status, v.introduced), (GuardStatus::Passed, Some(0)));
    let Err(Error::Rejected(r)) = upd(&s, "INSERT DATA { ex:erin a ex:Person }") else {
        panic!("erin has no name")
    };
    assert_eq!((r.summary.blocking, r.summary.introduced), (2, Some(1)));
    assert_eq!(r.summary.results[0]["node"]["value"], "http://ex.org/erin");
    assert!(
        r.to_string().contains("1 new nonconformant association;"),
        "{r}"
    );
    drop(s);
    let s = open(&root);
    install(&s).unwrap().unwrap();
    assert!(matches!(
        upd(&s, "INSERT DATA { ex:erin a ex:Person }"),
        Err(Error::Rejected(_))
    ));
    // a full validation (the state unknown) compares with the state before the write
    s.guard().unwrap().bypassed();
    let v = summary(&upd(&s, "INSERT DATA { ex:fay a ex:Person ; foaf:name \"F\" }").unwrap());
    assert_eq!(
        (v.strategy, v.fallback.as_deref(), v.introduced),
        (Strategy::Full, Some("baseline"), Some(0))
    );
}

/// Writes after enabling are incremental; the state survives a restart; a bulk load
/// and an unknown state fall back to a full validation.
#[test]
fn writes_are_validated_incrementally() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = loaded(&root);
    let (_, sum) = installed(set_config(&s, Some(cfg("warn", SCHEMA, MAP)), &NoImports).unwrap());
    assert_eq!((sum.blocking, sum.total), (1, 3));
    let v = summary(&upd(&s, "INSERT DATA { ex:dave a ex:Person }").unwrap());
    assert_eq!(
        (v.strategy, v.blocking, v.total, v.focus_nodes),
        (Strategy::Incremental, 2, 4, Some(1))
    );
    // bob knows dave, who does not conform: bob, and alice who knows bob, are revalidated
    let v = summary(&upd(&s, "INSERT DATA { ex:bob foaf:knows ex:dave }").unwrap());
    assert_eq!((v.strategy, v.blocking), (Strategy::Incremental, 4));
    assert!(v.focus_nodes.unwrap() >= 2, "{v:?}");
    drop(s);
    let s = open(&root);
    let g = install(&s).unwrap().unwrap();
    assert_eq!(g.status().baseline.unwrap().blocking, 4);
    let v = summary(&upd(&s, "INSERT DATA { ex:dave foaf:name \"Dave\" }").unwrap());
    assert_eq!((v.strategy, v.blocking), (Strategy::Incremental, 1));
    // an unknown state: the next write is validated in full
    g.bypassed();
    let v = summary(&upd(&s, "INSERT DATA { ex:erin a ex:Person ; foaf:name \"E\" }").unwrap());
    assert_eq!(
        (v.strategy, v.fallback.as_deref(), v.blocking),
        (Strategy::Full, Some("baseline"), 1)
    );
}
