//! Footprints, coverage and the guard (C22 §6, A15, A16).

use super::*;
use sparkles::access::{Limits, TripleRules};

const A: &str = "urn:g:a";
const B: &str = "urn:g:b";
const REPORTS: &str = "urn:x-sparkles:graphrag:reports";

fn graphs(names: &[&str]) -> Graphs {
    Graphs::Only(GraphRule::new(names.iter().copied(), &[]))
}

use sparkles::access::GraphRule;

fn protection(name: &str, pattern: Option<&str>) -> Arc<Protection> {
    Arc::new(Protection {
        name: name.into(),
        predicates: Some(vec!["http://ex/salary".into()]),
        classes: None,
        subclasses: true,
        graphs: None,
        pattern: pattern.map(str::to_string),
        prefixes: BTreeMap::new(),
        hide_inferences: false,
    })
}

fn caller(user: &str, groups: &[&str]) -> Caller {
    Caller {
        user: Some(user.into()),
        roles: vec!["staff".into()],
        groups: groups.iter().map(|g| g.to_string()).collect(),
    }
}

/// A view reading `read`, with `protections` in force everywhere (lifted nowhere).
fn view(read: &[&str], protections: &[Arc<Protection>], c: Caller) -> GraphAccess {
    let rules = TripleRules {
        rules: protections
            .iter()
            .map(|p| Rule {
                protection: p.clone(),
                read: Graphs::none(),
                write: Graphs::none(),
            })
            .collect(),
        caller: c,
        limits: Limits::default(),
    };
    if protections.is_empty() {
        GraphAccess::graphs(graphs(read), Graphs::none())
    } else {
        GraphAccess::with_triples(graphs(read), Graphs::none(), rules)
    }
}

fn footprint(v: Option<&GraphAccess>, read: &[&str], c: &Caller) -> Footprint {
    let graphs: Vec<String> = read.iter().map(|g| g.to_string()).collect();
    capture(&CaptureInput {
        dataset: "ds",
        branch: "main",
        source_commit: 7,
        principal: "reports",
        endpoint: "query",
        view: v,
        caller: c,
        graphs: &graphs,
        reports: &[],
    })
    .unwrap()
}

#[test]
fn unrestricted_readers_are_proven() {
    let c = caller("reports", &[]);
    let p = view(&[A, B], &[protection("pay", None)], c.clone());
    let fp = footprint(Some(&p), &[A, B], &c);
    assert_eq!(covers(None, &fp), Decision::Proven);
    let all = GraphAccess::graphs(Graphs::All, Graphs::All);
    assert_eq!(covers(Some(&all), &fp), Decision::Proven);
}

#[test]
fn an_identical_view_is_proven() {
    let c = caller("reports", &["g1"]);
    let p = view(
        &[A, B],
        &[protection("pay", Some("?s <http://ex/owner> ?user"))],
        c.clone(),
    );
    let fp = footprint(Some(&p), &[A, B], &c);
    assert_eq!(covers(Some(&p), &fp), Decision::Proven);
}

/// The principal later loses B; a reader of A only is still denied (A15).
#[test]
fn coverage_follows_the_footprint_not_current_grants() {
    let c = caller("reports", &[]);
    let at_build = view(&[A, B], &[], c.clone());
    let fp = footprint(Some(&at_build), &[A, B], &c);
    let reader = view(&[A], &[], caller("someone", &[]));
    assert_eq!(covers(Some(&reader), &fp), Decision::Denied);
    let wider = view(&[A, B, "urn:g:c"], &[], caller("someone", &[]));
    assert_eq!(covers(Some(&wider), &fp), Decision::Proven);
}

