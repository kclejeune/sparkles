use super::*;
use crate::encode::{DEFAULT_GRAPH, NONE, OP_ADD, OP_DELETE, UNION_GRAPH, write_term};
use oxrdf::{BlankNode, Literal};

fn opts(mode: BlankNodeMode) -> DatasetOptions {
    DatasetOptions {
        blank_node_labels: mode,
        read_only: false,
        term_cache_size: 1000,
    }
}

fn qopts() -> QueryOpts {
    QueryOpts {
        base_iri: None,
        union_default_graph: None,
        include_inferred: false,
        timeout_ms: None,
        max_rows: None,
        max_memory_bytes: None,
        max_rows_produced: None,
        allow_service: false,
        allow_private_network: true,
        binding_names: Vec::new(),
        binding_values: Vec::new(),
        no_cache: false,
    }
}

fn iri(s: &str) -> Term {
    NamedNode::new_unchecked(format!("http://ex.org/{s}")).into()
}

fn bnode(l: &str) -> Term {
    BlankNode::new_unchecked(l).into()
}

/// A graph position: `None` is the default graph.
fn quad_items(buf: &mut Vec<u8>, g: Option<&Term>, s: &Term, p: &Term, o: &Term) {
    match g {
        None => buf.push(DEFAULT_GRAPH),
        Some(g) => write_term(buf, g, &|_| None),
    }
    for t in [s, p, o] {
        write_term(buf, t, &|_| None);
    }
}

fn op(buf: &mut Vec<u8>, add: bool, g: Option<&Term>, s: &Term, p: &Term, o: &Term) {
    buf.push(if add { OP_ADD } else { OP_DELETE });
    quad_items(buf, g, s, p, o);
}

/// A pattern: `None` items are wildcards; the graph is any, the default, the union or a
/// term.
enum G<'a> {
    Any,
    Default,
    Union,
    Named(&'a Term),
}

fn pattern(g: G<'_>, s: Option<&Term>, p: Option<&Term>, o: Option<&Term>) -> Vec<u8> {
    let mut b = Vec::new();
    match g {
        G::Any => b.push(NONE),
        G::Default => b.push(DEFAULT_GRAPH),
        G::Union => b.push(UNION_GRAPH),
        G::Named(t) => write_term(&mut b, t, &|_| None),
    }
    for t in [s, p, o] {
        match t {
            None => b.push(NONE),
            Some(t) => write_term(&mut b, t, &|_| None),
        }
    }
    b
}

/// Decode a row batch with a running term table.
fn rows(batch: &[u8], table: &mut Vec<Option<Term>>) -> Vec<Vec<Option<Term>>> {
    let none = |_: &str| None;
    assert_eq!(batch[0], encode::ENCODING_VERSION as u8);
    if batch[1] & encode::FLAG_RESTART != 0 {
        table.clear();
    }
    let n = u32::from_le_bytes(batch[2..6].try_into().unwrap()) as usize;
    let mut r = Reader::new(&batch[6..], &none);
    for _ in 0..n {
        table.push(match r.item().unwrap() {
            Item::Term(t) => Some(t),
            Item::DefaultGraph => None,
            i => panic!("{i:?}"),
        });
    }
    // the reader consumed the terms; find where the rows start
    let consumed = 6 + r_pos(&batch[6..], n);
    let rows = u32::from_le_bytes(batch[consumed..consumed + 4].try_into().unwrap()) as usize;
    let cols = u16::from_le_bytes(batch[consumed + 4..consumed + 6].try_into().unwrap()) as usize;
    let mut out = Vec::new();
    let mut at = consumed + 6;
    for _ in 0..rows {
        let mut row = Vec::new();
        for _ in 0..cols {
            let c = u32::from_le_bytes(batch[at..at + 4].try_into().unwrap()) as usize;
            at += 4;
            row.push(if c == 0 { None } else { table[c - 1].clone() });
        }
        out.push(row);
    }
    assert_eq!(at, batch.len());
    out
}

