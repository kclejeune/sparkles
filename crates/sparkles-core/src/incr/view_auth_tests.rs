//! Who reads a view (spec F13 §6.1): containment of grant sets, the grant that names a
//! view, the cache of decisions per caller and view, and their audit records.

use super::runtime::{Host, Runtime, RuntimeOptions, covers};
use super::views::ViewDef;
use super::{ViewAccessEvent, ViewDecision, ViewOp};
use crate::access::{
    Caller, GraphAccess, GraphRule, Graphs, Limits, Protection, Rule, TripleRules, ViewGrants,
};
use crate::io::RdfFormat;
use crate::sparql::viewsvc::ViewRead;
use crate::store::{Store, StoreOptions};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

mod eval;

const PREFIX: &str = "PREFIX : <http://ex/> PREFIX org: <http://www.w3.org/ns/org#> ";
const DATA: &str = r#"
@prefix : <http://ex/> .
@prefix org: <http://www.w3.org/ns/org#> .
:a org:memberOf :t1 . :b org:memberOf :t1 . :c org:memberOf :t2 .
"#;
const TEAM_SIZES: &str =
    "SELECT ?team (COUNT(?p) AS ?n) WHERE { ?p org:memberOf ?team } GROUP BY ?team";
/// A graph whose name must never reach a refusal or an audit record.
const SECRET: &str = "http://ex/secret-graph-7f3a";
const INFERRED: &str = crate::INFERRED_GRAPH;

fn q(s: &str) -> String {
    format!("{PREFIX}{s}")
}

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[crate::io::Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .expect("load");
    s
}

fn rule(names: &[&str]) -> Graphs {
    Graphs::Only(GraphRule::new(names.iter().copied(), &[INFERRED]))
}

fn reads(names: &[&str]) -> Arc<GraphAccess> {
    Arc::new(GraphAccess::graphs(rule(names), Graphs::none()))
}

fn granted(names: &[&str], views: &[&str], who: &str) -> Arc<GraphAccess> {
    Arc::new(GraphAccess {
        views: ViewGrants::new(views.iter().map(|v| v.to_string()), Some(who)),
        ..GraphAccess::graphs(rule(names), Graphs::none())
    })
}

fn protection(name: &str, pattern: Option<&str>) -> Arc<Protection> {
    Arc::new(Protection {
        name: name.into(),
        predicates: Some(vec!["http://ex/age".into()]),
        classes: None,
        subclasses: false,
        graphs: None,
        pattern: pattern.map(str::to_string),
        prefixes: Default::default(),
        hide_inferences: false,
    })
}

fn protected(
    names: &[&str],
    p: &Arc<Protection>,
    lifted: Graphs,
    caller: Caller,
) -> Arc<GraphAccess> {
    Arc::new(GraphAccess::with_triples(
        rule(names),
        Graphs::none(),
        TripleRules {
            rules: vec![Rule {
                protection: p.clone(),
                read: lifted,
                write: Graphs::none(),
            }],
            caller,
            limits: Limits::default(),
        },
    ))
}

/// A policy whose principals and epoch tests change, recording the audit events.
#[derive(Default)]
struct Policy {
    principals: Mutex<HashMap<String, Option<Arc<GraphAccess>>>>,
    epoch: AtomicU64,
    events: Mutex<Vec<String>>,
}

impl Policy {
    fn with(principals: &[(&str, Option<Arc<GraphAccess>>)]) -> Arc<Policy> {
        let p = Policy::default();
        for (n, a) in principals {
            p.principals.lock().insert(n.to_string(), a.clone());
        }
        Arc::new(p)
    }

    fn events(&self) -> Vec<String> {
        std::mem::take(&mut *self.events.lock())
    }
}

impl Host for Policy {
    fn access(&self, principal: Option<&str>) -> crate::Result<Option<Arc<GraphAccess>>> {
        match principal {
            None => Ok(None),
            Some(p) => self
                .principals
                .lock()
                .get(p)
                .cloned()
                .ok_or_else(|| crate::Error::NotFound(format!("no principal {p}"))),
        }
    }
    fn stored_query(&self, _: &str) -> crate::Result<String> {
        Err(crate::Error::NotFound("none".into()))
    }
    fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }
    fn view_access(&self, e: &ViewAccessEvent<'_>) {
        self.events.lock().push(format!(
            "{} {} {} {} cached={} {}",
            e.view,
            e.op.as_str(),
            e.decision.as_str(),
            e.decision.reason(),
            e.cached,
            e.caller.unwrap_or("-")
        ));
    }
}

fn runtime(s: &Store, policy: &Arc<Policy>) -> Arc<Runtime> {
    let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
    rt.set_host(policy.clone());
    rt
}

fn define(rt: &Runtime, s: &Store, name: &str, principal: &str) {
    let mut def = ViewDef::table(q(TEAM_SIZES));
    def.principal = Some(principal.into());
    rt.put(s, name, def).unwrap();
    rt.round(s);
}

