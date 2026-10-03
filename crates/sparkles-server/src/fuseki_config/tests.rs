//! The converter against Apache Jena's own Fuseki configurations, copied into
//! `testsuite/fuseki-config/jena` under the Apache License 2.0 (spec G08 §8).

use super::convert::{Mode, Plan};
use super::report::Kind;
use super::{UserSource, plan_for, write};
use std::path::PathBuf;

/// The directories of `testsuite/fuseki-config/jena` with configurations.
const SUBDIRS: [&str; 6] = [
    "examples",
    "examples/rdfs",
    "testing/Access",
    "testing/FusekiBuild",
    "testing/GeoAssembler",
    "testing/Shiro",
];

fn jena() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testsuite/fuseki-config/jena")
}

fn plan(rel: &str) -> Plan {
    plan_with(rel, UserSource::default())
}

fn plan_with(rel: &str, users: UserSource) -> Plan {
    plan_for(&[jena().join(rel)], &users, Mode::Files).unwrap_or_else(|e| panic!("{rel}: {e:#}"))
}

fn has(p: &Plan, kind: Kind, needle: &str) -> bool {
    p.report
        .items
        .iter()
        .any(|i| i.kind == kind && (i.message.contains(needle) || i.place.contains(needle)))
}

fn args(p: &Plan) -> String {
    write::serve_args(p).join(" ")
}

#[test]
fn a1_memory_dataset() {
    let p = plan("examples/config-1-mem.ttl");
    assert!(!p.report.has_unsupported(), "{}", p.report.text());
    assert_eq!(p.datasets.len(), 1);
    assert!(args(&p).contains("--mem dataset"), "{}", args(&p));
    assert!(p.auth.is_none());
    // query, update, the Graph Store and patch at their usual names
    assert!(
        has(&p, Kind::Converted, "patch at /dataset/patch"),
        "{}",
        p.report.text()
    );
}

#[test]
fn a2_tdb2_with_union_default_graph() {
    let p = plan("examples/config-tdb2.ttl");
    assert!(!p.report.has_unsupported(), "{}", p.report.text());
    let a = args(&p);
    assert!(a.contains("--loc dataset=db/dataset"), "{a}");
    assert!(a.contains("--union-default-graph"), "{a}");
    assert!(has(&p, Kind::Manual, "tdb2.tdbdump"), "{}", p.report.text());
    let ds = &p.datasets[0];
    assert!(ds.tdb.as_ref().unwrap().ends_with("DB2"));
    // TDB1 too
    let p = plan("examples/config-tdb1.ttl");
    assert!(
        p.datasets[0].tdb1 || p.report.has_unsupported(),
        "{}",
        p.report.text()
    );
}

#[test]
fn a3_text_index() {
    let p = plan("examples/config-text-tdb2.ttl");
    assert!(!p.report.has_unsupported(), "{}", p.report.text());
    let text = p.datasets[0].text.as_ref().unwrap();
    assert_eq!(
        text["predicates"],
        serde_json::json!(["http://www.w3.org/2000/01/rdf-schema#label"])
    );
    #[cfg(feature = "text")]
    {
        let c: sparkles::text::TextConfig = serde_json::from_value(text.clone()).unwrap();
        assert!(
            c.predicates
                .contains("http://www.w3.org/2000/01/rdf-schema#label")
        );
    }
    assert!(args(&p).contains("--text dataset=datasets/dataset/text.json"));
    assert!(has(&p, Kind::Ignored, "text:directory"));
}

#[test]
fn a4_inference() {
    let p = plan("examples/config-inference-1.ttl");
    let ds = &p.datasets[0];
    let r = ds.reasoning.as_ref().unwrap();
    assert_eq!(r.profile, Some("owl-rl"));
    assert!(ds.data[0].path.ends_with("Data/data.ttl"), "{:?}", ds.data);
    assert!(ds.persistent(Mode::Files));
    assert!(has(&p, Kind::Approximated, "OWLFBRuleReasoner"));
    // the only service is read-only
    assert!(p.server.read_only && p.auth.is_none());

    // a schema bound to the reasoner, over a TDB2 graph
    let p = plan("examples/config-inference-2.ttl");
    let ds = &p.datasets[0];
    let r = ds.reasoning.as_ref().unwrap();
    assert!(r.schema[0].ends_with("myOntology.ttl"));
    assert!(ds.tdb.as_ref().unwrap().ends_with("DB"));
    assert!(!p.report.has_unsupported(), "{}", p.report.text());
    assert!(args(&p).contains("--auto-reason 5"));
}