/// The length of `n` terms at the start of `b`.
fn r_pos(b: &[u8], n: usize) -> usize {
    for len in 0..=b.len() {
        let none = |_: &str| None;
        let mut r = Reader::new(&b[..len], &none);
        if (0..n).all(|_| r.item().is_ok()) && r.at_end() {
            return len;
        }
    }
    panic!("terms do not parse")
}

fn find_all(ds: &FfiDataset, pat: &[u8]) -> Vec<Vec<Option<Term>>> {
    let mut table = Vec::new();
    let f = ds.find(pat.to_vec(), 2).unwrap();
    let mut out = rows(&f.batch, &mut table);
    if let Some(c) = f.cursor {
        loop {
            let b = c.next_batch(3).unwrap();
            out.extend(rows(&b.batch, &mut table));
            if b.done {
                break;
            }
        }
    }
    out
}

#[test]
fn write_read_and_query() {
    let ds = FfiDataset::memory(opts(BlankNodeMode::Dataset));
    let g = iri("g");
    let w = ds.begin_write(None, true).unwrap().unwrap();
    let mut b = Vec::new();
    for i in 0..10 {
        op(
            &mut b,
            true,
            None,
            &iri(&format!("s{i}")),
            &iri("p"),
            &Literal::from(i).into(),
        );
    }
    op(
        &mut b,
        true,
        Some(&g),
        &iri("s0"),
        &iri("p"),
        &Literal::from(0).into(),
    );
    op(&mut b, true, Some(&g), &iri("s1"), &iri("q"), &iri("o"));
    let st = w.apply(b).unwrap();
    assert_eq!((st.inserted, st.deleted), (12, 0));
    // the transaction sees its changes; the dataset does not until the commit
    assert_eq!(w.count(pattern(G::Any, None, None, None)).unwrap(), 12);
    assert_eq!(ds.count(pattern(G::Any, None, None, None)).unwrap(), 0);
    let q = w
        .prepare_query("SELECT (COUNT(*) AS ?n) { ?s ?p ?o }".into(), qopts())
        .unwrap();
    let e = q.execute(10).unwrap();
    assert_eq!(e.variables, vec!["n".to_string()]);
    assert_eq!(
        rows(&e.batch, &mut Vec::new()),
        vec![vec![Some(Literal::from(10).into())]]
    );
    let r = w.commit().unwrap();
    assert!(r.committed);
    assert_eq!(r.commit.inserted, 12);
    assert_eq!(ds.head_commit().seq, r.commit.seq);

    // find in small batches, the graph first and the default graph as its own tag
    let all = find_all(&ds, &pattern(G::Any, None, None, None));
    assert_eq!(all.len(), 12);
    assert_eq!(all.iter().filter(|r| r[0].is_none()).count(), 10);
    assert_eq!(
        find_all(&ds, &pattern(G::Named(&g), None, None, None)).len(),
        2
    );
    assert_eq!(
        find_all(&ds, &pattern(G::Default, None, None, None)).len(),
        10
    );
    // the union graph removes the duplicate triple of the named graphs only
    assert_eq!(find_all(&ds, &pattern(G::Union, None, None, None)).len(), 2);
    assert_eq!(
        find_all(&ds, &pattern(G::Any, Some(&iri("s0")), None, None)).len(),
        2
    );
    // a literal subject matches nothing
    let lit: Term = Literal::from(1).into();
    assert!(find_all(&ds, &pattern(G::Any, Some(&lit), None, None)).is_empty());
    let one = ds
        .find(pattern(G::Any, Some(&iri("s9")), None, None), 64)
        .unwrap();
    assert!(one.cursor.is_none());
    let mut quad = Vec::new();
    quad_items(&mut quad, Some(&g), &iri("s1"), &iri("q"), &iri("o"));
    assert!(ds.contains(quad).unwrap());
    assert!(
        ds.contains(pattern(G::Named(&g), None, None, None))
            .unwrap()
    );
    assert!(ds.contains(pattern(G::Union, None, None, None)).unwrap());
    // A union existence check must ignore triples found only in the default graph.
    assert!(
        ds.contains(pattern(G::Any, Some(&iri("s9")), None, None))
            .unwrap()
    );
    assert!(
        !ds.contains(pattern(G::Union, Some(&iri("s9")), None, None))
            .unwrap()
    );
    assert!(
        !ds.contains(pattern(G::Any, Some(&iri("missing")), None, None))
            .unwrap()
    );
    assert!(
        !ds.contains(pattern(G::Any, Some(&lit), None, None))
            .unwrap()
    );
    assert_eq!(
        rows(&ds.graph_names().unwrap(), &mut Vec::new()),
        vec![vec![Some(g.clone())]]
    );

    // a read transaction keeps its snapshot
    let rt = ds.begin_read();
    let w = ds.begin_write(None, true).unwrap().unwrap();
    assert_eq!(
        w.remove_matching(pattern(G::Default, None, None, None))
            .unwrap(),
        10
    );
    w.commit().unwrap();
    assert_eq!(rt.count(pattern(G::Any, None, None, None)).unwrap(), 12);
    assert_eq!(ds.count(pattern(G::Any, None, None, None)).unwrap(), 2);
    assert!(rt.contains(pattern(G::Default, None, None, None)).unwrap());
    assert!(!ds.contains(pattern(G::Default, None, None, None)).unwrap());
}

