//! The resource catalog: the model's checks, references, licence propagation, the
//! NOTICE, the DCAT description, the file format and the routes.

use super::*;
use crate::state::{AppState, DbType};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value as J, json};
use sparkles::io::Source;
use sparkles::store::StoreOptions;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const VOCAB: &str = r#"
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix dct: <http://purl.org/dc/terms/> .
@prefix vann: <http://purl.org/vocab/vann/> .
<http://ex/vocab/g> {
  <http://ex/v#> a owl:Ontology ;
    owl:versionInfo "2.0" ;
    dct:license <https://spdx.org/licenses/CC-BY-4.0.html> ;
    dct:title "Example vocabulary" ;
    vann:preferredNamespacePrefix "exv" ;
    vann:preferredNamespaceUri "http://ex/v#" .
}
"#;

fn input(kind: Kind, title: &str) -> ResourceInput {
    ResourceInput {
        kind,
        title: title.into(),
        description: None,
        homepage: None,
        ownership: Ownership::Owned,
        license: None,
        license_class: None,
        license_url: None,
        attribution: None,
        namespaces: Vec::new(),
        graphs: Vec::new(),
        dataset: None,
    }
}

fn version(v: &str) -> VersionInput {
    VersionInput {
        version: v.into(),
        ..Default::default()
    }
}

fn pin(r: &str, v: &str) -> Pin {
    Pin {
        resource: r.into(),
        version: v.into(),
    }
}

#[test]
fn ids_versions_and_members_are_checked() {
    assert!(valid_id("osm") && valid_id("a.b_c-1") && !valid_id("-x") && !valid_id(""));
    assert!(!valid_id(&"x".repeat(65)) && !valid_id("a b"));
    assert!(valid_version("2024-01+1") && !valid_version("1 0") && !valid_version(""));
    let mut i = input(Kind::Source, "S");
    assert!(check_input(&i).is_ok());
    i.ownership = Ownership::Mirrored;
    assert!(message(&check_input(&i).unwrap_err()).contains("item 81"));
    i.ownership = Ownership::Synced;
    assert!(message(&check_input(&i).unwrap_err()).contains("item 339"));
    i.ownership = Ownership::Owned;
    i.namespaces.push(Namespace {
        prefix: "x".into(),
        iri: "http://ex/x#".into(),
    });
    assert!(message(&check_input(&i).unwrap_err()).contains("vocabulary"));
    let mut v = input(Kind::Vocabulary, "V");
    v.namespaces = i.namespaces.clone();
    assert!(check_input(&v).is_ok());
    v.namespaces.push(Namespace {
        prefix: "x".into(),
        iri: "http://ex/y#".into(),
    });
    assert!(check_input(&v).is_err());
    let mut l = input(Kind::Source, "L");
    l.license = Some("LicenseRef-x".into());
    assert!(message(&check_input(&l).unwrap_err()).contains("licenseClass"));
    l.license_class = Some(license::Class::Restricted);
    assert!(check_input(&l).is_ok());
    l.license = None;
    assert!(check_input(&l).is_err(), "a class without a licence");
    let mut t = input(Kind::Source, " ");
    assert!(check_input(&t).is_err());
    t.title = "a\u{1}".into();
    assert!(check_input(&t).is_err());
    let mut d = version("1");
    d.distributions.push(Distribution {
        url: "https://ex/d.nt".into(),
        media_type: Some("application/n-triples".into()),
        byte_size: Some(10),
        sha256: Some("AB".repeat(32)),
    });
    assert!(check_version(Kind::Source, &d).is_ok());
    d.distributions[0].sha256 = Some("xyz".into());
    assert!(check_version(Kind::Source, &d).is_err());
    d.distributions[0].sha256 = None;
    d.distributions[0].media_type = Some("nope".into());
    assert!(check_version(Kind::Source, &d).is_err());
    let mut g = version("1");
    g.graphs.push(GraphRecord {
        graph: "http://ex/g".into(),
        sources: Vec::new(),
    });
    assert!(check_version(Kind::Source, &g).is_err());
    assert!(check_version(Kind::Dataset, &g).is_ok());
    g.issued = Some("2024-13-40".into());
    assert!(check_version(Kind::Dataset, &g).is_err());
    g.issued = Some("2024-02-01".into());
    assert!(check_version(Kind::Dataset, &g).is_ok());
}