#[test]
fn a5_timeouts() {
    let p = plan("examples/config-timeout-server.ttl");
    assert_eq!(p.server.timeout, Some(10.0));
    assert!(args(&p).contains("--timeout 10"));
    let p = plan("examples/config-timeout-endpoint.ttl");
    assert_eq!(p.server.timeout, Some(60.0));
    assert!(
        has(&p, Kind::Approximated, "first result"),
        "{}",
        p.report.text()
    );
    assert!(has(&p, Kind::Approximated, "apply to every dataset"));
    let p = plan("examples/config-timeout-dataset.ttl");
    assert_eq!(p.server.timeout, Some(60.0));
}

#[test]
fn a6_rdfs_on_read() {
    let p = plan("examples/rdfs/config-rdfs.ttl");
    let ds = &p.datasets[0];
    assert!(ds.rdfs.as_ref().unwrap().ends_with("vocabulary.ttl"));
    assert!(ds.data[0].path.ends_with("data.trig"));
    assert!(args(&p).contains("--rdfs dataset=datasets/dataset/rdfs-vocabulary.ttl"));
}

#[test]
fn a7_selected_graphs_are_unsupported() {
    for f in [
        "examples/tdb2-select-graphs.ttl",
        "examples/tdb2-select-graphs-alt.ttl",
        "examples/custom-union-graph.ttl",
    ] {
        let p = plan(f);
        assert!(p.report.has_unsupported(), "{f}: {}", p.report.text());
    }
    let p = plan("examples/tdb2-select-graphs.ttl");
    assert!(
        has(&p, Kind::Unsupported, "https://example/ng"),
        "{}",
        p.report.text()
    );
}

#[test]
fn endpoint_names() {
    // an update endpoint named "sparql" is not served there
    let p = plan("examples/config-4-endpoint-sparql.ttl");
    assert!(
        has(&p, Kind::Unsupported, "update at /dataset/sparql"),
        "{}",
        p.report.text()
    );
    // legacy properties, and prefixes at "updatePrefixes"
    let p = plan("examples/config-2-mem-old.ttl");
    assert!(!p.report.has_unsupported(), "{}", p.report.text());
    let p = plan("examples/config-prefixes.ttl");
    assert!(
        has(&p, Kind::Unsupported, "updatePrefixes"),
        "{}",
        p.report.text()
    );
    // the SHACL endpoint
    let p = plan("examples/config-shacl.ttl");
    assert!(!p.report.has_unsupported(), "{}", p.report.text());
}

#[test]
fn a8_allowed_users_with_a_password_file() {
    let p = plan_with(
        "testing/Access/config-server-1.ttl",
        UserSource {
            passwd: Some(jena().join("testing/Access/passwd")),
            shiro: None,
        },
    );
    let auth = p.auth.as_ref().expect("an auth plan");
    let reads = |u: &str, ds: &str| {
        auth.users
            .iter()
            .find(|x| x.name == u)
            .is_some_and(|x| x.grants.datasets.get(ds) == Some(&"read"))
    };
    assert!(reads("user1", "database1") && reads("user3", "database1"));
    assert!(!reads("user2", "database1"));
    // the server's "*" admits every user to the other dataset
    assert!(reads("user2", "database2") && reads("user9", "database2"));
    assert!(auth.anonymous.is_empty());
    let mut report = p.report.clone();
    let toml = write::auth_toml(auth, None, &mut report).unwrap();
    assert!(!toml.contains("pw1"), "{toml}");
    #[cfg(feature = "auth")]
    {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("auth.toml");
        write::write_private(&f, toml.as_bytes()).unwrap();
        let (cfg, _) = crate::auth::config::FileConfig::load(&f).unwrap();
        assert_eq!(cfg.users.len(), 4);
    }
}

