//! FILTER selectivities measured on samples: close to the share of rows a conjunct keeps,
//! shown by EXPLAIN, aware of the delta, and never changing an answer.

use super::*;
use crate::io::{RdfFormat, Source};
use crate::store::{Store, StoreOptions};

const PREFIXES: &str = "PREFIX ex: <http://ex.org/> ";

fn off() -> Optimizations {
    Optimizations {
        sampled_filters: false,
        ..Optimizations::ALL
    }
}

fn opts(opt: Optimizations) -> QueryOptions {
    QueryOptions {
        optimizations: Some(opt),
        no_cache: true,
        ..Default::default()
    }
}

fn load(ttl: String) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        ttl.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

/// `n` subjects with a name `"a<i mod 20>-<i>"`, an age `i mod 60`, and a link to
/// another subject.
fn people(n: usize) -> String {
    let mut t = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..n {
        t.push_str(&format!(
            "ex:s{i} ex:name \"a{}-{i}\" ; ex:age {} ; ex:knows ex:s{} .\n",
            i % 20,
            i % 60,
            (i * 7 + 3) % n
        ));
    }
    t
}

/// The FILTER nodes of a plan: their description and estimated rows over their input's.
fn filters(p: &PlanInfo, out: &mut Vec<(String, f64)>) {
    if p.operator == "Filter" {
        let below = p.children[0].estimated_rows.max(1.0);
        out.push((p.description.clone(), p.estimated_rows / below));
    }
    for c in &p.children {
        filters(c, out);
    }
}

fn explain_filters(
    snap: &Arc<crate::store::Snapshot>,
    q: &str,
    opt: Optimizations,
) -> Vec<(String, f64)> {
    let (_, info) = explain(snap.clone(), &format!("{PREFIXES}{q}"), &opts(opt))
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    let mut out = Vec::new();
    filters(&info, &mut out);
    out
}