fn read_at(
    rt: &Runtime,
    s: &Store,
    name: &str,
    caller: Option<&Arc<GraphAccess>>,
    deadline: Option<Instant>,
) -> crate::Result<usize> {
    let snap = s.snapshot();
    let r = rt.read(&ViewRead {
        name,
        at: snap.commit,
        snap: Some(&snap),
        allow_stale: false,
        caller,
        deadline,
        aggregate_errors: None,
        memory_parent: None,
        memory_pool: None,
        max_memory_bytes: None,
    })?;
    Ok(r.rows.len())
}

fn read(rt: &Runtime, s: &Store, name: &str, caller: &Arc<GraphAccess>) -> crate::Result<usize> {
    read_at(rt, s, name, Some(caller), None)
}

fn code(e: &crate::Error) -> String {
    match e {
        crate::Error::Component(c) => c.code.clone(),
        e => format!("{e:?}"),
    }
}

/// A caller whose grants contain the principal's reads the view, and one whose grants
/// leave out a graph the principal reads is refused.
#[test]
fn contained_callers_read_the_view() {
    let s = store();
    let policy = Policy::with(&[("reports", Some(reads(&["default"])))]);
    let rt = runtime(&s, &policy);
    define(&rt, &s, "team-sizes", "reports");
    let wider = reads(&["default", "http://ex/g1"]);
    assert_eq!(read(&rt, &s, "team-sizes", &wider).unwrap(), 2);
    assert!(rt.readable_by("team-sizes", Some(&wider)));
    let narrow = reads(&["http://ex/g1", SECRET]);
    let e = read(&rt, &s, "team-sizes", &narrow).unwrap_err();
    assert_eq!(code(&e), "view-access");
    assert!(!e.to_string().contains(SECRET), "{e}");
    assert!(!rt.readable_by("team-sizes", Some(&narrow)));
    // the same grants as the principal's, and every graph
    assert_eq!(
        read(&rt, &s, "team-sizes", &reads(&["default"])).unwrap(),
        2
    );
    assert_eq!(read_at(&rt, &s, "team-sizes", None, None).unwrap(), 2);
}

/// Containment by each dimension of a grant set: one near miss and one cover each.
#[test]
fn containment_follows_every_dimension_of_the_grants() {
    let n = |names: &[&str]| Some(reads(names));
    type Case = (
        &'static str,
        Option<Arc<GraphAccess>>,
        Option<Arc<GraphAccess>>,
        bool,
    );
    let cases: Vec<Case> = vec![
        (
            "superset",
            n(&["default", "http://ex/a"]),
            n(&["default"]),
            true,
        ),
        ("default graph", n(&["http://ex/a"]), n(&["default"]), false),
        (
            "exact graph",
            n(&["http://ex/a"]),
            n(&["http://ex/b"]),
            false,
        ),
        (
            "pattern covers iri",
            n(&["http://ex/*"]),
            n(&["http://ex/b"]),
            true,
        ),
        (
            "pattern covers pattern",
            n(&["http://ex/*"]),
            n(&["http://ex/b/*"]),
            true,
        ),
        (
            "narrower pattern",
            n(&["http://ex/b/*"]),
            n(&["http://ex/*"]),
            false,
        ),
        (
            "other pattern",
            n(&["http://ex/*"]),
            n(&["http://other/*"]),
            false,
        ),
        (
            "principal reads everything",
            n(&["*", "default"]),
            None,
            false,
        ),
        (
            "inferred graph only by name",
            n(&["*", "default"]),
            n(&[INFERRED]),
            false,
        ),
        (
            "inferred graph named",
            n(&["*", "default", INFERRED]),
            n(&[INFERRED, "default"]),
            true,
        ),
    ];
    for (what, caller, view, want) in cases {
        assert_eq!(covers(caller.as_ref(), view.as_ref()), want, "{what}");
    }
    let all = || Arc::new(GraphAccess::all());
    let derived = |g: Graphs| Some(Arc::new(GraphAccess::graphs(g, Graphs::none())));
    // blank-node graph names: a set made from every graph holds them, patterns do not
    let blank = derived(Graphs::All.without_iri("http://ex/x"));
    assert!(!covers(n(&["*", "default"]).as_ref(), blank.as_ref()));
    assert!(covers(blank.as_ref(), blank.as_ref()));
    // graphs taken out of a pattern
    let excluded = derived(Graphs::All.without_pattern("http://ex/secret/*"));
    assert!(!covers(excluded.as_ref(), n(&["http://ex/*"]).as_ref()));
    assert!(covers(
        excluded.as_ref(),
        n(&["http://ex/public/*"]).as_ref()
    ));
    assert!(!covers(
        excluded.as_ref(),
        n(&["http://ex/secret/a"]).as_ref()
    ));
    assert!(covers(
        excluded.as_ref(),
        n(&["default", "http://ex/a"]).as_ref()
    ));
    // a guard keeps its graphs out of the caller's view
    let guarded = derived(Graphs::All.guarded("http://ex/reports/*", &[]));
    assert!(!covers(guarded.as_ref(), Some(&all())));
    assert!(!covers(guarded.as_ref(), n(&["http://ex/*"]).as_ref()));
    assert!(covers(
        guarded.as_ref(),
        n(&["http://ex/g1", "default"]).as_ref()
    ));
    assert!(covers(guarded.as_ref(), guarded.as_ref()));
    // protections: lifted on a narrower set, not lifted, lifted everywhere
    let p = protection("age", None);
    let view = Some(protected(
        &["default", "http://ex/a"],
        &p,
        rule(&["default"]),
        Caller::default(),
    ));
    let narrower = protected(
        &["default", "http://ex/a"],
        &p,
        Graphs::none(),
        Caller::default(),
    );
    let wider = protected(
        &["default", "http://ex/a"],
        &p,
        rule(&["default", "http://ex/a"]),
        Caller::default(),
    );
    assert!(!covers(Some(&narrower), view.as_ref()), "narrower lift");
    assert!(covers(Some(&wider), view.as_ref()), "wider lift");
    // a caller that lifts it everywhere sees more, and one under a protection the
    // principal does not have sees less
    assert!(covers(
        n(&["default", "http://ex/a"]).as_ref(),
        view.as_ref()
    ));
    assert!(!covers(Some(&wider), n(&["default"]).as_ref()));
    // a pattern that binds the caller needs the same caller
    let bound = protection("own", Some("?s <http://ex/owner> ?user"));
    let who = |u: &str| Caller {
        user: Some(u.into()),
        ..Default::default()
    };
    let as_alice = protected(&["default"], &bound, Graphs::none(), who("alice"));
    let as_bob = protected(&["default"], &bound, Graphs::none(), who("bob"));
    assert!(covers(Some(&as_alice), Some(&as_alice)));
    assert!(!covers(Some(&as_bob), Some(&as_alice)));
}