#[test]
fn blank_node_labels() {
    for mode in [BlankNodeMode::Dataset, BlankNodeMode::Transaction] {
        let ds = FfiDataset::memory(opts(mode));
        let x = bnode("jena-made-1");
        let w = ds.begin_write(None, true).unwrap().unwrap();
        let mut b = Vec::new();
        op(&mut b, true, None, &x, &iri("p"), &iri("o"));
        op(&mut b, true, None, &x, &iri("q"), &iri("o"));
        w.apply(b).unwrap();
        // in the transaction, the label names the node it made, and reads return it
        let found = w.find(pattern(G::Any, Some(&x), None, None), 10).unwrap();
        let r = rows(&found.batch, &mut Vec::new());
        assert_eq!(r.len(), 2);
        assert_eq!(r[0][1], Some(x.clone()));
        w.commit().unwrap();
        let later = find_all(&ds, &pattern(G::Any, Some(&x), None, None));
        match mode {
            BlankNodeMode::Dataset => {
                assert_eq!(later.len(), 2);
                assert_eq!(later[0][1], Some(x.clone()));
                assert_eq!(ds.label_count(), 1);
            }
            BlankNodeMode::Transaction => {
                assert!(later.is_empty());
                // the node is there with its stored label
                let all = find_all(&ds, &pattern(G::Any, None, Some(&iri("p")), None));
                let Some(Term::BlankNode(stored)) = &all[0][1] else {
                    panic!("{all:?}")
                };
                assert!(stored.as_str().starts_with('b'));
                // which names it
                let s: Term = stored.clone().into();
                assert_eq!(
                    find_all(&ds, &pattern(G::Any, Some(&s), None, None)).len(),
                    2
                );
            }
        }
        // an aborted transaction leaves no label behind
        let w = ds.begin_write(None, true).unwrap().unwrap();
        let mut b = Vec::new();
        op(&mut b, true, None, &bnode("gone"), &iri("p"), &iri("o"));
        w.apply(b).unwrap();
        w.abort();
        assert!(find_all(&ds, &pattern(G::Any, Some(&bnode("gone")), None, None)).is_empty());
    }
}