/// A catalog in memory with a source, a vocabulary importing a second vocabulary, and a
/// dataset whose graphs come from the source and the vocabulary.
fn sample() -> Resources {
    let r = Resources::in_dir(Path::new(""));
    let mut osm = input(Kind::Source, "OpenStreetMap");
    osm.license = Some("ODbL-1.0".into());
    osm.attribution = Some("© OpenStreetMap contributors".into());
    r.create("osm", osm, vec![version("2024"), version("2025")], "admin")
        .unwrap();
    let mut base = input(Kind::Vocabulary, "Base");
    base.license = Some("CC0-1.0".into());
    base.namespaces.push(Namespace {
        prefix: "base".into(),
        iri: "http://ex/base#".into(),
    });
    r.create("base", base, vec![version("1")], "admin").unwrap();
    let mut v = input(Kind::Vocabulary, "Example vocabulary");
    v.license = Some("CC-BY-4.0".into());
    v.namespaces.push(Namespace {
        prefix: "exv".into(),
        iri: "http://ex/v#".into(),
    });
    v.graphs.push(Location {
        dataset: "ds".into(),
        graph: "http://ex/vocab/g".into(),
    });
    let mut v2 = version("2.0");
    v2.imports.push(pin("base", "1"));
    r.create("exv", v, vec![v2], "admin").unwrap();
    let mut imdb = input(Kind::Source, "IMDb");
    imdb.license = Some("LicenseRef-imdb".into());
    imdb.license_class = Some(license::Class::Restricted);
    r.create("imdb", imdb, vec![version("1")], "admin").unwrap();
    r
}

#[test]
fn references_pin_existing_versions_and_block_removal() {
    let r = sample();
    let mut bad = version("3");
    bad.imports.push(pin("base", "9"));
    assert_eq!(
        r.add_version("exv", bad, "a").unwrap_err(),
        no_such_version("base", "9")
    );
    assert!(matches!(
        r.add_version("exv", version("2.0"), "a"),
        Err(Error::Conflict("version-exists", _))
    ));
    assert!(matches!(
        r.delete("base", None),
        Err(Error::Conflict("resource-in-use", m)) if m.contains("exv 2.0")
    ));
    assert!(matches!(
        r.delete_version("base", "1", "a"),
        Err(Error::Conflict("resource-in-use", _))
    ));
    // a resource's own versions do not keep it
    let mut own = version("2026");
    own.imports.push(pin("osm", "2024"));
    r.add_version("osm", own, "a").unwrap();
    assert!(matches!(
        r.delete_version("osm", "2024", "a"),
        Err(Error::Conflict("resource-in-use", _))
    ));
    r.delete("osm", None).unwrap();
    // a namespace belongs to one vocabulary
    let mut dup = input(Kind::Vocabulary, "Dup");
    dup.namespaces.push(Namespace {
        prefix: "other".into(),
        iri: "http://ex/v#".into(),
    });
    assert!(matches!(
        r.create("dup", dup, Vec::new(), "a"),
        Err(Error::Conflict("namespace-owned", m)) if m.contains("exv")
    ));
    // the kind stays
    assert!(
        r.replace("exv", input(Kind::Source, "X"), None, "a")
            .is_err()
    );
}