/// A grant that names a view declassifies it for that caller alone: a caller with the
/// same graphs and no such grant is refused.
#[test]
fn a_grant_that_names_the_view_declassifies_it() {
    let s = store();
    let policy = Policy::with(&[("reports", Some(reads(&["default"])))]);
    let rt = runtime(&s, &policy);
    define(&rt, &s, "team-sizes", "reports");
    define(&rt, &s, "other", "reports");
    let named = granted(&["http://ex/g1"], &["team-sizes"], "carol");
    let plain = granted(&["http://ex/g1"], &[], "dave");
    assert_eq!(read(&rt, &s, "team-sizes", &named).unwrap(), 2);
    assert!(rt.readable_by("team-sizes", Some(&named)));
    assert_eq!(
        code(&read(&rt, &s, "team-sizes", &plain).unwrap_err()),
        "view-access"
    );
    assert!(!rt.readable_by("team-sizes", Some(&plain)));
    assert_eq!(
        code(&read(&rt, &s, "other", &named).unwrap_err()),
        "view-access"
    );
    assert_eq!(
        rt.authorize_view("team-sizes", Some(&named), ViewOp::Watch)
            .unwrap(),
        ViewDecision::Allow(super::ViewAllow::Declassified)
    );
    // the grant reads nothing else of the store
    assert!(!named.readable(None));
}

/// Decisions are cached per caller and view: a second read hits, and another caller,
/// another view or changed grants miss.
#[test]
fn decisions_are_cached_per_caller_and_view() {
    let s = store();
    let policy = Policy::with(&[("reports", Some(reads(&["default"])))]);
    let rt = runtime(&s, &policy);
    define(&rt, &s, "team-sizes", "reports");
    define(&rt, &s, "other", "reports");
    let wider = reads(&["default", "http://ex/g1"]);
    let base = rt.view_auth_stats();
    read(&rt, &s, "team-sizes", &wider).unwrap();
    let first = rt.view_auth_stats();
    assert_eq!((first.misses - base.misses, first.hits - base.hits), (1, 0));
    // equal grants in another allocation hit
    read(&rt, &s, "team-sizes", &reads(&["default", "http://ex/g1"])).unwrap();
    let second = rt.view_auth_stats();
    assert_eq!(
        (second.misses - first.misses, second.hits - first.hits),
        (0, 1)
    );
    read(&rt, &s, "other", &wider).unwrap();
    let third = rt.view_auth_stats();
    assert_eq!(third.misses - second.misses, 1, "another view misses");
    // narrowed grants of the same caller miss and are refused at once
    let narrowed = reads(&["http://ex/g1"]);
    assert_eq!(
        code(&read(&rt, &s, "team-sizes", &narrowed).unwrap_err()),
        "view-access"
    );
    assert_eq!(rt.view_auth_stats().misses - third.misses, 1);
    // a guard decided against one snapshot or another keeps the same decision, so
    // the decision outlives a commit
    let decided = |exempt: &str| {
        Arc::new(GraphAccess::graphs(
            rule(&["default", "http://ex/g1"])
                .guarded("urn:x-test:guarded/*", &[exempt.to_string()]),
            Graphs::none(),
        ))
    };
    let at = rt.view_auth_stats();
    read(&rt, &s, "team-sizes", &decided("urn:x-test:guarded/1")).unwrap();
    read(&rt, &s, "team-sizes", &decided("urn:x-test:guarded/2")).unwrap();
    let now = rt.view_auth_stats();
    assert_eq!((now.misses - at.misses, now.hits - at.hits), (1, 1));
    // an unrestricted caller is neither cached nor counted
    let before = rt.view_auth_stats();
    read_at(&rt, &s, "team-sizes", None, None).unwrap();
    read(&rt, &s, "team-sizes", &Arc::new(GraphAccess::all())).unwrap();
    let after = rt.view_auth_stats();
    assert_eq!((after.hits, after.misses), (before.hits, before.misses));
}