#[test]
fn promotion_and_failures() {
    let ds = FfiDataset::memory(opts(BlankNodeMode::Dataset));
    let rt = ds.begin_read();
    let base = rt.commit_seq();
    // another writer commits a change
    let w = ds.begin_write(None, true).unwrap().unwrap();
    let mut b = Vec::new();
    op(&mut b, true, None, &iri("a"), &iri("p"), &iri("o"));
    w.apply(b).unwrap();
    w.commit().unwrap();
    assert!(ds.begin_write(Some(base), true).unwrap().is_none());
    let now = ds.begin_read().commit_seq();
    let w = ds.begin_write(Some(now), true).unwrap().unwrap();
    // a syntax error leaves the transaction usable; another failure aborts it
    assert_eq!(
        w.update("NOT SPARQL".into(), qopts()).unwrap_err().kind(),
        ErrorKind::SparqlSyntax
    );
    let st = w
        .update(
            "INSERT DATA { <http://ex.org/b> <http://ex.org/p> 1 }".into(),
            qopts(),
        )
        .unwrap();
    assert_eq!(st.inserted, 1);
    assert!(w.is_open());
    assert!(
        w.update("LOAD <file:///does/not/exist.ttl>".into(), qopts())
            .is_err()
    );
    assert!(!w.is_open());
    assert_eq!(w.commit().unwrap_err().kind(), ErrorKind::TransactionEnded);
    assert_eq!(ds.count(pattern(G::Any, None, None, None)).unwrap(), 1);
    // the lock was released
    ds.begin_write(None, true).unwrap().unwrap().abort();
}

#[test]
fn queries_and_cancellation() {
    let ds = FfiDataset::memory(opts(BlankNodeMode::Dataset));
    ds.load_bytes(
        b"<http://ex.org/g> { <http://ex.org/a> <http://ex.org/p> 1 } <http://ex.org/b> <http://ex.org/p> 2 .".to_vec(),
        "application/trig".into(),
        None,
        None,
    )
    .unwrap();
    let run = |text: &str, o: QueryOpts| {
        let q = ds.prepare_query(text.into(), o).unwrap();
        let e = q.execute(1).unwrap();
        let mut table = Vec::new();
        let mut out = rows(&e.batch, &mut table);
        let mut done = e.done;
        while !done {
            let b = q.next_batch(1);
            out.extend(rows(&b.batch, &mut table));
            done = b.done;
        }
        (e, out)
    };
    let (_, r) = run("SELECT ?s ?o { ?s ?p ?o } ORDER BY ?o", qopts());
    assert_eq!(r.len(), 1);
    let mut u = qopts();
    u.union_default_graph = Some(true);
    let (_, r) = run("SELECT ?s ?o { ?s ?p ?o } ORDER BY ?o", u);
    assert_eq!(r, vec![vec![Some(iri("a")), Some(Literal::from(1).into())]]);
    let (e, _) = run("ASK { ?s ?p 2 }", qopts());
    assert!(e.boolean);
    assert_eq!(e.kind, FfiQueryKind::Ask);
    let (e, r) = run(
        "CONSTRUCT { ?s ?p ?o . GRAPH <http://ex.org/h> { ?s ?p ?o } } WHERE { GRAPH ?g { ?s ?p ?o } }",
        qopts(),
    );
    assert_eq!(e.kind, FfiQueryKind::Construct);
    assert_eq!(r.len(), 2);
    assert!(r.iter().any(|q| q[0].is_none()));
    // pre-bound variables
    let mut o = qopts();
    o.binding_names = vec!["s".into()];
    let mut v = Vec::new();
    write_term(&mut v, &iri("b"), &|_| None);
    o.binding_values = v;
    let (_, r) = run("SELECT ?o { ?s ?p ?o }", o);
    assert_eq!(r, vec![vec![Some(Literal::from(2).into())]]);
    // syntax errors
    let q = ds
        .prepare_query("SELECT * WHERE {".into(), qopts())
        .unwrap();
    assert_eq!(q.execute(1).unwrap_err().kind(), ErrorKind::SparqlSyntax);
    // cancellation from another thread
    let mut nt = String::new();
    for i in 0..3000 {
        nt.push_str(&format!(
            "<http://ex.org/x{i}> <http://ex.org/r> <http://ex.org/y{i}> .\n"
        ));
    }
    ds.load_bytes(nt.into_bytes(), "nt".into(), None, None)
        .unwrap();
    let q = ds
        .prepare_query(
            "SELECT (COUNT(*) AS ?n) { ?a <http://ex.org/r> ?b . ?c <http://ex.org/r> ?d . ?e <http://ex.org/r> ?f FILTER(STR(?b) < STR(?d) && STR(?d) < STR(?f)) }".into(),
            qopts(),
        )
        .unwrap();
    let q2 = q.clone();
    let t = std::thread::spawn(move || q2.execute(1).map(|_| ()));
    std::thread::sleep(std::time::Duration::from_millis(100));
    let start = std::time::Instant::now();
    q.cancel();
    let r = t.join().unwrap();
    assert_eq!(r.unwrap_err().kind(), ErrorKind::Cancelled);
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn persistent_dataset_is_locked_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db").to_string_lossy().into_owned();
    {
        let ds = FfiDataset::open(path.clone(), opts(BlankNodeMode::Dataset)).unwrap();
        ds.set_prefix("ex".into(), "http://ex.org/".into()).unwrap();
        let file = dir.path().join("d.ttl");
        std::fs::write(
            &file,
            "<http://ex.org/a> <http://ex.org/p> <http://ex.org/b> .",
        )
        .unwrap();
        let r = ds
            .load_files(
                vec![file.to_string_lossy().into_owned()],
                Some("http://ex.org/g".into()),
            )
            .unwrap();
        assert!(r.committed);
        assert_eq!(r.commit.quads, 1);
    }
    let ds = FfiDataset::open(path, opts(BlankNodeMode::Dataset)).unwrap();
    assert_eq!(
        ds.count(pattern(G::Named(&iri("g")), None, None, None))
            .unwrap(),
        1
    );
    assert_eq!(
        ds.prefixes().get("ex").map(String::as_str),
        Some("http://ex.org/")
    );
    assert!(ds.remove_prefix("ex".into()).unwrap());
    let caps = ds.capabilities();
    assert!(caps.functions.iter().any(|f| f.ends_with("#localname")));
    assert_eq!(ffi_version().encoding_version, encode::ENCODING_VERSION);
}

