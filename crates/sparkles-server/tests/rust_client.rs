//! The Rust client (`crates/sparkles-client`) against a real `sparkles serve`: the
//! acceptance examples of spec P02 through the async API, and through the blocking facade
//! against a server with authentication.

use oxrdf::{Literal, NamedNode, Quad, Term, Triple};
use sparkles_client::{
    At, Client, CommitsOptions, DatasetType, Error, Graph, QueryOptions, RdfBody, ReadOptions,
    UpdateOptions, UploadOptions, UploadPart, WriteOptions,
};
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

struct Server {
    child: Child,
    url: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A server on a free port of `ports` with its data in `dir`. Each test has its own
/// ports, so two tests never start servers on the same one.
fn serve(dir: &Path, ports: std::ops::Range<u16>, extra: &[&str]) -> Server {
    let port = ports
        .clone()
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .unwrap_or_else(|| panic!("no free port in {ports:?}"));
    let child = Command::new(BIN)
        .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string()])
        .args(["--idle-release-ms", "0"])
        .args(extra)
        .arg("--data")
        .arg(dir.join("data"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut child = child;
    let t0 = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "the server exited before listening"
        );
        assert!(
            t0.elapsed() < Duration::from_secs(120),
            "server did not start"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    Server {
        child,
        url: format!("http://127.0.0.1:{port}"),
    }
}

fn iri(s: &str) -> NamedNode {
    NamedNode::new(s).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_client_against_a_server() {
    let dir = tempfile::tempdir().unwrap();
    let server = serve(dir.path(), 5540..5545, &[]);
    let client = Client::new(&server.url).unwrap();
    client.ping().await.unwrap();
    let info = client.server().await.unwrap();
    assert!(!info.read_only);

    // a persistent dataset keeps its history for `at`
    let ds = client
        .create_dataset("lib", DatasetType::Persistent)
        .await
        .unwrap();
    assert!(
        client
            .datasets()
            .await
            .unwrap()
            .iter()
            .any(|d| d.name.as_deref() == Some("lib"))
    );

    // A1: an update's receipt names its commit
    let r = ds
        .update(r#"INSERT DATA { <http://e/a> <http://e/name> "Ann"@en ; <http://e/age> 42 }"#)
        .await
        .unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.commit_seq, Some(1));
    assert_eq!(r.committed, Some(true));
    let c = r.commit.unwrap();
    assert_eq!((c.seq, c.inserted, c.kind.as_str()), (1, 2, "update"));
    assert_eq!(r.body["inserted"], 2);
    assert!(r.dataset_id.is_some());

    // A2: solutions are oxrdf terms
    let mut sols = ds
        .select("SELECT ?p ?o { <http://e/a> ?p ?o } ORDER BY ?p")
        .await
        .unwrap();
    assert_eq!(
        sols.variables()
            .iter()
            .map(|v| v.as_str())
            .collect::<Vec<_>>(),
        ["p", "o"]
    );
    assert_eq!(sols.meta().commit, Some(1));
    let first = sols.next().await.unwrap().unwrap();
    assert_eq!(first.get("o"), Some(&Term::from(Literal::from(42))));
    let second = sols.next().await.unwrap().unwrap();
    assert_eq!(
        second.get("o"),
        Some(&Term::from(
            Literal::new_language_tagged_literal("Ann", "en").unwrap()
        ))
    );
    assert!(sols.next().await.is_none());

    // A3: ASK, and the wrong form of result
    assert!(ds.ask("ASK { ?s <http://e/age> 42 }").await.unwrap());
    let e = ds.select("ASK {}").await.unwrap_err();
    assert!(matches!(e, Error::UnexpectedResults { .. }), "{e}");

    // A4: CONSTRUCT streams triples
    let triples = ds
        .construct("CONSTRUCT WHERE { ?s ?p ?o }")
        .await
        .unwrap()
        .collect()
        .await
        .unwrap();
    assert_eq!(triples.len(), 2);

    // A5: a past state, with a commit message that is not ASCII
    let r = ds
        .update_with(
            "DELETE DATA { <http://e/a> <http://e/age> 42 }",
            &UpdateOptions::new().message("âge retiré"),
        )
        .await
        .unwrap();
    assert_eq!(r.commit_seq, Some(2));
    assert_eq!(
        ds.commit("head").await.unwrap().message.as_deref(),
        Some("âge retiré")
    );
    let old = ds
        .query_with(
            "SELECT * { ?s ?p ?o }",
            &QueryOptions::new().at(At::Commit(1)),
        )
        .await
        .unwrap();
    assert_eq!(old.meta().at.as_deref(), Some("commit:1"));
    assert_eq!(
        old.into_solutions().unwrap().collect().await.unwrap().len(),
        2
    );
    let now = ds.select("SELECT * { ?s ?p ?o }").await.unwrap();
    assert_eq!(now.collect().await.unwrap().len(), 1);

    // a syntax error carries the server's position
    let e = ds.query("SELECT * { ?s ?p }").await.unwrap_err();
    assert_eq!(e.status(), Some(400));
    let Error::Status(s) = &e else { unreachable!() };
    assert!(s.line.is_some() && s.request_id.is_some(), "{s:?}");

    // A6: the Graph Store Protocol, with entity tags
    let g = iri("http://e/g");
    let t1 = Triple::new(iri("http://e/s"), iri("http://e/p"), Literal::from("one"));
    let t2 = Triple::new(iri("http://e/s"), iri("http://e/p"), Literal::from("two"));
    let r = ds
        .put_graph(g.clone(), RdfBody::triples([&t1]))
        .await
        .unwrap();
    assert_eq!(r.commit.as_ref().unwrap().kind, "gsp-put");
    let read = ds.get_graph(g.clone()).await.unwrap();
    let etag = read.meta().etag.clone().unwrap();
    assert_eq!(read.collect().await.unwrap(), vec![t1.clone()]);
    // unchanged: 304 and an empty stream
    let same = ds
        .get_graph_with(g.clone(), &ReadOptions::new().if_none_match(etag.clone()))
        .await
        .unwrap();
    assert!(same.meta().not_modified());
    assert!(same.collect().await.unwrap().is_empty());
    ds.post_graph(g.clone(), RdfBody::triples([&t2]))
        .await
        .unwrap();
    let e = ds
        .put_graph_with(
            g.clone(),
            RdfBody::triples([&t1]),
            &WriteOptions::new().if_match(etag),
        )
        .await
        .unwrap_err();
    assert_eq!(e.status(), Some(412));
    assert_eq!(e.code(), Some("precondition-failed"));
    let fresh = ds.get_graph(g.clone()).await.unwrap();
    let etag = fresh.meta().etag.clone().unwrap();
    assert_eq!(fresh.collect().await.unwrap().len(), 2);
    ds.put_graph_with(
        g.clone(),
        RdfBody::triples([&t1]),
        &WriteOptions::new().if_match(etag),
    )
    .await
    .unwrap();
    let quads = ds.get_dataset().await.unwrap().collect().await.unwrap();
    assert!(quads.contains(&Quad::new(
        t1.subject.clone(),
        t1.predicate.clone(),
        t1.object.clone(),
        g.clone()
    )));
    let r = ds.delete_graph(g.clone()).await.unwrap();
    assert_eq!(r.commit.unwrap().deleted, 1);
    let e = ds.get_graph(g.clone()).await.unwrap_err();
    assert_eq!(e.status(), Some(404));

    // a dry run commits nothing
    let head = ds.info().await.unwrap().head.unwrap();
    let r = ds
        .update_with(
            "INSERT DATA { <http://e/x> <http://e/p> 1 }",
            &UpdateOptions::new().dry_run(),
        )
        .await
        .unwrap();
    assert!(r.dry_run);
    assert_eq!(r.committed, Some(false));
    assert_eq!(r.commit.unwrap().seq, head + 1);
    assert_eq!(ds.info().await.unwrap().head, Some(head));

    // loads: Turtle into the default graph, gzipped N-Quads into the dataset
    let ttl = dir.path().join("more.ttl");
    std::fs::write(&ttl, "<http://e/b> <http://e/name> \"Bob\" .\n").unwrap();
    let r = ds.load(&ttl).await.unwrap();
    assert_eq!(r.commit.unwrap().inserted, 1);
    let nq = dir.path().join("more.nq.gz");
    let mut gz = flate2::write::GzEncoder::new(
        std::fs::File::create(&nq).unwrap(),
        flate2::Compression::default(),
    );
    gz.write_all(b"<http://e/c> <http://e/name> \"Cy\" <http://e/g2> .\n")
        .unwrap();
    gz.finish().unwrap();
    let r = ds.load(&nq).await.unwrap();
    assert_eq!(r.commit.unwrap().inserted, 1);

    // an upload with a CSV table
    let r = ds
        .upload(
            vec![UploadPart::bytes("people.csv", "id,name\np1,Dee\np2,Eve\n")],
            &UploadOptions::new()
                .base("http://e/people/")
                .key("id")
                .message("people"),
        )
        .await
        .unwrap();
    assert_eq!(r.committed, Some(true));
    assert!(r.commit.unwrap().inserted >= 2);

    // a batch is one commit
    let head = ds.info().await.unwrap().head.unwrap();
    let q = Quad::new(
        iri("http://e/d"),
        iri("http://e/name"),
        Literal::from("Di"),
        iri("http://e/g3"),
    );
    let r = ds
        .batch()
        .insert([&q])
        .update("INSERT DATA { <http://e/d> <http://e/age> 7 }")
        .commit()
        .await
        .unwrap();
    assert_eq!(r.commit_seq, Some(head + 1));
    assert_eq!(r.commit.unwrap().inserted, 2);

    // A7: a stored query by name, with a typed parameter
    ds.put_stored_query(
        "older",
        &serde_json::json!({
            "query": "SELECT ?s WHERE { ?s <http://e/age> ?a FILTER(?a >= ?min) }",
            "parameters": { "min": { "type": "integer" } }
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        ds.stored_queries().await.unwrap()["queries"][0]["name"],
        "older"
    );
    let res = ds
        .run_stored("older", &serde_json::json!({ "min": 5 }))
        .await
        .unwrap();
    assert_eq!(res.meta().query_version, Some(1));
    let rows = res.into_solutions().unwrap().collect().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("s"), Some(&Term::from(iri("http://e/d"))));
    assert_eq!(
        ds.stored_query("older").await.unwrap()["version"]["version"],
        1
    );
    ds.delete_stored_query("older").await.unwrap();

    // commits, statistics, the schema report
    let page = ds
        .commits(&CommitsOptions {
            limit: Some(2),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.commits.len(), 2);
    assert_eq!(page.commits[0].seq, page.head);
    assert!(page.next.is_some());
    let stats = ds.stats(None).await.unwrap();
    assert!(stats.is_object());
    let schema = ds.schema(Some(At::Head)).await.unwrap();
    assert_eq!(schema["dataset"], "lib");
    let c = client
        .call_json("getCommit", &[("ds", "lib"), ("reference", "1")], &[], None)
        .await
        .unwrap();
    assert_eq!(c["commit"]["seq"], 1);

    // A8: a backup task, polled to the end
    let task = ds.backup_nquads().await.unwrap();
    let done = client
        .wait_for_task(&task.id, Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(done.state, "done", "{done:?}");
    assert!(
        client
            .tasks()
            .await
            .unwrap()
            .iter()
            .any(|t| t.id == task.id)
    );
    assert!(
        client.backup_files().await.unwrap()["backups"][0]
            .as_str()
            .unwrap()
            .starts_with("lib_")
    );
    assert_eq!(ds.backups().await.unwrap()["dataset"], "lib");
    let tmp = client
        .create_dataset("scratch", DatasetType::Memory)
        .await
        .unwrap();
    tmp.update("INSERT DATA { <urn:a> <urn:b> <urn:c> }")
        .await
        .unwrap();
    client.delete_dataset("scratch").await.unwrap();
    assert_eq!(tmp.ask("ASK {}").await.unwrap_err().status(), Some(404));

    // A10: the same server as a plain SPARQL endpoint gets no Sparkles parameters
    let plain = Client::new(&server.url)
        .unwrap()
        .endpoint(format!("{}/lib/sparql", server.url))
        .unwrap()
        .with_update_url(format!("{}/lib/update", server.url))
        .unwrap();
    let r = plain
        .update("INSERT DATA { <http://e/z> <http://e/p> 1 }")
        .await
        .unwrap();
    assert!(r.commit.is_none() && r.committed.is_none());
    assert!(r.commit_seq.is_some());
    assert!(plain.ask("ASK { <http://e/z> ?p ?o }").await.unwrap());
    let e = plain
        .query_with("ASK {}", &QueryOptions::new().at(1))
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Config(_)), "{e}");
    let fuseki = sparkles_client::Endpoint::fuseki(&server.url, "lib").unwrap();
    assert!(
        fuseki
            .get_graph(Graph::Default)
            .await
            .unwrap()
            .collect()
            .await
            .unwrap()
            .len()
            >= 2
    );
}

/// A12 and A9: the blocking facade, against a server with authentication.
#[cfg(feature = "auth")]
#[test]
fn blocking_client_with_authentication() {
    let dir = tempfile::tempdir().unwrap();
    let minted = Command::new(BIN)
        .args(["auth", "gen-token", "--name", "ci"])
        .output()
        .unwrap();
    assert!(minted.status.success());
    let token = String::from_utf8(minted.stdout).unwrap().trim().to_string();
    let entry = String::from_utf8(minted.stderr).unwrap();
    let hash = entry
        .lines()
        .find_map(|l| l.strip_prefix("hash = \""))
        .unwrap()
        .trim_end_matches('"')
        .to_string();
    let cfg = dir.path().join("auth.toml");
    std::fs::write(
        &cfg,
        format!(
            "version = 1\n[[tokens]]\nname = \"ci\"\nhash = \"{hash}\"\ndatasets = {{ wiki = \"write\" }}\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let server = serve(
        dir.path(),
        5545..5550,
        &["--mem", "wiki", "--auth-config", cfg.to_str().unwrap()],
    );

    // without credentials, and with a wrong token: 401
    use sparkles_client::ClientBuilder;
    let anon = sparkles_client::blocking::Client::new(&server.url).unwrap();
    assert_eq!(anon.whoami().unwrap().principal.kind, "anonymous");
    let e = anon.dataset("wiki").ask("ASK {}").unwrap_err();
    assert_eq!(e.status(), Some(401));
    let wrong = sparkles_client::blocking::Client::from_async(
        ClientBuilder::new()
            .base_url(&server.url)
            .bearer_token("spk_0000000000000000000000000000000000000000000")
            .build()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        wrong.dataset("wiki").ask("ASK {}").unwrap_err().status(),
        Some(401)
    );

    let client = sparkles_client::blocking::Client::from_async(
        ClientBuilder::new()
            .base_url(&server.url)
            .bearer_token(&token)
            .build()
            .unwrap(),
    )
    .unwrap();
    let who = client.whoami().unwrap();
    assert!(who.auth_enabled);
    assert_eq!(who.principal.kind, "token");
    assert_eq!(who.datasets["wiki"], "write");

    let ds = client.dataset("wiki");
    let r = ds
        .update(r#"INSERT DATA { <http://e/a> <http://e/name> "Ann"@en }"#)
        .unwrap();
    assert_eq!(r.commit_seq, Some(1));
    let rows: Vec<_> = ds
        .select("SELECT ?o { ?s ?p ?o }")
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(ds.ask("ASK { ?s ?p \"Ann\"@en }").unwrap());
    let triples: Vec<_> = ds
        .construct("CONSTRUCT WHERE { ?s ?p ?o }")
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(triples.len(), 1);
    let r = ds
        .batch()
        .update("INSERT DATA { <http://e/b> <http://e/name> \"Bob\" }")
        .commit()
        .unwrap();
    assert_eq!(r.commit_seq, Some(2));
    // the token's grant is write, not admin
    assert_eq!(
        ds.put_stored_query("q", &serde_json::json!({"query": "ASK {}"}))
            .unwrap_err()
            .status(),
        Some(403)
    );
    // credentials are refused over plain http to another host
    assert!(
        ClientBuilder::new()
            .base_url("http://sparql.example.org")
            .bearer_token(&token)
            .build()
            .is_err()
    );
}