/// A policy change that narrows the view's principal is decided again as soon as the
/// runtime resolves the principal under it, and a wider principal refuses a caller
/// that the narrower one let read.
#[test]
fn a_policy_change_is_decided_again() {
    let s = store();
    let policy = Policy::with(&[("reports", Some(reads(&["default"])))]);
    let rt = runtime(&s, &policy);
    define(&rt, &s, "team-sizes", "reports");
    let caller = reads(&["default", "http://ex/g1"]);
    assert_eq!(read(&rt, &s, "team-sizes", &caller).unwrap(), 2);
    assert!(rt.view_auth_stats().entries > 0);
    policy
        .principals
        .lock()
        .insert("reports".into(), Some(reads(&["default", SECRET])));
    policy.epoch.fetch_add(1, Ordering::SeqCst);
    rt.round(&s);
    assert_eq!(
        rt.view_auth_stats().entries,
        0,
        "the reload empties the cache"
    );
    let e = read(&rt, &s, "team-sizes", &caller).unwrap_err();
    assert_eq!(code(&e), "view-access");
    assert!(!e.to_string().contains(SECRET), "{e}");
}

/// Redefining a view with another principal decides its readers again.
#[test]
fn a_new_definition_is_decided_again() {
    let s = store();
    let policy = Policy::with(&[
        ("reports", Some(reads(&["default"]))),
        ("auditors", Some(reads(&["default", "http://ex/g2"]))),
    ]);
    let rt = runtime(&s, &policy);
    define(&rt, &s, "team-sizes", "reports");
    let caller = reads(&["default", "http://ex/g1"]);
    assert_eq!(read(&rt, &s, "team-sizes", &caller).unwrap(), 2);
    define(&rt, &s, "team-sizes", "auditors");
    assert_eq!(
        code(&read(&rt, &s, "team-sizes", &caller).unwrap_err()),
        "view-access"
    );
    assert!(!rt.readable_by("team-sizes", Some(&caller)));
    // and removing it forgets the decisions
    rt.delete("team-sizes").unwrap();
    assert_eq!(rt.view_auth_stats().entries, 0);
}

/// The audit log records every refusal, every declassified read and each decision the
/// cache did not hold, by category and caller, and never a graph name.
#[test]
fn decisions_are_audited_without_grants() {
    let s = store();
    let policy = Policy::with(&[("reports", Some(reads(&["default", SECRET])))]);
    let rt = runtime(&s, &policy);
    define(&rt, &s, "team-sizes", "reports");
    policy.events();
    let wider = granted(&["default", SECRET, "http://ex/g1"], &[], "erin");
    read(&rt, &s, "team-sizes", &wider).unwrap();
    read(&rt, &s, "team-sizes", &wider).unwrap();
    assert_eq!(
        policy.events(),
        ["team-sizes read allow contained cached=false erin"],
        "a cached allow is not recorded again"
    );
    let narrow = granted(&["default"], &[], "frank");
    read(&rt, &s, "team-sizes", &narrow).unwrap_err();
    read(&rt, &s, "team-sizes", &narrow).unwrap_err();
    assert_eq!(
        policy.events(),
        [
            "team-sizes read deny graphs cached=false frank",
            "team-sizes read deny graphs cached=true frank"
        ]
    );
    let named = granted(&["default"], &["team-sizes"], "grace");
    read(&rt, &s, "team-sizes", &named).unwrap();
    read(&rt, &s, "team-sizes", &named).unwrap();
    assert_eq!(
        policy.events(),
        [
            "team-sizes read allow declassified cached=false grace",
            "team-sizes read allow declassified cached=true grace"
        ]
    );
    // listings ask without writing records, and unrestricted reads are not recorded
    assert!(!rt.readable_by("team-sizes", Some(&narrow)));
    read_at(&rt, &s, "team-sizes", None, None).unwrap();
    assert!(policy.events().is_empty());
    rt.authorize_view("team-sizes", Some(&narrow), ViewOp::Watch)
        .unwrap();
    let events = policy.events();
    assert_eq!(events, ["team-sizes watch deny graphs cached=true frank"]);
    assert!(events.iter().all(|e| !e.contains(SECRET)));
}

