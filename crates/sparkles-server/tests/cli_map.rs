//! `sparkles map` and `sparkles load --mapping` run as the real binary (spec F19): a
//! YARRRML mapping over CSV files run to standard output and loaded, a check that names
//! a missing column, conversion to RML-Core, stored mappings run into a database in add
//! and replace mode, and CSVW standard mode with a failing foreign key.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

fn sparkles(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn expect(dir: &Path, args: &[&str], code: i32) -> Output {
    let o = sparkles(dir, args);
    assert_eq!(
        o.status.code(),
        Some(code),
        "sparkles {args:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const YARRRML: &str = r#"prefixes:
  ex: http://ex.org/
mappings:
  person:
    sources:
      - [people.csv~csv]
    s: ex:person/$(id)
    po:
      - [a, ex:Person]
      - [ex:name, $(name)]
      - p: ex:city
        o:
          mapping: city
          condition:
            function: equal
            parameters:
              - [str1, $(city)]
              - [str2, $(id)]
  city:
    sources:
      - [cities.csv~csv]
    s: ex:city/$(id)
    po:
      - [ex:label, $(label)]
"#;

fn setup() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("people.csv"), "id,name,city\n1,Ann,b\n2,Bo,p\n").unwrap();
    std::fs::write(d.join("cities.csv"), "id,label\nb,Berlin\np,Paris\n").unwrap();
    std::fs::write(d.join("map.yml"), YARRRML).unwrap();
    dir
}

#[test]
fn runs_checks_and_converts_a_mapping() {
    let dir = setup();
    let d = dir.path();
    let o = expect(d, &["map", "run", "map.yml"], 0);
    let nq = out(&o);
    assert!(
        nq.contains("<http://ex.org/person/1> <http://ex.org/city> <http://ex.org/city/b> ."),
        "{nq}"
    );
    assert!(
        nq.contains("<http://ex.org/city/p> <http://ex.org/label> \"Paris\" ."),
        "{nq}"
    );
    assert!(
        err(&o).contains("6 quads") || err(&o).contains("quads"),
        "{}",
        err(&o)
    );

    // a source given by name
    std::fs::rename(d.join("cities.csv"), d.join("towns.csv")).unwrap();
    let o = expect(d, &["map", "run", "map.yml"], 1);
    assert!(err(&o).contains("cities.csv"), "{}", err(&o));
    let o = expect(
        d,
        &[
            "map",
            "run",
            "map.yml",
            "--source",
            "cities.csv=towns.csv",
            "-o",
            "out.nq",
        ],
        0,
    );
    let _ = o;
    let nq = std::fs::read_to_string(d.join("out.nq")).unwrap();
    assert!(nq.contains("Berlin"), "{nq}");
    std::fs::rename(d.join("towns.csv"), d.join("cities.csv")).unwrap();

    // check lists the sources and the join, and names a missing column
    let o = expect(d, &["map", "check", "map.yml"], 0);
    let s = out(&o);
    assert!(s.contains("CSV file people.csv"), "{s}");
    assert!(
        s.contains("every referenced column is in the header"),
        "{s}"
    );
    assert!(s.contains("joins:"), "{s}");
    std::fs::write(d.join("people.csv"), "id,fullname,city\n1,Ann,b\n").unwrap();
    let o = expect(d, &["map", "check", "map.yml"], 0);
    assert!(out(&o).contains("missing columns: \"name\""), "{}", out(&o));
    std::fs::write(d.join("people.csv"), "id,name,city\n1,Ann,b\n2,Bo,p\n").unwrap();

    // functions are listed
    let o = expect(d, &["map", "check", "--functions"], 0);
    assert!(out(&o).contains("toUpperCase"), "{}", out(&o));

    // convert prints RML-Core that runs to the same quads
    let o = expect(d, &["map", "convert", "map.yml"], 0);
    let ttl = out(&o);
    assert!(ttl.contains("rml:"), "{ttl}");
    std::fs::write(d.join("map.rml.ttl"), &ttl).unwrap();
    let a = out(&expect(d, &["map", "run", "map.yml"], 0));
    let b = out(&expect(d, &["map", "run", "map.rml.ttl"], 0));
    let sort = |s: &str| {
        let mut v: Vec<&str> = s.lines().collect();
        v.sort();
        v.join("\n")
    };
    assert_eq!(sort(&a), sort(&b));

    // CSVW metadata is not a mapping for `map run`
    std::fs::write(
        d.join("m.json"),
        r#"{"@context": "http://www.w3.org/ns/csvw", "url": "people.csv"}"#,
    )
    .unwrap();
    let o = expect(d, &["map", "run", "m.json"], 1);
    assert!(err(&o).contains("sparkles load"), "{}", err(&o));
}

#[test]
fn loads_through_a_mapping() {
    let dir = setup();
    let d = dir.path();
    expect(d, &["load", "--loc", "db", "--mapping", "map.yml"], 0);
    let o = expect(
        d,
        &[
            "query",
            "--loc",
            "db",
            "SELECT ?n WHERE { ?p <http://ex.org/city> ?c . ?c <http://ex.org/label> ?n } ORDER BY ?n",
        ],
        0,
    );
    let s = out(&o);
    assert!(s.contains("\"Berlin\"") && s.contains("\"Paris\""), "{s}");
    // --graph puts every quad into one graph, and a mapping's own graph maps conflict
    expect(
        d,
        &[
            "load",
            "--loc",
            "db2",
            "--mapping",
            "map.yml",
            "--graph",
            "urn:g",
        ],
        0,
    );
    let o = expect(
        d,
        &[
            "query",
            "--loc",
            "db2",
            "ASK { GRAPH <urn:g> { ?p <http://ex.org/city> ?c } }",
        ],
        0,
    );
    assert_eq!(out(&o).trim(), "yes");
    let graphed = YARRRML.replace(
        "    s: ex:city/$(id)\n",
        "    s: ex:city/$(id)\n    graphs: ex:other\n",
    );
    std::fs::write(d.join("graphed.yml"), graphed).unwrap();
    let o = expect(
        d,
        &[
            "load",
            "--loc",
            "db2",
            "--mapping",
            "graphed.yml",
            "--graph",
            "urn:g",
        ],
        1,
    );
    assert!(err(&o).contains("mapping-graph-conflict"), "{}", err(&o));
}

#[test]
fn stored_mappings_run_into_a_database() {
    let dir = setup();
    let d = dir.path();
    std::fs::write(
        d.join("seed.ttl"),
        "<http://ex.org/x> <http://ex.org/y> 1 .\n",
    )
    .unwrap();
    expect(d, &["load", "--loc", "db", "seed.ttl"], 0);
    // a mapping into a named graph, so replace mode knows what it clears
    let graphed = YARRRML
        .replace(
            "    s: ex:person/$(id)\n",
            "    s: ex:person/$(id)\n    graphs: ex:g\n",
        )
        .replace(
            "    s: ex:city/$(id)\n",
            "    s: ex:city/$(id)\n    graphs: ex:g\n",
        );
    std::fs::write(d.join("g.yml"), &graphed).unwrap();
    let o = expect(
        d,
        &[
            "map",
            "put",
            "people",
            "g.yml",
            "--loc",
            "db",
            "--description",
            "people and cities",
        ],
        0,
    );
    assert!(out(&o).contains("version 1"), "{}", out(&o));
    let o = expect(d, &["map", "list", "--loc", "db"], 0);
    assert!(out(&o).contains("people  v1  yarrrml"), "{}", out(&o));
    let o = expect(d, &["map", "get", "people", "--loc", "db"], 0);
    assert_eq!(out(&o), graphed);

    let o = expect(d, &["map", "run", "people", "--loc", "db", "--dry-run"], 0);
    assert!(out(&o).contains("dry run"), "{}", out(&o));
    let o = expect(d, &["map", "run", "people", "--loc", "db"], 0);
    assert!(out(&o).contains("commit"), "{}", out(&o));
    let count = |d: &Path| {
        let o = expect(
            d,
            &[
                "query",
                "--loc",
                "db",
                "SELECT (COUNT(*) AS ?n) WHERE { GRAPH <http://ex.org/g> { ?s ?p ?o } }",
            ],
            0,
        );
        out(&o)
    };
    assert!(count(d).contains("8"), "{}", count(d));
    // one person fewer: replace mode removes Bo's triples, add mode would keep them
    std::fs::write(d.join("people.csv"), "id,name,city\n1,Ann,b\n").unwrap();
    expect(
        d,
        &["map", "run", "people", "--loc", "db", "--mode", "replace"],
        0,
    );
    assert!(count(d).contains("5"), "{}", count(d));
    // the default graph was not touched
    let o = expect(
        d,
        &[
            "query",
            "--loc",
            "db",
            "ASK { <http://ex.org/x> <http://ex.org/y> 1 }",
        ],
        0,
    );
    assert!(out(&o).trim() == "yes", "{}", out(&o));
    let o = expect(d, &["map", "list", "--loc", "db"], 0);
    assert!(out(&o).contains("last run"), "{}", out(&o));

    // replace mode refuses a template graph map
    let templated = YARRRML.replace(
        "    s: ex:person/$(id)\n",
        "    s: ex:person/$(id)\n    graphs: ex:g/$(id)\n",
    );
    std::fs::write(d.join("t.yml"), templated).unwrap();
    expect(d, &["map", "put", "tg", "t.yml", "--loc", "db"], 0);
    let o = expect(
        d,
        &["map", "run", "tg", "--loc", "db", "--mode", "replace"],
        1,
    );
    assert!(err(&o).contains("constant graph maps"), "{}", err(&o));

    expect(d, &["map", "delete", "people", "--loc", "db"], 0);
    expect(d, &["map", "get", "people", "--loc", "db"], 1);
}

#[test]
fn csvw_standard_mode_checks_foreign_keys() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("customer.csv"), "id,name\n1,Ann\n").unwrap();
    std::fs::write(d.join("orders.csv"), "id,customer\n10,1\n11,2\n").unwrap();
    std::fs::write(
        d.join("meta.json"),
        r#"{"@context": "http://www.w3.org/ns/csvw", "tables": [
            {"url": "customer.csv", "tableSchema": {"primaryKey": "id",
              "aboutUrl": "http://ex.org/c/{id}",
              "columns": [{"name": "id"}, {"name": "name"}]}},
            {"url": "orders.csv", "tableSchema": {
              "aboutUrl": "http://ex.org/o/{id}",
              "columns": [{"name": "id"}, {"name": "customer"}],
              "foreignKeys": [{"columnReference": "customer",
                "reference": {"resource": "customer.csv", "columnReference": "id"}}]}}]}"#,
    )
    .unwrap();
    let o = expect(
        d,
        &[
            "load",
            "--loc",
            "db",
            "--mapping",
            "meta.json",
            "--csvw-mode",
            "standard",
        ],
        1,
    );
    assert!(
        err(&o).contains("is not among the values of column id"),
        "{}",
        err(&o)
    );
    std::fs::write(d.join("orders.csv"), "id,customer\n10,1\n").unwrap();
    expect(
        d,
        &[
            "load",
            "--loc",
            "db",
            "--mapping",
            "meta.json",
            "--csvw-mode",
            "standard",
        ],
        0,
    );
    let o = expect(
        d,
        &[
            "query",
            "--loc",
            "db",
            "ASK { ?g a <http://www.w3.org/ns/csvw#TableGroup> ; <http://www.w3.org/ns/csvw#table> ?t . ?t <http://www.w3.org/ns/csvw#row> ?r }",
        ],
        0,
    );
    assert!(out(&o).trim() == "yes", "{}", out(&o));
    // the two tables belong to one table group
    let o = expect(
        d,
        &[
            "query",
            "--loc",
            "db",
            "ASK { ?g a <http://www.w3.org/ns/csvw#TableGroup> ; <http://www.w3.org/ns/csvw#table> ?a , ?b . FILTER (?a != ?b) }",
        ],
        0,
    );
    assert!(out(&o).trim() == "yes", "{}", out(&o));
}

