//! Hash joins that partition their inputs to disk when the build input outgrows memory,
//! and merge joins that spool long runs of equal keys (X08 §6.3), checked against eager
//! execution and in-memory cursors.
use oxrdf::Term;
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{
    CursorOptions, CursorStatus, FallbackPolicy, QueryOptions, query, select_cursor,
};
use sparkles_core::spill::{Fault, Spill, SpillOptions};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::{BudgetKind, Error};
use std::sync::Arc;

/// `n` subjects `a` and `n` subjects `b`. Each `a` has a value `v` and, unless it is one
/// of every twenty, a key `p`. Each `b` has a value `w`, a label `l` and, unless it is
/// one of every twenty, a key `q`. With `heavy`, all keys are bound and exactly nine
/// keys in ten are one key on both sides; otherwise each key has about three subjects
/// on each side.
fn store(n: usize, heavy: bool) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let key = |i: usize| {
        if heavy {
            if i.is_multiple_of(10) { i + 1 } else { 0 }
        } else {
            (i * 7919) % (n / 3).max(1)
        }
    };
    let mut data = String::new();
    for i in 0..n {
        let a = (i * 104_729) % n;
        data.push_str(&format!("<urn:a:{a}> <urn:v> {} .\n", a % 13));
        if heavy || !a.is_multiple_of(20) {
            data.push_str(&format!("<urn:a:{a}> <urn:p> <urn:k:{}> .\n", key(a)));
        }
        let b = (i * 15_485_863) % n;
        data.push_str(&format!("<urn:b:{b}> <urn:w> {} .\n", b % 11));
        data.push_str(&format!("<urn:b:{b}> <urn:l> \"label {b}\" .\n"));
        for x in 1..=3 {
            data.push_str(&format!("<urn:b:{b}> <urn:x{x}> {} .\n", b * x));
        }
        if heavy || b % 20 != 3 {
            data.push_str(&format!("<urn:b:{b}> <urn:q> <urn:k:{}> .\n", key(b * 7)));
        }
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn spill_with(dir: &std::path::Path, encrypted: bool) -> Arc<Spill> {
    Spill::open(&SpillOptions {
        dir: dir.to_path_buf(),
        max_bytes: 4 << 30,
        query_bytes: 4 << 30,
        min_free_bytes: 0,
        encrypted,
    })
    .unwrap()
}

fn spill(dir: &std::path::Path) -> Arc<Spill> {
    spill_with(dir, false)
}

struct Run {
    rows: Vec<String>,
    /// The first column of each row, which holds the probe input's order.
    firsts: Vec<String>,
    plan: String,
    peak: u64,
}

/// The rows of `q` in the cursor's order, its plan and its memory peak.
fn run(
    s: &Store,
    q: &str,
    memory: u64,
    spill: Option<Arc<Spill>>,
    batch_rows: usize,
) -> Result<Run, Error> {
    let opts = QueryOptions {
        max_memory_bytes: Some(memory),
        spill,
        ..Default::default()
    };
    let cursor = CursorOptions {
        batch_rows,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    let mut c = select_cursor(s.snapshot(), q, &opts, &cursor)?;
    let mut rows = Vec::new();
    let mut firsts = Vec::new();
    while let Some(b) = c.next_batch()? {
        assert!(b.len() <= batch_rows);
        for i in 0..b.len() {
            let row = b.row(i).unwrap();
            firsts.push(format!("{:?}", row[0]));
            rows.push(format!("{row:?}"));
        }
    }
    assert_eq!(c.status(), CursorStatus::Complete);
    Ok(Run {
        rows,
        firsts,
        plan: c.plan_json()?,
        peak: c.stats().mem_peak_bytes,
    })
}

fn sorted(mut rows: Vec<String>) -> Vec<String> {
    rows.sort();
    rows
}

fn eager(s: &Store, q: &str) -> Vec<String> {
    sorted(
        query(s.snapshot(), q, &Default::default())
            .unwrap()
            .rows()
            .into_iter()
            .map(|r: Vec<Option<Term>>| format!("{r:?}"))
            .collect(),
    )
}

/// The names in `dir`'s process directories other than their markers.
fn leftovers(dir: &std::path::Path) -> Vec<String> {
    let mut names = Vec::new();
    for process in std::fs::read_dir(dir).unwrap() {
        let process = process.unwrap().path();
        if process.is_dir() {
            for f in std::fs::read_dir(&process).unwrap() {
                let name = f.unwrap().file_name().to_string_lossy().into_owned();
                if name != "SPARKLES-SPILL" {
                    names.push(name);
                }
            }
        }
    }
    names
}

/// A counter of the hash join in a plan.
fn counter(plan: &str, name: &str) -> Option<u64> {
    let at = plan.find(&format!("\"{name}\":"))? + name.len() + 3;
    plan[at..]
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

const MEMORY: u64 = 1 << 20;

/// The named/default-graph shape of W3C dawg-optional-complex-3 has two rows,
/// but speculative output buffers of nested merges used to exhaust 1 MiB.
#[test]
fn nested_optional_with_a_named_graph_fits_a_small_budget_at_large_batch_caps() {
    let s = Store::in_memory(StoreOptions::default());
    let data = r#"
        @prefix foaf: <http://xmlns.com/foaf/0.1/> .
        @prefix ex: <http://example.org/things#> .
        @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
        _:a a foaf:Person; foaf:name "Eve"; ex:empId "9"^^xsd:integer .
        _:b a foaf:Person; foaf:name "Alice"; ex:empId "29"^^xsd:integer;
            ex:healthplan ex:HealthPlanD .
        _:c a foaf:Person; foaf:name "Fred"; ex:empId "27"^^xsd:integer .
        _:e foaf:name "Bob"; ex:empId "23"^^xsd:integer; ex:healthplan ex:HealthPlanC .
        _:f foaf:name "Bob"; ex:empId "30"^^xsd:integer; ex:healthplan ex:HealthPlanB .
        _:g a foaf:Person; ex:ssn "000000000"; foaf:name "Bert";
            ex:department "DeptA"; ex:healthplan ex:HealthPlanA .
        <urn:contacts> {
            <tag:alice@example:foafUri> foaf:mbox <mailto:alice@example.net>;
                foaf:name "Alice"; foaf:nick "WhoMe?";
                foaf:depiction <http://example.com/alice.png> .
            <tag:bert@example:foafUri> foaf:mbox <mailto:bert@example.net>;
                foaf:nick "BigB"; foaf:name "Bert" .
            <tag:eve@example:foafUri> foaf:mbox <mailto:eve@example.net>;
                foaf:firstName "Eve" .
            <tag:john@example:foafUri> foaf:mbox <mailto:john@example.net>;
                foaf:nick "jDoe"; foaf:isPrimaryTopicOf <http://example.com/people/johnDoe> .
        }
    "#;
    s.load(&[Source::from_bytes(
        data.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    let q = "PREFIX foaf: <http://xmlns.com/foaf/0.1/> \
             PREFIX ex: <http://example.org/things#> \
             SELECT ?name ?nick ?plan ?dept { \
               ?person a foaf:Person; foaf:name ?name . \
               GRAPH ?x { [] foaf:name ?name; foaf:nick ?nick } \
               OPTIONAL { ?person ex:healthplan ?plan \
                          OPTIONAL { ?person ex:department ?dept } } }";
    let expected = eager(&s, q);
    assert_eq!(expected.len(), 2);
    for encrypted in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let spill = spill_with(tmp.path(), encrypted);
        for batch in [1, 7, 4096] {
            let result = run(&s, q, MEMORY, Some(spill.clone()), batch).unwrap();
            assert_eq!(
                sorted(result.rows),
                expected,
                "encrypted={encrypted}, batch={batch}"
            );
            assert!(result.peak <= MEMORY);
            assert_eq!(spill.reserved_bytes(), 0);
            assert!(leftovers(tmp.path()).is_empty());
        }
    }
}

/// Joins on `?k` whose inputs come in no order of the key, so the cursor joins them by
/// hash. The probe input of the first is ordered on `?a`, so a spilled join restores
/// that order; the UNION of the second has no order to keep.
const INNER: &str = "SELECT ?a ?v ?b ?w ?k { ?a <urn:v> ?v . ?a <urn:p> ?k . \
                     ?b <urn:q> ?k . ?b <urn:w> ?w . ?b <urn:l> ?l }";
/// The join of INNER under a grouping, which may use its input's order, so that a
/// spilled join keeps the probe input's order.
const INNER_ORDERED: &str = "SELECT ?a (COUNT(*) AS ?n) (SUM(?w) AS ?ws) { ?a <urn:v> ?v . \
                             ?a <urn:p> ?k . ?b <urn:q> ?k . ?b <urn:w> ?w . ?b <urn:l> ?l } \
                             GROUP BY ?a";
const INNER_UNORDERED: &str = "SELECT ?a ?v ?b ?k ?l { { ?a <urn:v> ?v . ?a <urn:p> ?k } \
                               UNION { ?a <urn:v> ?v . ?a <urn:p> ?k FILTER(?v = 3) } \
                               ?b <urn:q> ?k . ?b <urn:w> ?w . ?b <urn:l> ?l }";

#[test]
fn spilled_hash_joins_match_eager_at_every_batch_size() {
    let s = store(30_000, false);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    for q in [INNER_ORDERED, INNER, INNER_UNORDERED] {
        let expected = eager(&s, q);
        let memory = run(&s, q, 256 << 20, None, 4096).unwrap();
        assert_eq!(sorted(memory.rows.clone()), expected, "{q}");
        assert!(memory.plan.contains("HashJoin"), "{q}: {}", memory.plan);
        // A8: the build input is several times the budget.
        assert!(
            memory.peak > 4 * MEMORY,
            "{q}: an unlimited join peaks at {} bytes",
            memory.peak
        );
        match run(&s, q, MEMORY, None, 4096) {
            Err(Error::BudgetExceeded(b)) => assert_eq!(b.kind, BudgetKind::Memory, "{q}"),
            other => panic!(
                "{q}: expected a memory budget error, got {:?}",
                other.map(|r| r.rows.len())
            ),
        }
        for batch in [1, 7, 4096] {
            let spilled = run(&s, q, MEMORY, Some(spill.clone()), batch).unwrap();
            assert!(spilled.peak <= MEMORY, "{q}: peak {}", spilled.peak);
            assert!(
                counter(&spilled.plan, "spillPartitions").is_some_and(|p| p >= 8),
                "{q}: {}",
                spilled.plan
            );
            assert!(counter(&spilled.plan, "spillLevels").is_some(), "{q}");
            assert!(
                counter(&spilled.plan, "spilledBytes").is_some_and(|b| b > 0),
                "{q}"
            );
            if q == INNER_ORDERED {
                // The grouping may rely on the probe input's order, so the spilled join
                // merges its output back into that order. The unit tests of the join
                // check the order itself.
                assert!(spilled.plan.contains("spillMergePasses"), "{q}");
            } else {
                // Nothing relies on the order, so the join emits partition by partition.
                assert!(!spilled.plan.contains("spillMergePasses"), "{q}");
            }
            assert_eq!(sorted(spilled.rows), expected, "{q} at batch size {batch}");
            assert_eq!(spill.reserved_bytes(), 0, "{q}");
        }
    }
    assert!(leftovers(tmp.path()).is_empty());
}

/// A9: OPTIONAL with a condition and MINUS, where one build row and one probe row in
/// twenty leave the key unbound. Every probe row's unbound extension appears exactly when
/// none of its candidates passes.
#[test]
fn spilled_optional_and_minus_handle_unbound_keys() {
    let s = store(6_000, false);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    let left = "{ ?a <urn:v> ?v OPTIONAL { ?a <urn:p> ?k } }";
    let right = "?b <urn:w> ?w . ?b <urn:l> ?l . ?b <urn:x1> ?x1 . ?b <urn:x2> ?x2 . \
                 ?b <urn:x3> ?x3 OPTIONAL { ?b <urn:q> ?k }";
    for q in [
        format!(
            "SELECT ?a ?v ?k ?b ?w {{ {left} OPTIONAL {{ {right} FILTER(?w < 3 && ?v != 4) }} }}"
        ),
        format!("SELECT ?a ?v ?k {{ {left} MINUS {{ {right} FILTER(?w != 3) }} }}"),
    ] {
        let expected = eager(&s, &q);
        let memory = run(&s, &q, 1 << 30, None, 4096).unwrap();
        assert_eq!(sorted(memory.rows.clone()), expected, "{q}");
        for (budget, batch) in [(768 << 10, 1000), (1 << 20, 7)] {
            let spilled = match run(&s, &q, budget, Some(spill.clone()), batch) {
                Ok(r) => r,
                Err(e) => panic!("{q} under {budget}: {e}"),
            };
            assert_eq!(sorted(spilled.rows.clone()), expected, "{q} under {budget}");
            if spilled.plan.contains("spillPartitions") && spilled.plan.contains("spillMergePasses")
            {
                assert_eq!(spilled.firsts, memory.firsts, "{q}: the order changed");
            }
            assert_eq!(spill.reserved_bytes(), 0, "{q}");
        }
        let spilled = run(&s, &q, 768 << 10, Some(spill.clone()), 1000).unwrap();
        assert!(
            spilled.plan.contains("spillPartitions"),
            "{q}: {}",
            spilled.plan
        );
    }
    assert!(leftovers(tmp.path()).is_empty());
}

const HEAVY_MEMORY: u64 = 320 << 10;

/// A10: a key that holds nine tenths of both inputs joins as a block nested loop within
/// the budget.
#[test]
fn a_heavy_key_joins_as_a_block_nested_loop() {
    let s = store(3_000, true);
    for property in ["p", "q"] {
        let all = format!("SELECT ?s {{ ?s <urn:{property}> ?k }}");
        let heavy = format!("SELECT ?s {{ ?s <urn:{property}> <urn:k:0> }}");
        assert_eq!(
            query(s.snapshot(), &all, &Default::default())
                .unwrap()
                .len(),
            3_000
        );
        assert_eq!(
            query(s.snapshot(), &heavy, &Default::default())
                .unwrap()
                .len(),
            2_700
        );
    }
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    // Wide build rows, so that a few thousand of them outgrow the budget.
    let right = "?b <urn:q> ?k . ?b <urn:w> ?w . ?b <urn:l> ?l . ?b <urn:x1> ?x1 . \
                 ?b <urn:x2> ?x2 . ?b <urn:x3> ?x3";
    let q = format!("SELECT ?a ?b {{ ?a <urn:v> ?v . ?a <urn:p> ?k . {right} }}");
    let expected = eager(&s, &q);
    assert!(expected.len() > 4_000_000);
    let spilled = run(&s, &q, HEAVY_MEMORY, Some(spill.clone()), 100).unwrap();
    assert!(spilled.peak <= HEAVY_MEMORY, "peak {}", spilled.peak);
    assert_eq!(sorted(spilled.rows), expected);
    assert!(
        counter(&spilled.plan, "spillChunks").is_some_and(|c| c > 1),
        "{}",
        spilled.plan
    );
    for q in [
        format!("SELECT ?a ?k {{ ?a <urn:v> ?v . ?a <urn:p> ?k MINUS {{ {right} }} }}"),
        format!(
            "SELECT ?a ?k ?b {{ ?a <urn:v> ?v . ?a <urn:p> ?k OPTIONAL {{ {right} \
             FILTER(?w = 3 && ?v = 2) }} }}"
        ),
    ] {
        let q = q.as_str();
        let expected = eager(&s, q);
        let spilled = run(&s, q, HEAVY_MEMORY, Some(spill.clone()), 100).unwrap();
        assert!(
            counter(&spilled.plan, "spillChunks").is_some_and(|c| c > 1),
            "{q}: {}",
            spilled.plan
        );
        assert_eq!(sorted(spilled.rows), expected, "{q}");
    }
    assert_eq!(spill.reserved_bytes(), 0);
    assert!(leftovers(tmp.path()).is_empty());
}

#[test]
fn hash_joins_that_fit_never_spill() {
    let s = store(3_000, false);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    let spilled = run(&s, INNER, 256 << 20, Some(spill.clone()), 4096).unwrap();
    assert_eq!(sorted(spilled.rows), eager(&s, INNER));
    assert!(
        !spilled.plan.contains("spillPartitions"),
        "{}",
        spilled.plan
    );
    assert_eq!(spill.written_bytes(), 0);
}

#[test]
fn encrypted_spilled_joins_match_eager() {
    let s = store(30_000, false);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill_with(tmp.path(), true);
    for q in [INNER, INNER_UNORDERED] {
        let spilled = run(&s, q, MEMORY, Some(spill.clone()), 4096).unwrap();
        assert_eq!(sorted(spilled.rows), eager(&s, q), "{q}");
        assert!(spilled.plan.contains("spillPartitions"), "{q}");
    }
    assert!(spill.written_bytes() > 0);
    assert!(leftovers(tmp.path()).is_empty());
}

/// The contents of this process's open spill files under `dir`, read through
/// `/proc/self/fd` while a query holds them, as A14 reads them.
fn open_spill_files(dir: &std::path::Path) -> Vec<Vec<u8>> {
    let dir = dir.canonicalize().unwrap();
    let mut files = Vec::new();
    for fd in std::fs::read_dir("/proc/self/fd").unwrap().flatten() {
        let Ok(target) = std::fs::read_link(fd.path()) else {
            continue;
        };
        if target.starts_with(&dir)
            && !target.ends_with("SPARKLES-SPILL")
            && let Ok(bytes) = std::fs::read(fd.path())
            && bytes.starts_with(b"SPKSPL01")
        {
            files.push(bytes);
        }
    }
    files
}

/// A14: with encryption, no spill file holds a 16-byte stretch of the plain frames that
/// the same query writes without it, nor the bytes of a known inline value.
#[cfg(target_os = "linux")]
#[test]
fn encrypted_spill_files_hold_no_plain_bytes() {
    let s = store(30_000, false);
    let read = |encrypted: bool| {
        let tmp = tempfile::tempdir().unwrap();
        let spill = spill_with(tmp.path(), encrypted);
        let opts = QueryOptions {
            max_memory_bytes: Some(MEMORY),
            spill: Some(spill.clone()),
            ..Default::default()
        };
        let cursor = CursorOptions {
            batch_rows: 1000,
            fallback: FallbackPolicy::RejectMaterialization,
            ..Default::default()
        };
        let mut c = select_cursor(s.snapshot(), INNER_UNORDERED, &opts, &cursor).unwrap();
        // Both inputs are partitioned before the first output.
        assert!(c.next_batch().unwrap().is_some());
        let files = open_spill_files(tmp.path());
        drop(c);
        assert!(leftovers(tmp.path()).is_empty());
        files
    };
    let plain = read(false);
    let sealed = read(true);
    assert!(!plain.is_empty() && !sealed.is_empty());
    let sealed_bytes: Vec<u8> = sealed.concat();
    // Stretches of the plain frames, after the file header.
    let stretches: std::collections::HashSet<&[u8]> = plain
        .iter()
        .flat_map(|file| file[32..].as_chunks::<16>().0.iter().map(|c| &c[..]))
        .filter(|w| w.iter().any(|&b| b != 0))
        .collect();
    assert!(stretches.len() > 100, "{} stretches", stretches.len());
    assert!(
        !sealed_bytes.windows(16).any(|w| stretches.contains(w)),
        "a plain stretch in an encrypted spill file"
    );
    // The inline integers of the input, as ids.
    for value in [7i64, 12, 9973] {
        let id = sparkles_core::id::Id::from_i64(value)
            .unwrap()
            .0
            .to_le_bytes();
        assert!(!sealed_bytes.windows(8).any(|w| w == id), "{value}");
    }
}

/// Cancellation, a deadline, a refused reservation, a full disk and a damaged frame in
/// the middle of a spilled join each fail the query with the right error and leave no
/// file and no reservation behind.
#[test]
fn failures_in_a_spilled_join_leave_nothing_behind() {
    let s = store(30_000, false);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    let cursor = CursorOptions {
        batch_rows: 1000,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    // Cancelled after the first output.
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let opts = QueryOptions {
        max_memory_bytes: Some(MEMORY),
        spill: Some(spill.clone()),
        cancel: Some(cancel.clone()),
        ..Default::default()
    };
    let mut c = select_cursor(s.snapshot(), INNER_UNORDERED, &opts, &cursor).unwrap();
    assert!(c.next_batch().unwrap().is_some());
    assert!(spill.reserved_bytes() > 0);
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(matches!(c.next_batch(), Err(Error::Cancelled)));
    drop(c);
    assert_eq!(spill.reserved_bytes(), 0);
    assert!(leftovers(tmp.path()).is_empty());
    // Dropped early, before the output merge of an ordered join ends.
    let opts = QueryOptions {
        max_memory_bytes: Some(MEMORY),
        spill: Some(spill.clone()),
        ..Default::default()
    };
    let mut c = select_cursor(s.snapshot(), INNER, &opts, &cursor).unwrap();
    assert!(c.next_batch().unwrap().is_some());
    drop(c);
    assert_eq!(spill.reserved_bytes(), 0);
    assert!(leftovers(tmp.path()).is_empty());
    // A deadline while the partitions join.
    let opts = QueryOptions {
        max_memory_bytes: Some(MEMORY),
        spill: Some(spill.clone()),
        timeout: Some(std::time::Duration::from_millis(1)),
        ..Default::default()
    };
    let result = (|| -> Result<(), Error> {
        let mut c = select_cursor(s.snapshot(), INNER, &opts, &cursor)?;
        while c.next_batch()?.is_some() {}
        Ok(())
    })();
    assert!(matches!(result, Err(Error::Timeout)), "{result:?}");
    assert_eq!(spill.reserved_bytes(), 0);
    // The disk reservation of the query, a refused extent and a full disk.
    let drain = |opts: &QueryOptions| -> Result<(), Error> {
        let mut c = select_cursor(s.snapshot(), INNER, opts, &cursor)?;
        while c.next_batch()?.is_some() {}
        Ok(())
    };
    let small = QueryOptions {
        max_memory_bytes: Some(MEMORY),
        spill: Some(spill.clone()),
        max_spill_bytes: Some(64 << 10),
        ..Default::default()
    };
    let error = drain(&small).unwrap_err();
    assert!(
        matches!(&error, Error::BudgetExceeded(b) if b.kind == BudgetKind::Spill),
        "{error:?}"
    );
    let opts = QueryOptions {
        max_memory_bytes: Some(MEMORY),
        spill: Some(spill.clone()),
        ..Default::default()
    };
    for (fault, after) in [(Fault::Reserve, 1), (Fault::Write, 40)] {
        spill.inject_fault(fault, after);
        let error = drain(&opts).unwrap_err();
        assert!(
            matches!(&error, Error::BudgetExceeded(b) if b.kind == BudgetKind::Spill),
            "{fault:?}: {error:?}"
        );
        assert_eq!(spill.reserved_bytes(), 0, "{fault:?}");
    }
    spill.inject_fault(Fault::Read, 20);
    let error = drain(&opts).unwrap_err();
    assert!(matches!(&error, Error::Corrupt(_)), "{error:?}");
    assert_eq!(spill.reserved_bytes(), 0);
    assert!(leftovers(tmp.path()).is_empty());
}