#[test]
fn named_snapshots_forks_history_and_streaming_dump() {
    let ds = FfiDataset::memory(opts(BlankNodeMode::Dataset));
    let w = ds.begin_write(None, true).unwrap().unwrap();
    let mut bytes = Vec::new();
    op(
        &mut bytes,
        true,
        None,
        &iri("s"),
        &iri("p"),
        &Literal::from(1).into(),
    );
    w.apply(bytes).unwrap();
    let first = w.commit().unwrap().commit.seq;
    ds.snapshots_create("before".into(), "head".into(), None, None, false)
        .unwrap();
    let w = ds.begin_write(None, true).unwrap().unwrap();
    let mut bytes = Vec::new();
    op(
        &mut bytes,
        true,
        None,
        &iri("s"),
        &iri("p"),
        &Literal::from(2).into(),
    );
    w.apply(bytes).unwrap();
    w.commit().unwrap();
    let historical = ds.begin_read_at("snapshot:before".into()).unwrap();
    assert_eq!(historical.commit_seq(), first);
    assert_eq!(
        historical
            .fork()
            .count(pattern(G::Any, None, None, None))
            .unwrap(),
        1
    );
    assert_eq!(ds.commits("latest".into(), None, 10).unwrap().len(), 3);
    let dump = historical
        .dump_cursor("application/n-quads".into())
        .unwrap();
    let control = FfiOperation::new(None);
    let mut output = Vec::new();
    loop {
        let b = dump.next_chunk(1, control.clone()).unwrap();
        output.extend(b.batch);
        if b.done {
            break;
        }
    }
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("1"));
    assert!(!output.contains("\"2\""));
    control.cancel();
    assert!(ds.compact(control).is_err());
}