#[test]
fn endpoint_users_and_graph_access() {
    let users = || UserSource {
        passwd: Some(jena().join("testing/Access/passwd")),
        shiro: None,
    };
    // query for user1 and user2, update for user1 and user3, at the dataset URL
    let p = plan_with("testing/Access/config-auth.ttl", users());
    let auth = p.auth.as_ref().unwrap();
    let u = |n: &str| auth.users.iter().find(|x| x.name == n).unwrap();
    assert_eq!(u("user1").grants.datasets.get("db"), Some(&"write"));
    // queries reach every graph, so a query grant needs no endpoint limit
    assert_eq!(u("user2").grants.datasets.get("db"), Some(&"read"));
    let g3 = &u("user3").grants.grants[0];
    assert_eq!(
        (g3.level, g3.endpoints.as_deref()),
        ("write", Some(&["update".to_string()][..]))
    );
    assert!(u("user9").grants.is_empty());

    // graph access control: the shared dataset is unsupported, the graphs are grants
    let p = plan_with("testing/Access/assem-security-shared.ttl", users());
    assert!(
        has(&p, Kind::Unsupported, "shares its dataset"),
        "{}",
        p.report.text()
    );
    let p = plan_with("testing/Access/assem-security.ttl", users());
    // one dataset has a union default graph, and the other does not
    assert!(
        has(&p, Kind::Unsupported, "UnionGraph"),
        "{}",
        p.report.text()
    );
    let auth = p.auth.as_ref().unwrap();
    let u = |n: &str| auth.users.iter().find(|x| x.name == n).unwrap();
    let g = u("user1")
        .grants
        .grants
        .iter()
        .find(|g| g.dataset == "database")
        .unwrap();
    assert_eq!(g.level, "read");
    assert_eq!(
        g.graphs.as_deref().unwrap(),
        [
            "http://host/graphname1",
            "http://host/graphname2",
            "http://host/graphname3"
        ]
    );
    assert!(
        !u("user9")
            .grants
            .grants
            .iter()
            .any(|g| g.dataset == "database")
    );
}

