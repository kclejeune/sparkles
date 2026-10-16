//! A warm repeat of a query asks the kernel for nothing (spec X07 §9, gate 4). The I/O
//! counters are process-wide, so this file holds a single test.

use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::ioacct::IoSnapshot;
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};

const EX: &str = "http://example.org/";

/// The shape of scripts/gen-data.py that the employee-docs query reads.
fn data(n: usize) -> String {
    let mut s = String::new();
    let orgs = (n / 50).max(10);
    let docs = n / 2;
    let mut r = 0x9e3779b97f4a7c15u64;
    let mut rnd = |m: usize| {
        r ^= r << 13;
        r ^= r >> 7;
        r ^= r << 17;
        (r % m as u64) as usize
    };
    let int = "http://www.w3.org/2001/XMLSchema#integer";
    for i in 0..n {
        s += &format!("<{EX}person/{i}> <{EX}name> \"p{i}\" .\n");
        s += &format!(
            "<{EX}person/{i}> <{EX}age> \"{}\"^^<{int}> .\n",
            18 + rnd(60)
        );
        if rnd(10) < 7 {
            s += &format!(
                "<{EX}person/{i}> <{EX}worksFor> <{EX}org/{}> .\n",
                rnd(orgs)
            );
        }
        for _ in 0..rnd(6) {
            s += &format!("<{EX}person/{i}> <{EX}knows> <{EX}person/{}> .\n", rnd(n));
        }
        if rnd(10) < 4 {
            s += &format!(
                "<{EX}person/{i}> <{EX}authorOf> <{EX}doc/{}> .\n",
                rnd(docs)
            );
        }
    }
    for d in 0..docs {
        s += &format!(
            "<{EX}doc/{d}> <{EX}title> \"On topic {} ({d})\"@en .\n",
            d % 997
        );
        s += &format!(
            "<{EX}doc/{d}> <{EX}year> \"{}\"^^<{int}> .\n",
            1950 + rnd(70)
        );
    }
    s
}

/// The I/O of the fourth run of `q`, after three that warm the cache.
fn warm_repeat(s: &Store, q: &str) -> IoSnapshot {
    let opts = QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    for _ in 0..3 {
        query(s.snapshot(), q, &opts).unwrap();
    }
    let before = IoSnapshot::now();
    query(s.snapshot(), q, &opts).unwrap();
    IoSnapshot::now().since(&before)
}

#[test]
fn warm_repeats_issue_no_read_hints() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
        s.load(&[Source::from_bytes(
            data(60_000).into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        s.compact().unwrap();
    }
    let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
    let hints = |io: &IoSnapshot| io.willneed_requests.iter().sum::<u64>();
    // a selective join: a small leaf scan and two index-join probes
    let join = format!(
        "SELECT ?p ?d ?t WHERE {{ ?p <{EX}worksFor> <{EX}org/7> . ?p <{EX}authorOf> ?d . ?d <{EX}title> ?t }}"
    );
    let io = warm_repeat(&s, &join);
    assert_eq!(io.columns_decoded(), 0, "join decoded columns: {io:?}");
    assert_eq!(hints(&io), 0, "join issued WILLNEED: {io:?}");
    // a scan of a predicate whose middle blocks hold only that predicate: the reader
    // decodes only the scanned columns of those blocks, never the bound P column
    let scan = format!("SELECT ?s ?o WHERE {{ ?s <{EX}knows> ?o }}");
    let io = warm_repeat(&s, &scan);
    assert_eq!(hints(&io), 0, "predicate scan issued WILLNEED: {io:?}");
    let range = format!(
        "SELECT ?p ?a WHERE {{ ?p <{EX}age> ?a FILTER(?a > 70) }} ORDER BY DESC(?a) LIMIT 10"
    );
    let io = warm_repeat(&s, &range);
    assert_eq!(hints(&io), 0, "range top-k issued WILLNEED: {io:?}");
    let limited = format!("SELECT ?s ?o WHERE {{ ?s <{EX}knows> ?o }} LIMIT 1");
    let io = warm_repeat(&s, &limited);
    assert_eq!(hints(&io), 0, "limited scan issued WILLNEED: {io:?}");
}