#[test]
fn structural_inclusion_admits_fewer_protections() {
    let c = caller("reports", &[]);
    let pay = protection("pay", None);
    let p = view(&[A], std::slice::from_ref(&pay), c.clone());
    let fp = footprint(Some(&p), &[A], &c);
    // the reader lifts the protection the principal had in force
    let reader = view(&[A], &[], caller("hr", &[]));
    assert_eq!(covers(Some(&reader), &fp), Decision::Proven);
    // a protection in force for the reader that the principal lifted is unproven
    let fp_open = footprint(Some(&view(&[A], &[], c.clone())), &[A], &c);
    let restricted = view(&[A], &[pay], caller("staff", &[]));
    assert_eq!(covers(Some(&restricted), &fp_open), Decision::Unproven);
}

#[test]
fn a_changed_definition_with_the_same_name_is_unproven() {
    let c = caller("reports", &[]);
    let fp = footprint(
        Some(&view(&[A], &[protection("pay", None)], c.clone())),
        &[A],
        &c,
    );
    let mut other = (*protection("pay", None)).clone();
    other.predicates = Some(vec!["http://ex/bonus".into()]);
    let reader = view(&[A], &[Arc::new(other)], c.clone());
    assert_eq!(covers(Some(&reader), &fp), Decision::Unproven);
}

/// Same grants and roles, different `?group` values (A16).
#[test]
fn caller_variables_a_pattern_uses_must_match() {
    let c = caller("reports", &["finance"]);
    let docs = protection("docs", Some("?s <http://ex/group> ?group"));
    let fp = footprint(
        Some(&view(&[A], std::slice::from_ref(&docs), c.clone())),
        &[A],
        &c,
    );
    let same = view(
        &[A],
        std::slice::from_ref(&docs),
        caller("ana", &["finance"]),
    );
    assert_eq!(
        covers(Some(&same), &fp),
        Decision::Proven,
        "?user is not used"
    );
    let other = view(&[A], std::slice::from_ref(&docs), caller("bob", &["sales"]));
    assert_eq!(covers(Some(&other), &fp), Decision::Unproven);
    // $-variables count as well
    let by_user = protection("mine", Some("?s <http://ex/owner> $user"));
    let fp = footprint(
        Some(&view(&[A], std::slice::from_ref(&by_user), c.clone())),
        &[A],
        &c,
    );
    let other = view(&[A], &[by_user], caller("bob", &["finance"]));
    assert_eq!(covers(Some(&other), &fp), Decision::Unproven);
}

#[test]
fn variable_detection_needs_a_whole_name() {
    assert!(uses_var("?s ex:o ?user .", "user"));
    assert!(uses_var("FILTER(?user)", "user"));
    assert!(!uses_var("?username", "user"));
    assert!(!uses_var("?s ex:user ?o", "user"));
    assert!(uses_var("$group", "group"));
}

#[test]
fn capture_refuses_what_it_cannot_describe() {
    let c = caller("reports", &[]);
    let v = view(&[A], &[], c.clone());
    let graphs = vec![B.to_string()];
    let e = capture(&CaptureInput {
        dataset: "ds",
        branch: "main",
        source_commit: 1,
        principal: "reports",
        endpoint: "query",
        view: Some(&v),
        caller: &c,
        graphs: &graphs,
        reports: &[],
    })
    .unwrap_err();
    assert_eq!(e, CaptureError::Unreadable(B.into()));
    let graphs = vec![REPORTS.to_string()];
    let e = capture(&CaptureInput {
        dataset: "ds",
        branch: "main",
        source_commit: 1,
        principal: "reports",
        endpoint: "query",
        view: None,
        caller: &c,
        graphs: &graphs,
        reports: &[],
    })
    .unwrap_err();
    assert!(matches!(e, CaptureError::Unrepresentable(_)));
}

fn guard_for(dir: Option<&Path>) -> Guard {
    let id = uuid::Uuid::from_u128(1);
    let root = dir.map(Path::to_path_buf);
    Guard::new(Box::new(move |name| {
        (name == "ds").then(|| (id, root.clone()))
    }))
}

fn entry(fp: Footprint) -> Entry {
    Entry {
        graph: REPORTS.into(),
        footprint: fp,
    }
}