#[test]
fn obligations_propagate_and_the_gate_warns_unless_enforced() {
    let r = sample();
    let mut ds = input(Kind::Dataset, "Built");
    ds.license = Some("CC-BY-4.0".into());
    ds.dataset = Some("ds".into());
    let mut v = version("1");
    v.graphs.push(GraphRecord {
        graph: "http://ex/osm".into(),
        sources: vec![pin("osm", "2025")],
    });
    v.graphs.push(GraphRecord {
        graph: "http://ex/vocab".into(),
        sources: vec![pin("exv", "2.0")],
    });
    r.create("built", ds, vec![v], "a").unwrap();
    let c = r.read().unwrap();
    let rep = report::license_report(&c, "built", None, license::Policy::Warn).unwrap();
    assert_eq!(rep.class, license::Class::ShareAlike);
    assert!(rep.redistributable);
    assert_eq!(rep.verdict, license::Verdict::Allow);
    let g: Vec<(&str, license::Class)> = rep
        .graphs
        .iter()
        .map(|g| (g.graph.as_str(), g.class))
        .collect();
    assert_eq!(
        g,
        [
            ("http://ex/osm", license::Class::ShareAlike),
            ("http://ex/vocab", license::Class::Attribution)
        ]
    );
    // the vocabulary graph carries the import's licence too
    assert!(rep.graphs[1].obligations.contains(&"base 1".to_string()));
    let n = report::notice(&c, &rep);
    assert!(n.contains("© OpenStreetMap contributors"), "{n}");
    assert!(n.contains("https://spdx.org/licenses/ODbL-1.0.html"), "{n}");
    assert!(n.contains("Graphs: http://ex/osm"), "{n}");

    // a restricted source makes its graph and the version not redistributable
    let mut v2 = version("2");
    v2.graphs.push(GraphRecord {
        graph: "http://ex/movies".into(),
        sources: vec![pin("imdb", "1")],
    });
    r.add_version("built", v2, "a").unwrap();
    let c = r.read().unwrap();
    let warn = report::license_report(&c, "built", None, license::Policy::Warn).unwrap();
    assert_eq!(warn.class, license::Class::Restricted);
    assert_eq!(warn.verdict, license::Verdict::Warn);
    assert_eq!(warn.unaccepted, ["imdb"]);
    let enforce = report::license_report(&c, "built", None, license::Policy::Enforce).unwrap();
    assert_eq!(enforce.verdict, license::Verdict::Refuse);
    let old = report::license_report(&c, "built", Some("1"), license::Policy::Enforce).unwrap();
    assert_eq!(old.verdict, license::Verdict::Allow);

    // acceptance is reported, and a licence change withdraws it
    r.accept("imdb", "root").unwrap();
    let c = r.read().unwrap();
    let rep = report::license_report(&c, "built", None, license::Policy::Warn).unwrap();
    assert!(rep.unaccepted.is_empty());
    assert!(matches!(
        r.accept("osm", "root"),
        Err(Error::Conflict("not-restricted", _))
    ));
    let mut imdb = find(&c, "imdb").unwrap().input.clone();
    imdb.license = Some("LicenseRef-imdb-2".into());
    let after = r.replace("imdb", imdb, None, "root").unwrap();
    assert!(after.acceptance.is_none());
    // a new version under other terms leaves an old acceptance stale
    r.accept("imdb", "root").unwrap();
    let mut nc = version("2");
    nc.license = Some("CC-BY-NC-4.0".into());
    let r2 = r.add_version("imdb", nc, "root").unwrap();
    assert!(r2.acceptance.is_some() && !r2.accepted());

    // a missing licence is unknown
    r.create(
        "mine",
        input(Kind::Dataset, "Mine"),
        vec![version("1")],
        "a",
    )
    .unwrap();
    let c = r.read().unwrap();
    let rep = report::license_report(&c, "mine", None, license::Policy::Warn).unwrap();
    assert_eq!(rep.class, license::Class::Unknown);
    assert!(!rep.redistributable && rep.warnings[0].contains("no licence"));
}

