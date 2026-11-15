//! `reasoning=auto` (spec F17 §4, acceptance examples A4 and A5): a fresh
//! materialization is read, a stale one is never read silently, and a pattern whose
//! facts the materialization lacks is derived.

#![cfg(feature = "reasoning")]

use sparkles::Dataset;
use sparkles::io::RdfFormat;
use sparkles::reasoning::reasoner::Profile;
use sparkles::reasoning::{Demand, QuerySetting, ReasonRequest, ReasoningMode};
use sparkles::sparql::{PlanInfo, QueryOptions};

const Q: &str = "PREFIX ex: <http://ex.org/> SELECT ?x { ?x a ex:B } ORDER BY ?x";

fn open() -> (tempfile::TempDir, Dataset) {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path().join("db")).unwrap();
    ds.load_str(
        "@prefix ex: <http://ex.org/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:C rdfs:subClassOf ex:B . ex:x a ex:C . ex:z a ex:B .",
        RdfFormat::Turtle,
    )
    .unwrap();
    (dir, ds)
}

fn materialize(ds: &Dataset, profile: Profile) {
    ds.reasoning()
        .run(&ReasonRequest {
            profile,
            ..Default::default()
        })
        .unwrap();
}

fn auto(ds: &Dataset, profile: Option<Profile>) -> QueryOptions {
    ds.inference_view(ReasoningMode::Auto(Demand {
        profile,
        max_derived: None,
    }))
    .unwrap()
    .query_options()
}

/// The answer, the `Sparkles-Reasoning` header and whether the query read the inferred
/// graph.
fn answers(ds: &Dataset, opts: &QueryOptions) -> (Vec<String>, Option<String>, bool) {
    let r = ds.query_with(Q, opts).unwrap();
    let rows: Vec<String> = r
        .rows()
        .unwrap()
        .into_iter()
        .map(|row| row.values()[0].as_ref().unwrap().to_string())
        .collect();
    let applied = r.reasoning().cloned().expect("auto applies");
    (rows, applied.header(), applied.reads_inferred())
}

/// The `Reason` node of an EXPLAIN and its `Derive` children.
fn reason_node(ds: &Dataset, opts: &QueryOptions) -> PlanInfo {
    let (_, plan) = ds.explain(Q, opts).unwrap();
    assert_eq!(plan.operator, "Reason", "{plan:?}");
    plan
}

fn counter<'a>(node: &'a PlanInfo, name: &str) -> &'a serde_json::Value {
    node.counters
        .as_ref()
        .and_then(|c| c.get(name))
        .unwrap_or(&serde_json::Value::Null)
}

fn derive_nodes(plan: &PlanInfo) -> Vec<&PlanInfo> {
    plan.children
        .iter()
        .filter(|c| c.operator == "Derive")
        .collect()
}