/// A refused caller is refused before the read waits for a view that is behind, so
/// the refusal does not wait out `viewWaitMs` or the request's deadline.
#[test]
fn a_refusal_does_not_wait_for_the_view() {
    let s = store();
    let policy = Policy::with(&[("reports", Some(reads(&["default"])))]);
    let rt = Runtime::open(
        None,
        s.dataset_id(),
        RuntimeOptions {
            view_wait: Duration::from_secs(20),
            ..Default::default()
        },
    );
    rt.set_host(policy.clone());
    define(&rt, &s, "team-sizes", "reports");
    // a commit the view has not reached: no round runs
    crate::sparql::update::update(
        &s,
        &q("INSERT DATA { :d org:memberOf :t3 }"),
        &crate::sparql::QueryOptions::default(),
    )
    .unwrap();
    let start = Instant::now();
    let narrow = reads(&["http://ex/g1"]);
    let deadline = Instant::now() + Duration::from_secs(30);
    let e = read_at(&rt, &s, "team-sizes", Some(&narrow), Some(deadline)).unwrap_err();
    assert_eq!(code(&e), "view-access");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    // a reader that may read it still honors its deadline
    let wider = reads(&["default", "http://ex/g1"]);
    let start = Instant::now();
    let e = read_at(
        &rt,
        &s,
        "team-sizes",
        Some(&wider),
        Some(Instant::now() + Duration::from_millis(200)),
    )
    .unwrap_err();
    assert!(matches!(e, crate::Error::Timeout), "{e:?}");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
}

/// The cache holds at most its bound of decisions, however many callers ask.
#[test]
fn the_cache_is_bounded() {
    let s = store();
    let policy = Policy::with(&[("reports", Some(reads(&["default"])))]);
    let rt = runtime(&s, &policy);
    define(&rt, &s, "team-sizes", "reports");
    for i in 0..super::runtime::AUTH_CACHE_ENTRIES + 200 {
        let g = format!("http://ex/caller/{i}");
        let caller = reads(&["default", g.as_str()]);
        assert!(rt.readable_by("team-sizes", Some(&caller)));
        assert!(rt.view_auth_stats().entries <= super::runtime::AUTH_CACHE_ENTRIES);
    }
}

/// A view cannot read another view, so a principal's own grants that name views never
/// widen what a view reads.
#[test]
fn views_do_not_read_views() {
    let s = store();
    let policy = Policy::with(&[("reports", Some(reads(&["default"])))]);
    let rt = runtime(&s, &policy);
    define(&rt, &s, "team-sizes", "reports");
    let def = ViewDef::table(q(
        "SELECT ?team ?n WHERE { SERVICE <urn:x-sparkles:view#read> { [] <urn:x-sparkles:view#name> \"team-sizes\" ; <urn:x-sparkles:view#bind> ?team, ?n } }",
    ));
    let e = rt.put(&s, "nested", def).unwrap_err();
    assert!(e.to_string().contains("view:read"), "{e}");
}