#[test]
fn restricted_views_lose_unproven_guarded_graphs() {
    let reader = GraphAccess::graphs(Graphs::All.without_iri("urn:g:z"), Graphs::none());
    // without a guard, or with no footprint, the graph is hidden
    let v = apply(reader.clone(), "ds", None);
    assert!(!v.read.allows_iri(REPORTS));
    assert!(v.read.allows_iri(A));
    let g = guard_for(None);
    assert!(
        !apply(reader.clone(), "ds", Some(&g))
            .read
            .allows_iri(REPORTS)
    );
    // a footprint the reader covers exempts it
    let c = caller("reports", &[]);
    g.install(
        "ds",
        vec![entry(footprint(
            Some(&view(&[A], &[], c.clone())),
            &[A],
            &c,
        ))],
    )
    .unwrap();
    assert!(
        apply(reader.clone(), "ds@main", Some(&g))
            .read
            .allows_iri(REPORTS)
    );
    // other guarded graphs stay hidden
    assert!(
        !apply(reader.clone(), "ds", Some(&g))
            .read
            .allows_iri("urn:x-sparkles:graphrag:other")
    );
    // a second footprint the reader does not cover hides it again
    g.install(
        "ds",
        vec![entry(footprint(
            Some(&view(&["urn:g:z"], &[], c.clone())),
            &["urn:g:z"],
            &c,
        ))],
    )
    .unwrap();
    assert!(
        !apply(reader.clone(), "ds", Some(&g))
            .read
            .allows_iri(REPORTS)
    );
    // an unrestricted view is left as it is
    let all = GraphAccess::graphs(Graphs::All, Graphs::All);
    assert_eq!(apply(all.clone(), "ds", None), all);
}

#[test]
fn registries_are_durable_add_only_and_versioned() {
    let dir = tempfile::tempdir().unwrap();
    let g = guard_for(Some(dir.path()));
    let c = caller("reports", &[]);
    let fp = footprint(Some(&view(&[A], &[], c.clone())), &[A], &c);
    g.install("ds", vec![entry(fp.clone())]).unwrap();
    g.install("ds", vec![entry(fp.clone())]).unwrap();
    let r = Registry::load(dir.path()).unwrap();
    assert_eq!(r.entries.len(), 1, "an entry is kept once");
    let reader = GraphAccess::graphs(graphs(&[A, REPORTS]), Graphs::none());
    assert!(
        apply(reader.clone(), "ds", Some(&g))
            .read
            .allows_iri(REPORTS)
    );
    // a fresh guard reads the file
    let g2 = guard_for(Some(dir.path()));
    assert!(
        apply(reader.clone(), "ds", Some(&g2))
            .read
            .allows_iri(REPORTS)
    );
    // an unknown version makes every guarded graph unproven, and refuses installs
    std::fs::write(path_in(dir.path()), br#"{"version": 2, "entries": []}"#).unwrap();
    assert!(matches!(
        Registry::load(dir.path()),
        Err(RegistryError::UnknownVersion(2))
    ));
    assert!(
        !apply(reader.clone(), "ds", Some(&g))
            .read
            .allows_iri(REPORTS)
    );
    assert!(g.install("ds", vec![entry(fp.clone())]).is_err());
    // a damaged file as well
    std::fs::write(path_in(dir.path()), b"{not json").unwrap();
    assert!(!apply(reader, "ds", Some(&g2)).read.allows_iri(REPORTS));
}

#[test]
fn entries_outside_the_namespace_are_corrupt() {
    let r = br#"{"version": 1, "entries": [{"graph": "urn:other", "footprint": {"version": 1, "dataset": "d", "branch": "main", "sourceCommit": 1, "principal": "p", "caller": {}, "endpoint": "query", "graphs": []}}]}"#;
    assert!(matches!(Registry::parse(r), Err(RegistryError::Corrupt(_))));
}

#[test]
fn installs_bump_the_epoch() {
    let g = guard_for(None);
    let before = g.epoch();
    let c = caller("reports", &[]);
    g.install("ds", vec![entry(footprint(None, &[A], &c))])
        .unwrap();
    assert!(g.epoch() > before);
}