#[test]
fn the_catalog_is_described_in_dcat() {
    let r = sample();
    let mut v = version("2.1");
    v.license = Some("CC-BY-4.0 OR MIT".into());
    v.issued = Some("2025-01-02".into());
    v.distributions.push(Distribution {
        url: "https://ex/v.ttl".into(),
        media_type: Some("text/turtle".into()),
        byte_size: Some(1234),
        sha256: Some("0f".repeat(32)),
    });
    v.imports.push(pin("base", "1"));
    r.add_version("exv", v, "a").unwrap();
    let c = r.read().unwrap();
    let nt = rdf::describe(&c, Some("exv"), oxrdfio::RdfFormat::NTriples);
    for want in [
        "<urn:x-sparkles:resources> <http://www.w3.org/ns/dcat#dataset> <urn:x-sparkles:resource:exv>",
        "<urn:x-sparkles:resource:exv> <http://purl.org/dc/terms/license> <http://spdx.org/licenses/CC-BY-4.0>",
        "<urn:x-sparkles:resource:exv> <http://www.w3.org/ns/dcat#hasCurrentVersion> <urn:x-sparkles:resource:exv/2.1>",
        "<urn:x-sparkles:resource:exv/2.1> <http://www.w3.org/ns/dcat#previousVersion> <urn:x-sparkles:resource:exv/2.0>",
        "<urn:x-sparkles:resource:exv/2.1> <http://www.w3.org/ns/prov#wasDerivedFrom> <urn:x-sparkles:resource:base/1>",
        "<http://www.w3.org/ns/dcat#mediaType> <https://www.iana.org/assignments/media-types/text/turtle>",
        "\"1234\"^^<http://www.w3.org/2001/XMLSchema#nonNegativeInteger>",
        "<http://spdx.org/rdf/terms#algorithm> <http://spdx.org/rdf/terms#checksumAlgorithm_sha256>",
        "<http://purl.org/vocab/vann/preferredNamespacePrefix> \"exv\"",
        "<http://rdfs.org/ns/void#inDataset> <urn:x-sparkles:dataset:ds>",
        "\"CC-BY-4.0 OR MIT\"",
        "\"2025-01-02\"^^<http://www.w3.org/2001/XMLSchema#date>",
    ] {
        assert!(nt.contains(want), "{want} missing from\n{nt}");
    }
    assert!(!nt.contains("resource:osm"));
    // every syntax the description offers parses back
    let ttl = rdf::describe(&c, None, oxrdfio::RdfFormat::Turtle);
    let n = oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::Turtle)
        .for_slice(ttl.as_bytes())
        .count();
    assert!(n > 40, "{n}");
}

#[test]
fn a_file_of_an_unknown_format_is_refused_and_kept() {
    let dir = tempfile::tempdir().unwrap();
    let r = Resources::in_dir(dir.path());
    assert!(r.read().unwrap().resources.is_empty());
    r.create("s", input(Kind::Source, "S"), Vec::new(), "a")
        .unwrap();
    let f = dir.path().join(FILE);
    let v: J = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
    assert_eq!(v["format"], 1);
    let newer = br#"{"format": 2, "resources": []}"#;
    std::fs::write(&f, newer).unwrap();
    let e = r.read().unwrap_err();
    assert!(
        matches!(&e, Error::Unreadable(m) if m.contains("newer release")),
        "{e:?}"
    );
    assert!(matches!(
        r.create("t", input(Kind::Source, "T"), Vec::new(), "a"),
        Err(Error::Unreadable(_))
    ));
    assert_eq!(std::fs::read(&f).unwrap(), newer);
    std::fs::write(&f, b"{not json").unwrap();
    assert!(matches!(r.read(), Err(Error::Unreadable(_))));
    // a file that breaks the catalog's rules is unreadable too
    std::fs::write(
        &f,
        br#"{"format":1,"resources":[{"id":"-bad","kind":"source","title":"x","created":{"by":"a","at":"t"},"modified":{"by":"a","at":"t"}}]}"#,
    )
    .unwrap();
    assert!(matches!(r.read(), Err(Error::Unreadable(_))));
}