/// Containment against the data: whenever the grant sets of a caller contain those of
/// a principal, every quad the principal reads, the caller reads too. The grant sets
/// combine graph sets with patterns, exclusions and guards, and protections by
/// predicate and by subject class, lifted in some graphs.
#[test]
fn containment_holds_against_the_data() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[crate::io::Source::from_bytes(
        br#"@prefix : <http://ex/> .
:d :p "default" . :d :age 1 . :d a :Secret .
<http://ex/a/1> { :a1 :p "a1" . :a1 :age 2 . :a1 a :Secret . }
<http://ex/a/2> { :a2 :p "a2" . :a2 :age 3 . }
<http://ex/b/1> { :b1 :p "b1" . :b1 :age 4 . :b1 a :Secret . }
_:g { :n :p "blank" . }
"#
        .to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .expect("load");
    let age = protection("age", None);
    let secret = Arc::new(Protection {
        name: "secret".into(),
        predicates: Some(vec!["http://ex/p".into()]),
        classes: Some(vec!["http://ex/Secret".into()]),
        subclasses: false,
        graphs: None,
        pattern: None,
        prefixes: Default::default(),
        hide_inferences: false,
    });
    let graph_sets: Vec<Graphs> = vec![
        Graphs::All,
        rule(&["default"]),
        rule(&["http://ex/a/1"]),
        rule(&["default", "http://ex/a/*"]),
        rule(&["default", "http://ex/*"]),
        rule(&["*"]),
        rule(&["default", "*"]),
        Graphs::All.without_pattern("http://ex/b/*"),
        Graphs::All.without_iri("http://ex/a/2"),
        Graphs::All.guarded("http://ex/a/*", &[]),
        rule(&["default", "http://ex/*"]).guarded("http://ex/b/*", &[]),
    ];
    let lifts: Vec<Graphs> = vec![
        Graphs::none(),
        rule(&["default"]),
        rule(&["http://ex/a/*"]),
        Graphs::All,
    ];
    let mut accesses: Vec<Arc<GraphAccess>> = Vec::new();
    for g in &graph_sets {
        accesses.push(Arc::new(GraphAccess::graphs(g.clone(), Graphs::none())));
        for p in [&age, &secret] {
            for l in &lifts {
                accesses.push(Arc::new(GraphAccess::with_triples(
                    g.clone(),
                    Graphs::none(),
                    TripleRules {
                        rules: vec![Rule {
                            protection: p.clone(),
                            read: l.clone(),
                            write: Graphs::none(),
                        }],
                        caller: Caller::default(),
                        limits: Limits::default(),
                    },
                )));
            }
        }
    }
    let query = "SELECT ?g ?s ?p ?o WHERE { { ?s ?p ?o } UNION { GRAPH ?g { ?s ?p ?o } } }";
    let rows: Vec<std::collections::BTreeSet<String>> = accesses
        .iter()
        .map(|a| {
            let r = crate::sparql::query(
                s.snapshot(),
                query,
                &crate::sparql::QueryOptions {
                    graphs: Some(a.clone()),
                    no_cache: true,
                    ..Default::default()
                },
            )
            .expect("query");
            r.test_application_rows()
                .into_iter()
                .map(|row| format!("{row:?}"))
                .collect()
        })
        .collect();
    let mut proven = 0;
    for (i, c) in accesses.iter().enumerate() {
        for (j, v) in accesses.iter().enumerate() {
            if covers(Some(c), Some(v)) {
                proven += 1;
                let missing: Vec<&String> = rows[j].difference(&rows[i]).collect();
                assert!(
                    missing.is_empty(),
                    "caller {i} misses {missing:?} of principal {j}"
                );
            }
        }
    }
    // the comparison proves more than equal grants
    assert!(
        proven > accesses.len() * 2,
        "{proven} of {}",
        accesses.len()
    );
}

fn waiting_runtime(s: &Store, policy: &Arc<Policy>) -> Arc<Runtime> {
    let rt = Runtime::open(
        None,
        s.dataset_id(),
        RuntimeOptions {
            view_wait: Duration::from_secs(20),
            ..Default::default()
        },
    );
    rt.set_host(policy.clone());
    rt
}

/// A refused caller is refused before the read reports the view's state or waits for
/// it: a suspended view does not tell it the size of its state, a view that failed to
/// attach does not tell it why, and a view that is not attached is refused at once,
/// each with a record of the refusal. A view that is not attached is decided as one
/// that reads every quad, so it refuses a caller that its principal would admit, too.
#[test]
fn a_refusal_comes_before_the_view_state() {
    let s = store();
    let policy = Policy::with(&[("reports", Some(reads(&["default", SECRET])))]);
    let rt = waiting_runtime(&s, &policy);
    let narrow = granted(&["default"], &[], "frank");
    let wider = granted(&["default", SECRET, "http://ex/g1"], &[], "erin");
    let deadline = || Some(Instant::now() + Duration::from_secs(30));
    let refused = |name: &str, caller: &Arc<GraphAccess>, hidden: &str| {
        let start = Instant::now();
        let e = read_at(&rt, &s, name, Some(caller), deadline()).unwrap_err();
        assert_eq!(code(&e), "view-access", "{name}: {e}");
        assert!(!e.to_string().contains(hidden), "{name}: {e}");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{name}: {:?}",
            start.elapsed()
        );
    };

    // over its own budget while it builds: still attached, so a contained caller is
    // told and a narrower one is refused without the reason
    // a query of its own, so that no other view shares its circuit and its budget
    let mut def = ViewDef::table(q("SELECT ?p ?team WHERE { ?p org:memberOf ?team }"));
    def.principal = Some("reports".into());
    def.memory_bytes = 1;
    rt.put(&s, "small", def).unwrap();
    rt.round(&s);
    let info = rt.get(&s, "small").unwrap();
    assert!(
        matches!(&info.state, crate::incr::ViewState::Suspended { reason } if reason.starts_with("over-budget: ")),
        "{:?}",
        info.state
    );
    policy.events();
    refused("small", &narrow, "over-budget");
    assert_eq!(
        policy.events(),
        ["small read deny graphs cached=false frank"]
    );
    let e = read_at(&rt, &s, "small", Some(&wider), deadline()).unwrap_err();
    assert_eq!(code(&e), "view-suspended", "{e}");

    // suspended by the views' shared budget: detached, so decided as a view that reads
    // every quad, which only an unrestricted reader reads
    let pooled = Runtime::open(
        None,
        s.dataset_id(),
        RuntimeOptions {
            view_wait: Duration::from_secs(20),
            views_memory: Some(1),
            ..Default::default()
        },
    );
    pooled.set_host(policy.clone());
    define(&pooled, &s, "pooled", "reports");
    let info = pooled.get(&s, "pooled").unwrap();
    assert!(
        matches!(&info.state, crate::incr::ViewState::Suspended { reason } if reason.contains("--views-memory")),
        "{:?}",
        info.state
    );
    policy.events();
    for caller in [&narrow, &wider] {
        let start = Instant::now();
        let e = read_at(&pooled, &s, "pooled", Some(caller), deadline()).unwrap_err();
        assert_eq!(code(&e), "view-access", "{e}");
        assert!(!e.to_string().contains("views-memory"), "{e}");
        assert!(start.elapsed() < Duration::from_secs(5));
    }
    let events = policy.events();
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(events.iter().all(|e| e.contains(" deny ")), "{events:?}");
    let e = read_at(&pooled, &s, "pooled", None, deadline()).unwrap_err();
    assert_eq!(code(&e), "view-suspended", "{e}");

    // rebuilt and not yet attached again: no round runs
    pooled.rebuild("pooled").unwrap();
    policy.events();
    for caller in [&narrow, &wider] {
        let start = Instant::now();
        let e = read_at(&pooled, &s, "pooled", Some(caller), deadline()).unwrap_err();
        assert_eq!(code(&e), "view-access", "{e}");
        assert!(start.elapsed() < Duration::from_secs(5));
    }
    assert_eq!(policy.events().len(), 2);

    // failed to attach: its principal is gone from the policy
    define(&rt, &s, "team-sizes", "reports");
    assert_eq!(read(&rt, &s, "team-sizes", &wider).unwrap(), 2);
    policy.principals.lock().remove("reports");
    policy.epoch.fetch_add(1, Ordering::SeqCst);
    rt.round(&s);
    assert!(
        matches!(
            rt.get(&s, "team-sizes").unwrap().state,
            crate::incr::ViewState::Failed { .. }
        ),
        "{:?}",
        rt.get(&s, "team-sizes").unwrap().state
    );
    policy.events();
    refused("team-sizes", &narrow, "no principal");
    refused("team-sizes", &wider, "no principal");
    let events = policy.events();
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(events.iter().all(|e| e.contains(" deny ")), "{events:?}");
    let e = read_at(&rt, &s, "team-sizes", None, deadline()).unwrap_err();
    assert_eq!(code(&e), "view-failed", "{e}");
}