/// A4 and A5: a fresh materialization is read without a `Sparkles-Reasoning` header,
/// and a stale one is not read, so the answer includes the new instance and the header
/// says what was derived. A re-run makes it fresh again.
#[test]
fn auto_reads_a_fresh_materialization_and_derives_over_a_stale_one() {
    let (_dir, ds) = open();
    materialize(&ds, Profile::Rdfs);
    let opts = auto(&ds, None);
    let (rows, header, read) = answers(&ds, &opts);
    assert_eq!(rows, ["<http://ex.org/x>", "<http://ex.org/z>"]);
    assert_eq!(
        header, None,
        "a fresh materialization answers without derivation"
    );
    assert!(read);
    let applied = ds
        .query_with(Q, &opts)
        .unwrap()
        .reasoning()
        .cloned()
        .unwrap();
    assert!(
        applied.nodes.is_empty() && applied.report.tables == 0,
        "rdfs holds no backward rule, so the query starts no session: {applied:?}"
    );
    let plan = reason_node(&ds, &opts);
    assert_eq!(counter(&plan, "materialization"), "fresh");
    let nodes = derive_nodes(&plan);
    assert_eq!(nodes.len(), 1);
    assert_eq!(counter(nodes[0], "source"), "materialized");
    assert!(counter(nodes[0], "readCost").is_number(), "{:?}", nodes[0]);
    assert!(
        counter(nodes[0], "deriveCost").is_number(),
        "{:?}",
        nodes[0]
    );
    assert!(
        counter(nodes[0], "readCost").as_f64() <= counter(nodes[0], "deriveCost").as_f64(),
        "{:?}",
        nodes[0]
    );

    // A4: the insert makes the materialization stale
    ds.update("PREFIX ex: <http://ex.org/> INSERT DATA { ex:y a ex:C }")
        .unwrap();
    let (rows, header, read) = answers(&ds, &opts);
    assert_eq!(
        rows,
        [
            "<http://ex.org/x>",
            "<http://ex.org/y>",
            "<http://ex.org/z>"
        ]
    );
    let header = header.expect("derived");
    assert!(header.starts_with("auto; derived="), "{header}");
    assert!(!read);
    let plan = reason_node(&ds, &opts);
    assert_eq!(counter(&plan, "materialization"), "not read");
    assert!(plan.description.contains("stale"), "{}", plan.description);
    let nodes = derive_nodes(&plan);
    assert_eq!(counter(nodes[0], "source"), "derived");
    assert!(
        counter(nodes[0], "reason")
            .as_str()
            .is_some_and(|r| r.contains("stale")),
        "{:?}",
        nodes[0]
    );

    // A5: after a re-run the inferred graph is read again
    materialize(&ds, Profile::Rdfs);
    let (rows, header, read) = answers(&ds, &opts);
    assert_eq!(
        rows,
        [
            "<http://ex.org/x>",
            "<http://ex.org/y>",
            "<http://ex.org/z>"
        ]
    );
    assert_eq!(header, None);
    assert!(read);
}

/// A materialization with another profile, or with rules the query's program lacks, is
/// not read: a top-level backward rule is skipped by the materializer, so a pattern in
/// its cone is derived while the others read.
#[test]
fn auto_derives_what_the_materialization_cannot_hold() {
    let (_dir, ds) = open();
    materialize(&ds, Profile::Rdfs);
    // another profile: never read (spec F17 §2.2)
    let owl = auto(&ds, Some(Profile::OwlRl));
    let (rows, header, read) = answers(&ds, &owl);
    assert_eq!(rows, ["<http://ex.org/x>", "<http://ex.org/z>"]);
    assert!(header.is_some(), "derived under another profile");
    assert!(!read);
    let plan = reason_node(&ds, &owl);
    assert!(plan.description.contains("profile"), "{}", plan.description);

    // forward and backward rules: the materialization holds the forward rule's facts
    let rules = "@prefix ex: <http://ex.org/>.
        [fwd: (?x rdf:type ex:C) -> (?x rdf:type ex:B)]
        [bwd: (?x ex:knows ?y) <- (?x rdf:type ex:B), (?y rdf:type ex:B)]";
    materialize(&ds, Profile::Rules(rules.to_string()));
    let opts = auto(&ds, None);
    let (rows, header, read) = answers(&ds, &opts);
    assert_eq!(rows, ["<http://ex.org/x>", "<http://ex.org/z>"]);
    assert_eq!(header, None);
    assert!(read);
    let both =
        "PREFIX ex: <http://ex.org/> SELECT ?x ?y { ?x a ex:B . ?x ex:knows ?y } ORDER BY ?x ?y";
    let r = ds.query_with(both, &opts).unwrap();
    assert_eq!(r.len(), 4, "every pair of B instances knows each other");
    let applied = r.reasoning().cloned().unwrap();
    assert_eq!(applied.read, 1, "the type pattern reads");
    assert_eq!(applied.report.tables, 1, "the knows pattern derives");
    let (_, plan) = ds.explain(both, &opts).unwrap();
    let knows = derive_nodes(&plan)
        .into_iter()
        .find(|n| n.description.contains("knows"))
        .expect("a Derive node for ex:knows");
    assert_eq!(counter(knows, "source"), "derived");
    assert!(
        counter(knows, "reason")
            .as_str()
            .is_some_and(|r| r.contains("backward")),
        "{knows:?}"
    );
}