#[test]
fn a9_shiro_users_roles_and_admin() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(
        jena().join("examples/config-1-mem.ttl"),
        dir.path().join("config.ttl"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("shiro.ini"),
        "[main]\nlocalhostFilter = org.apache.jena.fuseki.authz.LocalhostFilter\n\
         [users]\nadmin = s3cret-pw, administrator\nreader = r3ad-pw\n\
         [urls]\n/$/ping = anon\n/$/** = authcBasic, roles[administrator]\n\
         /dataset/** = authcBasic\n/** = anon\n",
    )
    .unwrap();
    let p = plan_for(
        &[dir.path().to_path_buf()],
        &UserSource::default(),
        Mode::Files,
    )
    .unwrap();
    let auth = p.auth.as_ref().unwrap();
    assert_eq!(
        auth.roles.get("administrator").map(|r| r.server.clone()),
        Some(vec!["server-admin"])
    );
    let admin = auth.users.iter().find(|u| u.name == "admin").unwrap();
    assert_eq!(admin.roles, vec!["administrator"]);
    assert_eq!(admin.grants.datasets.get("dataset"), Some(&"write"));
    assert!(auth.anonymous.is_empty());
    let out = dir.path().join("out");
    let mut p = p;
    write::write_dir(&mut p, &out, false).unwrap();
    let toml = std::fs::read_to_string(out.join("auth.toml")).unwrap();
    assert!(!toml.contains("s3cret") && !p.report.text().contains("s3cret"));
    for f in std::fs::read_dir(&out).unwrap() {
        let f = f.unwrap().path();
        if f.is_file() {
            assert!(
                !std::fs::read_to_string(&f).unwrap().contains("s3cret"),
                "{f:?}"
            );
        }
    }
    #[cfg(feature = "auth")]
    {
        assert!(toml.contains("$argon2id$"), "{toml}");
        let (cfg, _) = crate::auth::config::FileConfig::load(&out.join("auth.toml")).unwrap();
        assert_eq!(cfg.users.len(), 2);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(out.join("auth.toml"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn server_settings() {
    let p = plan("testing/FusekiBuild/config-context-path.ttl");
    assert!(
        has(&p, Kind::Unsupported, "contextPath"),
        "{}",
        p.report.text()
    );
}

/// An open server where one service is read-only: the anonymous caller's grants follow
/// each service's endpoints.
#[test]
fn some_read_only_services() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("config.ttl");
    std::fs::write(
        &f,
        "PREFIX fuseki: <http://jena.apache.org/fuseki#>\n\
         PREFIX ja: <http://jena.hpl.hp.com/2005/11/Assembler#>\n\
         [] a fuseki:Service ; fuseki:name \"ro\" ;\n\
           fuseki:endpoint [ fuseki:operation fuseki:gsp-r ; fuseki:name \"get\" ] ;\n\
           fuseki:dataset [ a ja:MemoryDataset ] .\n\
         [] a fuseki:Service ; fuseki:name \"rw\" ;\n\
           fuseki:endpoint [ fuseki:operation fuseki:query ] ;\n\
           fuseki:endpoint [ fuseki:operation fuseki:update ] ;\n\
           fuseki:dataset [ a ja:MemoryDataset ] .\n",
    )
    .unwrap();
    let p = plan_for(&[f], &UserSource::default(), Mode::Files).unwrap();
    assert!(!p.server.read_only);
    let auth = p.auth.as_ref().expect("an auth plan");
    assert_eq!(auth.anonymous.datasets.get("rw"), Some(&"write"));
    assert_eq!(
        auth.anonymous.grants[0].endpoints.as_deref(),
        Some(&["gsp-r".to_string()][..])
    );
    assert!(args(&p).contains("--auth-config auth.toml"));
}

#[test]
fn geosparql_datasets() {
    let p = plan("testing/GeoAssembler/geo-config-ex.ttl");
    let ds = &p.datasets[0];
    let geo = ds.geo.as_ref().unwrap();
    assert_eq!(geo["queryRewrite"], serde_json::json!(true));
    #[cfg(feature = "geo")]
    {
        let c: sparkles::geo::config::GeoConfig = serde_json::from_value(geo.clone()).unwrap();
        c.validate().unwrap();
    }
    assert!(ds.reasoning.as_ref().unwrap().geo_vocab);
    assert!(has(&p, Kind::Ignored, "spatialIndexFile"));
    assert!(args(&p).contains("--geo ds2=datasets/ds2/geo.json"));
}

/// A10: where the assembler bodies of `POST /$/datasets` accept a description, the
/// converter makes a dataset of the same name and type.
#[test]
fn a10_agrees_with_assembler_bodies() {
    use crate::http::fuseki::assembler;
    use crate::state::DbType;
    let mut checked = 0;
    let mut files: Vec<PathBuf> = Vec::new();
    for sub in SUBDIRS {
        for e in std::fs::read_dir(jena().join(sub)).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "ttl") {
                files.push(p);
            }
        }
    }
    files.sort();
    for f in files {
        let body = std::fs::read(&f).unwrap();
        let accepts = |union: bool| {
            let server = assembler::Server {
                union_default_graph: union,
                gsp_direct_naming: false,
            };
            assembler::parse(&body, oxrdfio::RdfFormat::Turtle, &server).ok()
        };
        let (plain, union) = (accepts(false), accepts(true));
        let Some(spec) = plain.as_ref().or(union.as_ref()) else {
            continue;
        };
        let p = plan_for(
            std::slice::from_ref(&f),
            &UserSource::default(),
            Mode::Serve,
        )
        .unwrap();
        let ds = p
            .datasets
            .iter()
            .find(|d| d.name == spec.name)
            .unwrap_or_else(|| panic!("{}: no dataset {}", f.display(), spec.name));
        assert_eq!(
            ds.persistent(Mode::Serve),
            spec.kind == DbType::Persistent,
            "{}",
            f.display()
        );
        // a description that needs one union setting gets it
        match (plain.is_some(), union.is_some()) {
            (true, false) => assert!(!p.server.union_default_graph, "{}", f.display()),
            (false, true) => assert!(p.server.union_default_graph, "{}", f.display()),
            _ => {}
        }
        checked += 1;
    }
    eprintln!("compared {checked} configurations");
    assert!(checked >= 5, "only {checked} configurations compared");
}

/// Every file of the copied examples converts without an error, and `--check` finds
/// the same report as a written conversion.
#[test]
fn every_configuration_converts() {
    for sub in SUBDIRS {
        for e in std::fs::read_dir(jena().join(sub)).unwrap() {
            let f = e.unwrap().path();
            if f.extension().is_none_or(|x| x != "ttl") {
                continue;
            }
            let p = plan_for(
                std::slice::from_ref(&f),
                &UserSource::default(),
                Mode::Files,
            );
            // configuration files that hold no service (vocabularies) are errors
            if let Ok(mut p) = p {
                let dir = tempfile::tempdir().unwrap();
                write::write_dir(&mut p, &dir.path().join("out"), false)
                    .unwrap_or_else(|e| panic!("{}: {e:#}", f.display()));
                assert!(dir.path().join("out/serve.sh").is_file());
            }
        }
    }
}