/// A refused read asks for no memory: the eager reader resolves its fallback memory
/// only after the decision, and a refused `SERVICE view:read` fits in a budget that
/// does not hold the cursor's scratch.
#[test]
fn a_refusal_asks_for_no_memory() {
    let s = store();
    let policy = Policy::with(&[("reports", Some(reads(&["default", SECRET])))]);
    let rt = runtime(&s, &policy);
    define(&rt, &s, "team-sizes", "reports");
    let narrow = granted(&["default"], &[], "frank");
    let pool = crate::sparql::pool::QueryPool::new(1 << 20);
    let parent = || -> crate::Result<sparkles_budget::ParentHandle> {
        Err(crate::Error::invalid("the memory parent was asked for"))
    };
    let snap = s.snapshot();
    let read_with = |caller: &Arc<GraphAccess>| {
        rt.read(&ViewRead {
            name: "team-sizes",
            at: snap.commit,
            snap: Some(&snap),
            allow_stale: false,
            caller: Some(caller),
            deadline: None,
            aggregate_errors: None,
            memory_parent: Some(&parent),
            memory_pool: Some(&pool),
            max_memory_bytes: None,
        })
    };
    let e = read_with(&narrow).err().expect("refused");
    assert_eq!(code(&e), "view-access", "{e}");
    // an admitted reader does ask for it
    let wider = granted(&["default", SECRET, "http://ex/g1"], &[], "erin");
    let e = read_with(&wider).err().expect("no memory parent");
    assert!(e.to_string().contains("memory parent"), "{e}");

    // The query's budget: a refusal needs no more of it than the refusal of a view
    // that does not exist, which comes before the view is opened, so the cursor's
    // scratch is not charged for either.
    let outcome = |name: &str, budget: u64| {
        let text = format!(
            "PREFIX view: <urn:x-sparkles:view#> SELECT ?team ?n WHERE {{ SERVICE view:read {{ [] view:name \"{name}\" ; view:bind ?team, ?n }} }}"
        );
        let o = crate::sparql::QueryOptions {
            views: Some(rt.clone()),
            graphs: Some(narrow.clone()),
            max_memory_bytes: Some(budget),
            no_cache: true,
            ..Default::default()
        };
        match crate::sparql::query(s.snapshot(), &text, &o) {
            Ok(_) => "ok".to_string(),
            Err(e) => code(&e),
        }
    };
    let least = |name: &str, want: &str| {
        let (mut lo, mut hi) = (1u64, 64u64 << 20);
        assert_eq!(outcome(name, hi), want, "{name}");
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if outcome(name, mid) == want {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        lo
    };
    let missing = least("missing-xx", "view-not-found");
    assert_eq!(outcome("team-sizes", missing), "view-access");
    assert_eq!(least("team-sizes", "view-access"), missing);
}

/// Another host resolves the views' principals again, even with the same epoch.
#[test]
fn a_new_host_is_decided_again() {
    let s = store();
    let first = Policy::with(&[("reports", Some(reads(&["default"])))]);
    let rt = runtime(&s, &first);
    define(&rt, &s, "team-sizes", "reports");
    let caller = reads(&["default", "http://ex/g1"]);
    assert_eq!(read(&rt, &s, "team-sizes", &caller).unwrap(), 2);
    let second = Policy::with(&[("reports", Some(reads(&["default", SECRET])))]);
    assert_eq!(first.epoch(), second.epoch());
    rt.set_host(second.clone());
    rt.round(&s);
    let e = read(&rt, &s, "team-sizes", &caller).unwrap_err();
    assert_eq!(code(&e), "view-access");
    assert!(!rt.readable_by("team-sizes", Some(&caller)));
    assert_eq!(read_at(&rt, &s, "team-sizes", None, None).unwrap(), 2);
}

/// A decision made for an entry read before the cache was emptied is not kept after
/// it, so a definition that replaces the view with equal grants is decided, and
/// recorded, afresh.
#[test]
fn a_decision_in_flight_is_not_kept_past_a_change() {
    let s = Arc::new(store());
    let policy = Policy::with(&[("reports", Some(reads(&["default"])))]);
    let rt = runtime(&s, &policy);
    define(&rt, &s, "team-sizes", "reports");
    let wider = granted(&["default", "http://ex/g1"], &[], "erin");
    let once = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (rt2, s2, once2) = (Arc::downgrade(&rt), s.clone(), once.clone());
    rt.set_auth_miss_hook(Some(Arc::new(move || {
        if !once2.swap(true, Ordering::SeqCst)
            && let Some(rt) = rt2.upgrade()
        {
            let mut def = ViewDef::table(q(TEAM_SIZES));
            def.principal = Some("reports".into());
            rt.put(&s2, "team-sizes", def).unwrap();
        }
    })));
    policy.events();
    assert!(rt.readable_by("team-sizes", Some(&wider)));
    assert!(once.load(Ordering::SeqCst), "the miss ran");
    rt.set_auth_miss_hook(None);
    assert_eq!(
        rt.view_auth_stats().entries,
        0,
        "nothing kept for the old entry"
    );
    rt.round(&s);
    assert_eq!(read(&rt, &s, "team-sizes", &wider).unwrap(), 2);
    assert_eq!(
        policy.events(),
        ["team-sizes read allow contained cached=false erin"],
        "the first read of the new definition is recorded"
    );
}

/// The metadata of a view and the decision on it come from one registry entry, so a
/// replacement between the two never pairs the old state with the new decision.
#[test]
fn metadata_and_its_decision_come_from_one_entry() {
    let s = Arc::new(store());
    let policy = Policy::with(&[
        ("secret", Some(reads(&["default", SECRET]))),
        ("reports", Some(reads(&["default"]))),
    ]);
    let rt = runtime(&s, &policy);
    define(&rt, &s, "team-sizes", "secret");
    let caller = reads(&["default", "http://ex/g1"]);
    let replace = |rt: &Arc<Runtime>| {
        let (weak, s2) = (Arc::downgrade(rt), s.clone());
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done2 = done.clone();
        rt.set_after_info_hook(Some(Arc::new(move || {
            if !done2.swap(true, Ordering::SeqCst)
                && let Some(rt) = weak.upgrade()
            {
                define(&rt, &s2, "team-sizes", "reports");
            }
        })));
        done
    };
    let done = replace(&rt);
    let (info, readable) = rt.get_readable(&s, "team-sizes", Some(&caller)).unwrap();
    assert!(done.load(Ordering::SeqCst));
    assert_eq!(info.definition.principal.as_deref(), Some("secret"));
    assert!(!readable, "decided on the entry whose state it reports");
    assert!(rt.readable_by("team-sizes", Some(&caller)), "the new entry");
    define(&rt, &s, "team-sizes", "secret");
    let done = replace(&rt);
    let listed = rt.list_readable(&s, Some(&caller));
    assert!(done.load(Ordering::SeqCst));
    let (info, readable) = listed.iter().find(|(v, _)| v.name == "team-sizes").unwrap();
    assert_eq!(info.definition.principal.as_deref(), Some("secret"));
    assert!(!readable);
    rt.set_after_info_hook(None);
}

/// The unnamed coverage test considers containment alone, whatever names a grant
/// holds, the empty one included.
#[test]
fn coverage_without_a_name_ignores_named_grants() {
    let view = reads(&["default", SECRET]);
    let empty = Arc::new(GraphAccess {
        views: ViewGrants::new([String::new()], Some("mallory")),
        ..GraphAccess::graphs(rule(&["default"]), Graphs::none())
    });
    assert!(empty.views.grants(""));
    assert!(!covers(Some(&empty), Some(&view)));
    assert_eq!(
        super::runtime::decide("", Some(&empty), Some(&view)),
        ViewDecision::Allow(super::ViewAllow::Declassified),
        "a decision for a view named so honors the grant"
    );
}

/// Closing a caller's guards closes those of its protections' lifts too.
#[test]
fn guards_close_inside_the_lifts() {
    let s = store();
    let p = protection("age", None);
    let lift = rule(&["default"]).guarded("http://ex/lift/*", &["http://ex/lift/1".to_string()]);
    let caller = protected(&["default"], &p, lift, Caller::default());
    let bound = caller.bound(&s.snapshot());
    let closed = bound.guards_closed();
    let rules = closed.triples.as_ref().unwrap();
    let Graphs::Only(r) = &rules.rules[0].read else {
        panic!("a lift by graph")
    };
    assert!(
        r.guards
            .iter()
            .all(|g| g.exempt.is_empty() && g.decide.is_none())
    );
    assert_eq!(closed.guards_closed(), closed);
}