/// The dataset setting (spec F17 §2.2) names the default mode, the profile and whether
/// a stale materialization may be read, and survives a reopen.
#[test]
fn the_query_setting_names_the_default_mode_and_the_stale_policy() {
    let (dir, ds) = open();
    assert_eq!(ds.default_reasoning_mode(), ReasoningMode::Materialized);
    assert!(ds.query_reasoning().is_none());
    let bad = ds
        .set_query_reasoning(Some(QuerySetting {
            default: Some("sometimes".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert!(bad.to_string().contains("default must be"), "{bad}");
    ds.set_query_reasoning(Some(QuerySetting {
        default: Some("auto".into()),
        profile: Some("rdfs".into()),
        stale: Some("allow".into()),
        ..Default::default()
    }))
    .unwrap();
    assert!(matches!(
        ds.default_reasoning_mode(),
        ReasoningMode::Auto(_)
    ));
    // the profile comes from the setting, so no materialization is needed to derive
    let mut opts = ds
        .inference_view(ds.default_reasoning_mode())
        .unwrap()
        .query_options();
    let (rows, header, _) = answers(&ds, &opts);
    assert_eq!(rows, ["<http://ex.org/x>", "<http://ex.org/z>"]);
    assert!(header.is_some(), "no materialization: derived");
    let plan = reason_node(&ds, &opts);
    assert!(
        plan.description.contains("no materialization"),
        "{}",
        plan.description
    );

    // a stale materialization is read when the setting allows it, and the plan says so.
    // The options are made again, since those above were made before the dataset had an
    // inferred graph to read.
    materialize(&ds, Profile::Rdfs);
    ds.update("PREFIX ex: <http://ex.org/> INSERT DATA { ex:y a ex:C }")
        .unwrap();
    opts = ds
        .inference_view(ds.default_reasoning_mode())
        .unwrap()
        .query_options();
    let (rows, header, read) = answers(&ds, &opts);
    assert_eq!(
        rows,
        ["<http://ex.org/x>", "<http://ex.org/z>"],
        "read stale"
    );
    assert_eq!(header, None);
    assert!(read);
    let plan = reason_node(&ds, &opts);
    assert_eq!(counter(&plan, "materialization"), "stale, read");
    let applied = ds
        .query_with(Q, &opts)
        .unwrap()
        .reasoning()
        .cloned()
        .unwrap();
    assert!(applied.reads_inferred());

    // the setting is kept in the dataset's directory; the options hold the dataset
    // through the auto reasoner, so they go first
    drop(applied);
    drop(opts);
    drop(ds);
    let ds = Dataset::open(dir.path().join("db")).unwrap();
    let s = ds.query_reasoning().expect("kept");
    assert_eq!(s.default.as_deref(), Some("auto"));
    assert!(s.stale_allowed());
    ds.set_query_reasoning(None).unwrap();
    assert_eq!(ds.default_reasoning_mode(), ReasoningMode::Materialized);
    drop(ds);
    let ds = Dataset::open(dir.path().join("db")).unwrap();
    assert!(ds.query_reasoning().is_none());
}

/// `budget.maxMillis` of the setting stops a derivation that runs longer, with the
/// reasoning budget's code, and `budget.maxDerived` applies when the query names none.
#[test]
fn the_setting_budget_bounds_a_derivation() {
    let (_dir, ds) = open();
    ds.set_query_reasoning(Some(QuerySetting {
        profile: Some("rdfs".into()),
        budget: sparkles::reasoning::setting::Budget {
            max_derived: Some(1),
            max_millis: None,
            max_union: None,
        },
        ..Default::default()
    }))
    .unwrap();
    // two instances of ex:B derive two facts, one more than the budget
    ds.update("PREFIX ex: <http://ex.org/> INSERT DATA { ex:y a ex:C }")
        .unwrap();
    let opts = ds
        .inference_view(ReasoningMode::Demand(Demand::default()))
        .unwrap()
        .query_options();
    let e = match ds.query_with(Q, &opts) {
        Err(e) => e,
        Ok(_) => panic!("the budget of one derived fact was not applied"),
    };
    assert_eq!(e.code(), "reasoning-budget", "{e}");
    ds.set_query_reasoning(Some(QuerySetting {
        profile: Some("rdfs".into()),
        budget: sparkles::reasoning::setting::Budget {
            max_derived: None,
            max_millis: Some(0),
            max_union: None,
        },
        ..Default::default()
    }))
    .unwrap_err();
}

/// A dataset of `data` materialized with the Jena rules `rules`, and its answers to
/// `query` under `auto` and under `demand`, sorted.
fn auto_and_demand(data: &str, rules: &str, query: &str) -> (Vec<String>, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path().join("db")).unwrap();
    ds.load_str(data, RdfFormat::Turtle).unwrap();
    materialize(&ds, Profile::Rules(rules.to_string()));
    let rows = |opts: &QueryOptions| {
        let mut rows: Vec<String> = ds
            .query_with(query, opts)
            .unwrap()
            .rows()
            .unwrap()
            .into_iter()
            .map(|row| {
                row.values()
                    .iter()
                    .map(|t| t.as_ref().map_or("UNDEF".into(), |t| t.to_string()))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        rows.sort();
        rows
    };
    let demand = ds
        .inference_view(ReasoningMode::Demand(Demand::default()))
        .unwrap()
        .query_options();
    (rows(&auto(&ds, None)), rows(&demand))
}

/// A forward rule that walks a list which backward rules make reads a cone the
/// materialization lacks, although none of its atoms names the list (review round 1).
#[test]
fn auto_follows_the_lists_a_builtin_reads_into_backward_rules() {
    let rules = "@prefix ex: <http://ex.org/>.
        [first: (?l rdf:first ?v) <- (?l ex:head ?v)]
        [rest: (?l rdf:rest rdf:nil) <- (?l ex:head ?v)]
        [member: (?s ex:items ?l), listMember(?l, ?m) -> (?s ex:member ?m)]";
    let (auto, demand) = auto_and_demand(
        "@prefix ex: <http://ex.org/> . ex:s ex:items ex:l . ex:l ex:head ex:a .",
        rules,
        "PREFIX ex: <http://ex.org/> SELECT ?m { ex:s ex:member ?m }",
    );
    assert_eq!(demand, ["<http://ex.org/a>"]);
    assert_eq!(auto, demand);
}

/// A materialization that skips a backward rule can hold a conclusion that `noValue`
/// draws from the rule's absence, which the whole program does not, so `auto` does not
/// read it beside derived facts (review round 1).
#[test]
fn auto_does_not_mix_a_materialization_with_negation_over_backward_rules() {
    let rules = "@prefix ex: <http://ex.org/>.
        [blocked: (?x ex:q ?y) <- (?x ex:seed ?y)]
        [eligible: (?x ex:seed ?y), noValue(?x, ex:q, ?y) -> (?x ex:r ?y)]";
    let (auto, demand) = auto_and_demand(
        "@prefix ex: <http://ex.org/> . ex:a ex:seed ex:b .",
        rules,
        "PREFIX ex: <http://ex.org/> SELECT ?p { ex:a ?p ex:b }",
    );
    assert!(
        !demand.contains(&"<http://ex.org/r>".to_string()),
        "{demand:?}"
    );
    assert_eq!(auto, demand);
}

/// `makeTemp`, `makeSkolem` and a blank node in a head make a new node on every run, so
/// the materialization's node and a derivation's node would both answer (review rounds 1
/// and 2).
#[test]
fn auto_does_not_mix_fresh_witnesses() {
    for make in [
        "[f: (?x ex:seed ?v), makeTemp(?n) -> (?x ex:node ?n)]",
        "[f: (?x ex:seed ?v), makeSkolem(?n, ?x, ?v) -> (?x ex:node ?n)]",
        "[f: (?x ex:seed ?v) -> (?x ex:node _:w)]",
    ] {
        let rules = format!(
            "@prefix ex: <http://ex.org/>.
            {make}
            [b: (?x ex:answer ?n) <- (?x ex:node ?n)]"
        );
        let (auto, demand) = auto_and_demand(
            "@prefix ex: <http://ex.org/> . ex:a ex:seed ex:b .",
            &rules,
            "PREFIX ex: <http://ex.org/> SELECT ?n { ex:a ex:node ?n . ex:a ex:answer ?answer }",
        );
        assert_eq!(demand.len(), 1, "{make}: {demand:?}");
        assert_eq!(auto.len(), demand.len(), "{make}: {auto:?}");
    }
}

/// A list builtin reads the list links that backward rules can add, so the list the
/// materialization saw can differ from the one a derivation sees and both lengths would
/// answer (review round 2).
#[test]
fn auto_does_not_mix_lists_that_backward_rules_extend() {
    let rules = "@prefix ex: <http://ex.org/>.
        [early: (?l rdf:rest ex:t) <- (?l ex:seed ex:a)]
        [delay: (?l ex:seed ex:a) -> (?l ex:step ex:a)]
        [links: (?l ex:step ex:a) -> (?l rdf:first ex:a), (?l rdf:rest rdf:nil), (?l ex:ready ex:yes)]
        [length: (?l ex:ready ex:yes), listLength(?l, ?n) -> (?l ex:length ?n)]";
    let (auto, demand) = auto_and_demand(
        "@prefix ex: <http://ex.org/> . ex:l ex:seed ex:a .
         ex:t <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> ex:b ;
              <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> <http://www.w3.org/1999/02/22-rdf-syntax-ns#nil> .",
        rules,
        "PREFIX ex: <http://ex.org/> SELECT ?n { ex:l ex:length ?n }",
    );
    assert_eq!(auto, demand);
}

/// A query whose dataset leaves out the default graph reads a fresh materialization
/// under `auto`, and EXPLAIN describes it as the query runs (review round 1).
#[test]
fn explain_agrees_with_a_read_that_derives_nothing() {
    let (_dir, ds) = open();
    materialize(&ds, Profile::Rdfs);
    let opts = auto(&ds, None);
    let q = "SELECT ?s FROM <urn:g> { ?s ?p ?o }";
    ds.query_with(q, &opts).unwrap();
    let (_, plan) = ds.explain(q, &opts).unwrap();
    assert_eq!(plan.operator, "Reason", "{plan:?}");
}

/// Concurrent changes of the setting leave the setting in memory equal to the one a
/// reopen reads (review round 1).
#[test]
fn concurrent_setting_changes_keep_memory_and_file_equal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let ds = Dataset::open(&path).unwrap();
    std::thread::scope(|scope| {
        for t in 0..4 {
            let ds = ds.clone();
            scope.spawn(move || {
                for i in 0..50 {
                    let setting = if (i + t) % 3 == 0 {
                        None
                    } else {
                        Some(QuerySetting {
                            default: Some(if (i + t) % 2 == 0 { "auto" } else { "demand" }.into()),
                            budget: sparkles::reasoning::setting::Budget {
                                max_derived: Some(1 + i * 10 + t),
                                max_millis: None,
                                max_union: None,
                            },
                            ..Default::default()
                        })
                    };
                    ds.set_query_reasoning(setting).unwrap();
                }
            });
        }
    });
    let live = ds.query_reasoning();
    let mut on_disk = sparkles::reasoning::setting::read(&path);
    // the file carries its format version, which the setting in memory leaves unset
    if let Some(s) = on_disk.as_mut() {
        s.query_reasoning_format = 0;
    }
    assert_eq!(live, on_disk);
}