#[test]
fn templates_read_the_target_database_with_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(
        d.join("countries.ttl"),
        "<http://ex.org/country/de> <http://ex.org/isoCode> \"DE\" .\n",
    )
    .unwrap();
    expect(d, &["load", "--loc", "db", "countries.ttl"], 0);
    std::fs::write(d.join("people.csv"), "id,code\n1,DE\n").unwrap();
    std::fs::write(
        d.join("t.rq"),
        "PREFIX ex: <http://ex.org/>\nCONSTRUCT { ?p ex:country ?c } WHERE {\n  BIND(IRI(CONCAT(\"http://ex.org/p/\", ?id)) AS ?p)\n  ?c ex:isoCode ?code\n}\n",
    )
    .unwrap();
    // without --lookup the template reads an empty dataset and makes nothing
    expect(
        d,
        &["load", "--loc", "db", "--template", "t.rq", "people.csv"],
        0,
    );
    let ask = "ASK { <http://ex.org/p/1> <http://ex.org/country> <http://ex.org/country/de> }";
    let o = expect(d, &["query", "--loc", "db", ask], 0);
    assert_eq!(out(&o).trim(), "no");
    let o = expect(
        d,
        &[
            "load",
            "--loc",
            "db",
            "--template",
            "t.rq",
            "--lookup",
            "people.csv",
        ],
        0,
    );
    assert!(err(&o).contains("read commit"), "{}", err(&o));
    let o = expect(d, &["query", "--loc", "db", ask], 0);
    assert_eq!(out(&o).trim(), "yes");
    // --lookup needs the database
    let o = expect(
        d,
        &[
            "csv",
            "convert",
            "--template",
            "t.rq",
            "--lookup",
            "people.csv",
        ],
        1,
    );
    assert!(err(&o).contains("--loc"), "{}", err(&o));
}