fn answer(snap: &Arc<crate::store::Snapshot>, q: &str, opt: Optimizations) -> Vec<String> {
    let r = query(snap.clone(), &format!("{PREFIXES}{q}"), &opts(opt))
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    let mut rows: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| t.map_or("UNDEF".to_string(), |t| t.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    rows.sort();
    rows
}

#[test]
fn sampled_selectivity_is_close_to_the_share_kept() {
    let s = load(people(3000));
    let snap = s.snapshot();
    // a name of every twentieth subject contains "a3-"; ages below 6 are a tenth
    for (q, share) in [
        (
            "SELECT * WHERE { ?s ex:name ?n ; ex:knows ?k FILTER(CONTAINS(?n, \"a3-\")) }",
            0.05,
        ),
        (
            "SELECT * WHERE { ?s ex:age ?a ; ex:knows ?k FILTER(ABS(?a - 2) < 3) }",
            5.0 / 60.0,
        ),
        (
            "SELECT * WHERE { ?s ex:name ?n ; ex:knows ?k FILTER(STRLEN(?n) > 1) }",
            1.0,
        ),
    ] {
        let fs = explain_filters(&snap, q, Optimizations::ALL);
        assert_eq!(fs.len(), 1, "{q}: {fs:?}");
        let (desc, sel) = &fs[0];
        assert!(desc.contains("sampled rows]"), "{q}: {desc}");
        assert!(
            *sel > share * 0.5 && *sel < (share * 1.5).min(1.0) + 1e-9,
            "{q}: selectivity {sel} for a share of {share} ({desc})"
        );
        // switched off: the fixed factor, and no note
        let fs = explain_filters(&snap, q, off());
        assert!(!fs[0].0.contains("sampled"), "{q}: {}", fs[0].0);
        assert!((fs[0].1 - 0.3).abs() < 0.01, "{q}: {}", fs[0].1);
    }
}

/// A pattern in more blocks than a sample decodes is sampled per block, from the
/// metadata's first and last keys and a few decoded blocks.
#[test]
fn large_patterns_are_sampled_per_block() {
    let n = 3 * crate::index::BLOCK_ROWS + 1000;
    let mut t = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..n {
        t.push_str(&format!("ex:s{i} ex:name \"a{}-{i}\" ; a ex:C .\n", i % 25));
    }
    let s = load(t);
    let snap = s.snapshot();
    let q = "SELECT * WHERE { ?s ex:name ?n FILTER(CONTAINS(?n, \"a7-\")) }";
    let fs = explain_filters(&snap, q, Optimizations::ALL);
    let (desc, sel) = &fs[0];
    assert!(desc.contains("sampled rows]"), "{desc}");
    assert!(
        *sel > 0.02 && *sel < 0.07,
        "selectivity {sel} for 0.04 ({desc})"
    );
    // a pattern sorted on the filtered variable counts each block by its rows
    let q = "SELECT * WHERE { ?s a ex:C FILTER(STRSTARTS(STR(?s), \"http://ex.org/s1\")) }";
    let fs = explain_filters(&snap, q, Optimizations::ALL);
    let (desc, sel) = &fs[0];
    // s1, s10 to s19, s100 to s199, and so on up to s10000 to s19999
    let share = 11_111.0 / n as f64;
    assert!(
        *sel > share * 0.5 && *sel < share * 1.5,
        "selectivity {sel} for {share} ({desc})"
    );
}

/// Rows of the delta take part in the sample of a small pattern.
#[test]
fn samples_see_the_delta() {
    let s = load(people(500));
    let mut ins = String::from("PREFIX ex: <http://ex.org/> INSERT DATA {");
    for i in 0..500 {
        ins.push_str(&format!(" ex:t{i} ex:name \"zz{i}\" ; ex:knows ex:s{i} ."));
    }
    ins.push('}');
    update::update(&s, &ins, &QueryOptions::default()).unwrap();
    let snap = s.snapshot();
    let q = "SELECT * WHERE { ?s ex:name ?n ; ex:knows ?k FILTER(STRSTARTS(?n, \"zz\")) }";
    let fs = explain_filters(&snap, q, Optimizations::ALL);
    let (desc, sel) = &fs[0];
    assert!(
        *sel > 0.35 && *sel < 0.65,
        "selectivity {sel} for 0.5 ({desc})"
    );
}

/// Random filters over random joins: the same solutions with the sampled estimates on
/// and off (a debug build also checks that the join ordering's summaries match the
/// plans built from them).
#[test]
fn sampled_estimates_never_change_answers() {
    let s = load(people(400));
    let snap = s.snapshot();
    let filters = [
        "CONTAINS(?n, \"a1\")",
        "STRSTARTS(?n, \"a1-\")",
        "REGEX(?n, \"^a[23]-\")",
        "?a > 30",
        "ABS(?a - 30) < 5",
        "?a != 7",
        "?s != ?k",
        "STRLEN(STR(?k)) > 10",
        "LANGMATCHES(LANG(?n), \"en\")",
        "?a * 2 > ?a + 40",
        "CONTAINS(STR(?s), \"s1\") && ?a < 40",
    ];
    let shapes = [
        "?s ex:name ?n ; ex:age ?a ; ex:knows ?k",
        "?s ex:name ?n . ?k ex:age ?a . ?s ex:knows ?k",
        "?s ex:knows ?k . ?k ex:name ?n ; ex:age ?a",
        "?s ex:name ?n OPTIONAL { ?s ex:age ?a } ?s ex:knows ?k",
        "{ ?s ex:name ?n ; ex:knows ?k } UNION { ?s ex:age ?a ; ex:knows ?k }",
        "?s ex:knows ?k ; ex:age ?a FILTER EXISTS { ?k ex:name ?n }",
    ];
    for shape in shapes {
        for (i, f) in filters.iter().enumerate() {
            let q = format!("SELECT * WHERE {{ {shape} FILTER({f}) }}");
            assert_eq!(
                answer(&snap, &q, Optimizations::ALL),
                answer(&snap, &q, off()),
                "{q}"
            );
            // two filters at once
            let g = filters[(i * 5 + 3) % filters.len()];
            let q = format!("SELECT * WHERE {{ {shape} FILTER({f}) FILTER({g}) }}");
            assert_eq!(
                answer(&snap, &q, Optimizations::ALL),
                answer(&snap, &q, off()),
                "{q}"
            );
        }
    }
}