// ------------------------------------------------------------------- routes ------

struct Server {
    _dir: tempfile::TempDir,
    st: Arc<AppState>,
    app: Router,
}

fn server(read_only: bool, policy: license::Policy) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.read_only = read_only;
    st.license_policy = policy;
    let st = Arc::new(st);
    let ds = st.attach("ds", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            VOCAB.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let app = crate::http::router(st.clone());
    Server { _dir: dir, st, app }
}

struct Resp {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> J {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn code(&self) -> String {
        self.json()["code"].as_str().unwrap_or_default().to_string()
    }
}

async fn send(app: &Router, req: Request<Body>) -> Resp {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Resp {
        status,
        headers,
        body,
    }
}

fn req(method: &str, uri: &str, body: Option<J>) -> Request<Body> {
    let b = Request::builder().method(method).uri(uri);
    match body {
        Some(j) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(j.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

fn vocabulary() -> J {
    json!({
        "id": "exv",
        "kind": "vocabulary",
        "title": "Example vocabulary",
        "license": "CC-BY-4.0",
        "namespaces": [{"prefix": "exv", "iri": "http://ex/v#"}],
        "graphs": [{"dataset": "ds", "graph": "http://ex/vocab/g"}],
        "versions": [{"version": "2.0", "issued": "2024-05-01"}],
    })
}

#[tokio::test]
async fn resources_are_created_read_replaced_and_removed() {
    let s = server(false, license::Policy::Warn);
    let r = send(&s.app, req("POST", "/$/resources", Some(vocabulary()))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.headers[header::LOCATION], "/$/resources/exv");
    let tag = r.headers[header::ETAG].to_str().unwrap().to_string();
    assert_eq!(r.json()["versions"][0]["added"]["by"], "local");
    let again = send(&s.app, req("POST", "/$/resources", Some(vocabulary()))).await;
    assert_eq!(
        (again.status, again.code().as_str()),
        (StatusCode::CONFLICT, "resource-exists")
    );
    let list = send(&s.app, req("GET", "/$/resources", None)).await.json();
    assert_eq!(list["resources"][0]["id"], "exv");
    let one = send(&s.app, req("GET", "/$/resources/exv", None)).await;
    assert_eq!(one.headers[header::ETAG].to_str().unwrap(), tag);
    let ttl = send(
        &s.app,
        Request::get("/$/resources")
            .header(header::ACCEPT, "text/turtle")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(
        ttl.headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/turtle")
    );
    assert!(ttl.text().contains("dcat:Catalog"), "{}", ttl.text());
    let nt = send(&s.app, req("GET", "/$/resources/exv?format=ntriples", None)).await;
    assert!(nt.text().contains("<urn:x-sparkles:resource:exv>"));

    // members the catalog sets, unknown members and versions in a PUT are refused
    for (body, needle) in [
        (
            json!({"kind": "source", "title": "x", "created": {}}),
            "set by the catalog",
        ),
        (
            json!({"kind": "source", "title": "x", "colour": 1}),
            "not a member",
        ),
        (
            json!({"kind": "vocabulary", "title": "x", "versions": []}),
            "versions change",
        ),
    ] {
        let r = send(&s.app, req("PUT", "/$/resources/exv", Some(body))).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
        assert!(r.text().contains(needle), "{}", r.text());
    }
    let mut body = vocabulary();
    let o = body.as_object_mut().unwrap();
    o.remove("versions");
    o.insert("title".into(), json!("Renamed"));
    let stale = send(
        &s.app,
        Request::put("/$/resources/exv")
            .header(header::IF_MATCH, "\"nope\"")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);
    let ok = send(
        &s.app,
        Request::put("/$/resources/exv")
            .header(header::IF_MATCH, &tag)
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text());
    assert_eq!(ok.json()["title"], "Renamed");
    assert_eq!(ok.json()["versions"].as_array().unwrap().len(), 1);

    // versions are added and removed, never changed
    let v = send(
        &s.app,
        req(
            "POST",
            "/$/resources/exv/versions",
            Some(json!({"version": "2.1"})),
        ),
    )
    .await;
    assert_eq!(v.status, StatusCode::CREATED, "{}", v.text());
    let dup = send(
        &s.app,
        req(
            "POST",
            "/$/resources/exv/versions",
            Some(json!({"version": "2.1"})),
        ),
    )
    .await;
    assert_eq!(dup.code(), "version-exists");
    let gone = send(&s.app, req("DELETE", "/$/resources/exv/versions/9", None)).await;
    assert_eq!(
        (gone.status, gone.code().as_str()),
        (StatusCode::NOT_FOUND, "no-such-version")
    );
    assert_eq!(
        send(&s.app, req("DELETE", "/$/resources/exv/versions/2.1", None))
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    // a dataset resource pins the vocabulary, which then cannot go
    let ds = json!({
        "id": "built", "kind": "dataset", "title": "Built", "dataset": "ds",
        "versions": [{"version": "1", "graphs": [{"graph": "http://ex/vocab/g",
            "sources": [{"resource": "exv", "version": "2.0"}]}]}],
    });
    assert_eq!(
        send(&s.app, req("POST", "/$/resources", Some(ds)))
            .await
            .status,
        StatusCode::CREATED
    );
    let used = send(&s.app, req("DELETE", "/$/resources/exv", None)).await;
    assert_eq!(
        (used.status, used.code().as_str()),
        (StatusCode::CONFLICT, "resource-in-use")
    );
    assert_eq!(
        send(&s.app, req("DELETE", "/$/resources/built", None))
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(&s.app, req("DELETE", "/$/resources/exv", None))
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    let missing = send(&s.app, req("GET", "/$/resources/exv", None)).await;
    assert_eq!(missing.code(), "no-such-resource");
    // a mirrored resource names the work it waits for
    let m = send(
        &s.app,
        req(
            "POST",
            "/$/resources",
            Some(json!({"id": "m", "kind": "source", "title": "M", "ownership": "mirrored"})),
        ),
    )
    .await;
    assert_eq!(m.status, StatusCode::BAD_REQUEST);
    assert!(m.text().contains("item 81"));
    assert!(s.st.data_dir.join(FILE).exists());
}

#[tokio::test]
async fn licence_reports_follow_the_policy() {
    for (policy, verdict) in [
        (license::Policy::Warn, "warn"),
        (license::Policy::Enforce, "refuse"),
    ] {
        let s = server(false, policy);
        let src = json!({"id": "imdb", "kind": "source", "title": "IMDb",
            "license": "LicenseRef-imdb", "licenseClass": "restricted",
            "versions": [{"version": "1"}]});
        assert_eq!(
            send(&s.app, req("POST", "/$/resources", Some(src)))
                .await
                .status,
            StatusCode::CREATED
        );
        let rep = send(&s.app, req("GET", "/$/resources/imdb/license", None))
            .await
            .json();
        assert_eq!(rep["verdict"], verdict);
        assert_eq!(
            rep["policy"],
            if verdict == "warn" { "warn" } else { "enforce" }
        );
        assert_eq!(rep["class"], "restricted");
        assert_eq!(rep["unaccepted"], json!(["imdb"]));
        let acc = send(&s.app, req("POST", "/$/resources/imdb/acceptance", None)).await;
        assert_eq!(acc.status, StatusCode::OK, "{}", acc.text());
        assert!(acc.json()["acceptance"]["at"].is_string());
        let rep = send(
            &s.app,
            req("GET", "/$/resources/imdb/license?version=1", None),
        )
        .await
        .json();
        assert!(rep.get("unaccepted").is_none(), "{rep}");
        let w = send(&s.app, req("DELETE", "/$/resources/imdb/acceptance", None))
            .await
            .json();
        assert!(w.get("acceptance").is_none());
        let n = send(&s.app, req("GET", "/$/resources/imdb/notice", None)).await;
        assert!(
            n.headers[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/plain")
        );
        assert!(
            n.text().contains("LicenseRef-imdb (restricted)"),
            "{}",
            n.text()
        );
        assert!(n.text().contains("may not be redistributed"));
        let none = send(
            &s.app,
            req("GET", "/$/resources/imdb/license?version=7", None),
        )
        .await;
        assert_eq!(none.code(), "no-such-version");
    }
}

#[tokio::test]
async fn a_read_only_server_refuses_catalog_writes() {
    let s = server(true, license::Policy::Warn);
    for (m, u) in [
        ("POST", "/$/resources"),
        ("PUT", "/$/resources/x"),
        ("DELETE", "/$/resources/x"),
        ("POST", "/$/resources/x/versions"),
        ("DELETE", "/$/resources/x/versions/1"),
        ("POST", "/$/resources/x/acceptance"),
        ("DELETE", "/$/resources/x/acceptance"),
        ("POST", "/$/resources/x/install/ds"),
    ] {
        let r = send(&s.app, req(m, u, Some(json!({})))).await;
        assert_eq!(
            (r.status, r.code().as_str()),
            (StatusCode::FORBIDDEN, "server-read-only"),
            "{m} {u}"
        );
    }
    assert_eq!(
        send(&s.app, req("GET", "/$/resources", None)).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn an_unreadable_catalog_answers_503_on_every_route() {
    let s = server(false, license::Policy::Warn);
    let f = s.st.data_dir.join(FILE);
    let newer = br#"{"format": 9, "resources": []}"#;
    std::fs::write(&f, newer).unwrap();
    for (m, u, b) in [
        ("GET", "/$/resources", None),
        ("POST", "/$/resources", Some(vocabulary())),
        ("GET", "/$/resources/x", None),
        (
            "PUT",
            "/$/resources/x",
            Some(json!({"kind": "source", "title": "x"})),
        ),
        ("DELETE", "/$/resources/x", None),
        (
            "POST",
            "/$/resources/x/versions",
            Some(json!({"version": "1"})),
        ),
        ("DELETE", "/$/resources/x/versions/1", None),
        ("POST", "/$/resources/x/acceptance", None),
        ("DELETE", "/$/resources/x/acceptance", None),
        ("GET", "/$/resources/x/license", None),
        ("GET", "/$/resources/x/notice", None),
        ("GET", "/$/resources/x/header", None),
        ("POST", "/$/resources/x/install/ds", None),
    ] {
        let r = send(&s.app, req(m, u, b)).await;
        assert_eq!(
            (r.status, r.code().as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, "catalog-unreadable"),
            "{m} {u}: {}",
            r.text()
        );
    }
    assert_eq!(std::fs::read(&f).unwrap(), newer);
    // the datasets keep serving
    assert_eq!(
        send(&s.app, req("GET", "/ds/sparql?query=ASK%7B%7D", None))
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn the_header_is_read_and_compared() {
    let s = server(false, license::Policy::Warn);
    let mut v = vocabulary();
    v["graphs"]
        .as_array_mut()
        .unwrap()
        .push(json!({"dataset": "nope", "graph": "http://ex/vocab/g"}));
    v["graphs"]
        .as_array_mut()
        .unwrap()
        .push(json!({"dataset": "ds", "graph": "http://ex/empty"}));
    assert_eq!(
        send(&s.app, req("POST", "/$/resources", Some(v)))
            .await
            .status,
        StatusCode::CREATED
    );
    let h = send(&s.app, req("GET", "/$/resources/exv/header", None))
        .await
        .json();
    let g = &h["graphs"];
    assert_eq!(g[0]["found"], true, "{h}");
    assert_eq!(g[0]["versionInfo"], "2.0");
    assert_eq!(g[0]["prefix"], "exv");
    assert_eq!(g[0]["disagreements"], json!([]), "{h}");
    assert!(g[1]["error"].as_str().unwrap().contains("nope"));
    assert_eq!(g[2]["found"], false);
    // a new version and another licence disagree with the header
    send(
        &s.app,
        req(
            "POST",
            "/$/resources/exv/versions",
            Some(json!({"version": "3.0", "license": "CC0-1.0"})),
        ),
    )
    .await;
    let h = send(&s.app, req("GET", "/$/resources/exv/header", None))
        .await
        .json();
    let d = h["graphs"][0]["disagreements"].as_array().unwrap();
    assert_eq!(d.len(), 2, "{h}");
    // only a vocabulary has a header
    send(
        &s.app,
        req(
            "POST",
            "/$/resources",
            Some(json!({"id": "s", "kind": "source", "title": "S"})),
        ),
    )
    .await;
    let r = send(&s.app, req("GET", "/$/resources/s/header", None)).await;
    assert_eq!(r.code(), "not-a-vocabulary");
}

#[tokio::test]
async fn a_vocabulary_installs_its_namespaces_and_its_imports() {
    let s = server(false, license::Policy::Warn);
    let base = json!({"id": "base", "kind": "vocabulary", "title": "Base",
        "namespaces": [{"prefix": "base", "iri": "http://ex/base#"},
                       {"prefix": "rdfs", "iri": "http://www.w3.org/2000/01/rdf-schema#"}],
        "versions": [{"version": "1"}]});
    send(&s.app, req("POST", "/$/resources", Some(base))).await;
    let mut v = vocabulary();
    v["versions"][0]["imports"] = json!([{"resource": "base", "version": "1"}]);
    let r = send(&s.app, req("POST", "/$/resources", Some(v))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let out = send(&s.app, req("POST", "/$/resources/exv/install/ds", None)).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.text());
    let j = out.json();
    let installed: Vec<&str> = j["installed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["prefix"].as_str().unwrap())
        .collect();
    assert_eq!(installed.len(), 2, "{j}");
    assert!(installed.contains(&"exv") && installed.contains(&"base"));
    assert_eq!(j["unchanged"][0]["prefix"], "rdfs");
    let p = send(&s.app, req("GET", "/$/prefixes/ds", None))
        .await
        .json();
    assert_eq!(p["prefixes"]["exv"], "http://ex/v#", "{p}");
    // installing again changes nothing
    let again = send(&s.app, req("POST", "/$/resources/exv/install/ds", None))
        .await
        .json();
    assert_eq!(again["installed"], json!([]));
    // a prefix bound elsewhere conflicts unless replaced
    let set = send(
        &s.app,
        Request::post("/ds/prefixes")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("prefix=exv&uri=http%3A%2F%2Fother%2F"))
            .unwrap(),
    )
    .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.text());
    let c = send(&s.app, req("POST", "/$/resources/exv/install/ds", None)).await;
    assert_eq!(
        (c.status, c.code().as_str()),
        (StatusCode::CONFLICT, "prefix-conflict"),
        "{}",
        c.text()
    );
    assert_eq!(c.json()["conflicts"][0]["bound"], "http://other/");
    let ok = send(
        &s.app,
        req("POST", "/$/resources/exv/install/ds?replace=true", None),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text());
    assert_eq!(ok.json()["replaced"][0]["prefix"], "exv");
    let p = send(&s.app, req("GET", "/$/prefixes/ds", None))
        .await
        .json();
    assert_eq!(p["prefixes"]["exv"], "http://ex/v#");
    let src = send(
        &s.app,
        req(
            "POST",
            "/$/resources",
            Some(json!({"id": "s", "kind": "source", "title": "S"})),
        ),
    )
    .await;
    assert_eq!(src.status, StatusCode::CREATED);
    let r = send(&s.app, req("POST", "/$/resources/s/install/ds", None)).await;
    assert_eq!(r.code(), "not-a-vocabulary");
    let r = send(&s.app, req("POST", "/$/resources/exv/install/nope", None)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}