#[cfg(feature = "auth")]
#[test]
fn stored_mappings_on_a_server() {
    use std::process::{Child, Stdio};
    use std::time::{Duration, Instant};
    struct Server(Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let dir = setup();
    let d = dir.path();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut server = Server(
        Command::new(BIN)
            .current_dir(d)
            .args([
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--data",
                "data",
            ])
            .env_remove("SPARKLES_AUTH_CONFIG")
            .env_remove("SPARKLES_BACKUP_CONFIG")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let start = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(server.0.try_wait().unwrap().is_none(), "server exited");
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "server did not start"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    wait_ready(port);
    let url = format!("http://127.0.0.1:{port}");
    expect(d, &["dataset", "create", "ds", "--server", &url], 0);
    let at = ["--server", url.as_str(), "--dataset", "ds"];
    let with = |args: &[&'static str]| -> Vec<String> {
        args.iter()
            .map(|s| s.to_string())
            .chain(at.iter().map(|s| s.to_string()))
            .collect()
    };
    let run = |args: Vec<String>, code: i32| {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        expect(d, &a, code)
    };
    let o = run(
        with(&["map", "put", "people", "map.yml", "--message", "first café"]),
        0,
    );
    assert!(out(&o).contains("version 1"), "{}", out(&o));
    let o = run(with(&["map", "list"]), 0);
    assert!(out(&o).contains("people  v1  yarrrml"), "{}", out(&o));
    let o = run(with(&["map", "get", "people"]), 0);
    assert_eq!(out(&o), YARRRML);
    let o = run(
        with(&[
            "map",
            "run",
            "people",
            "--source",
            "people.csv=people.csv",
            "--source",
            "cities.csv=cities.csv",
        ]),
        0,
    );
    assert!(out(&o).contains("\"quads\": 8"), "{}", out(&o));
    run(with(&["map", "delete", "people"]), 0);
    run(with(&["map", "get", "people"]), 1);
}

/// Wait until the server on `port` answers `GET /$/ready` with `200`: it listens before
/// its datasets open.
#[cfg(feature = "auth")]
fn wait_ready(port: u16) {
    use std::io::{Read as _, Write as _};
    let t0 = std::time::Instant::now();
    loop {
        if let Ok(mut c) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            let _ = write!(c, "GET /$/ready HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
            let mut buf = String::new();
            let _ = c.read_to_string(&mut buf);
            if buf.starts_with("HTTP/1.0 200") || buf.starts_with("HTTP/1.1 200") {
                return;
            }
        }
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(60),
            "the server never became ready"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