#[test]
fn select_cursor_batches_restart_and_pin_the_prepared_snapshot() {
    let ds = FfiDataset::memory(opts(BlankNodeMode::Dataset));
    let data = (0..100)
        .map(|i| format!("<http://ex.org/s{i}> <http://ex.org/p> {i} .\n"))
        .collect::<String>();
    ds.load_bytes(data.into_bytes(), "text/turtle".into(), None, None)
        .unwrap();
    let query = ds
        .prepare_query("SELECT ?s ?o { ?s <http://ex.org/p> ?o }".into(), qopts())
        .unwrap();
    let cursor = query.open_cursor(2, 1 << 20, false).unwrap();
    assert_eq!(cursor.variables(), vec!["s", "o"]);
    assert_eq!(cursor.status().unwrap(), "open");
    let statistics: serde_json::Value =
        serde_json::from_str(&cursor.stats_json().unwrap()).unwrap();
    assert_eq!(statistics["stats"]["rowsProduced"], 0);
    assert!(serde_json::from_str::<serde_json::Value>(&cursor.plan_json().unwrap()).is_ok());
    ds.load_bytes(
        b"<http://ex.org/late> <http://ex.org/p> 101 .".to_vec(),
        "text/turtle".into(),
        None,
        None,
    )
    .unwrap();
    let mut dictionary = Vec::new();
    let mut got = Vec::new();
    loop {
        let batch = cursor.next_batch(3).unwrap();
        assert_ne!(batch.batch[1] & encode::FLAG_RESTART, 0);
        let decoded = rows(&batch.batch, &mut dictionary);
        assert!(decoded.len() <= 3);
        assert!(dictionary.len() <= 6);
        got.extend(decoded);
        if batch.done {
            break;
        }
    }
    assert_eq!(got.len(), 100);
    assert_eq!(cursor.status().unwrap(), "complete");
    cursor.release();
    assert_eq!(cursor.status().unwrap(), "complete");
    assert!(cursor.next_batch(3).unwrap().done);
}

#[test]
fn select_cursor_cancel_is_fused_and_transactions_reject_cursor_escape() {
    let ds = FfiDataset::memory(opts(BlankNodeMode::Dataset));
    let query = ds
        .prepare_query("SELECT ?x { VALUES ?x { 1 2 3 } }".into(), qopts())
        .unwrap();
    let cursor = query.open_cursor(1, 1 << 20, false).unwrap();
    assert!(!cursor.next_batch(1).unwrap().done);
    cursor.cancel();
    assert_eq!(
        cursor.next_batch(1).err().unwrap().kind(),
        ErrorKind::Cancelled
    );
    assert_eq!(cursor.status().unwrap(), "failed");
    assert!(cursor.next_batch(1).unwrap().done);
    let tx = ds.begin_write(None, true).unwrap().unwrap();
    let query = tx
        .prepare_query("SELECT ?x { VALUES ?x { 1 } }".into(), qopts())
        .unwrap();
    assert_eq!(
        query.open_cursor(1, 1 << 20, false).err().unwrap().kind(),
        ErrorKind::Invalid
    );
    assert!(query.execute(1).unwrap().done);
    tx.abort();
}

#[test]
fn select_cursor_pending_batch_is_open_until_delivered_or_closed() {
    let ds = FfiDataset::memory(opts(BlankNodeMode::Dataset));
    let q = ds
        .prepare_query("SELECT ?x { VALUES ?x { 1 2 3 } }".into(), qopts())
        .unwrap();
    let cursor = q.open_cursor(4096, 1 << 20, false).unwrap();
    assert!(!cursor.next_batch(1).unwrap().done);
    assert_eq!(cursor.status().unwrap(), "open");
    cursor.release();
    assert_eq!(cursor.status().unwrap(), "stopped");
    assert!(cursor.next_batch(1).unwrap().done);
}
