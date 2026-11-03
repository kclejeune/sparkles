//! The latency, memory and maintenance cost of the ontology entity search (item 616).
//!
//! ```text
//! cargo run --release -p sparkles --features ontology --example entity_search_bench -- \
//!     chebi_lite.owl [--per-bucket 60] [--rounds 5] [--seed 1] [--commits 50]
//! ```
//!
//! The ontology is loaded into a persistent dataset in a temporary directory. The first
//! search builds the index, and the run reports the build time, the index's own estimate
//! of its memory and the change in resident memory. The queries are prefixes of 1 to 12
//! characters of labels and synonyms drawn with a seeded generator, each with no typo, one
//! typo (from 4 characters) or two typos (from 8), each typo a substitution, deletion,
//! insertion or transposition at a random position. Each query runs through
//! `wire::call(ds, "search", …)`, the call the HTTP route and the bindings make, so the
//! JSON answer counts. After a warm-up pass every query runs `--rounds` times, and the
//! run reports the percentiles of the latencies by length and typo count, and how often
//! the entity the query came from is among the first ten hits. Then `--commits` small
//! commits each add a labelled class or remove a label, and the search after each one,
//! which applies the commit's changes to the index, is timed. The report is JSON on
//! stdout.

use serde_json::{Value as J, json};
use sparkles::Dataset;
use sparkles::handles::ontology::wire;
use std::collections::BTreeMap;
use std::time::Instant;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

/// One typo at a random position of `s`.
fn typo(s: &[char], rng: &mut Rng) -> Vec<char> {
    let mut v = s.to_vec();
    let letters: Vec<char> = "abcdefghijklmnopqrstuvwxyz".chars().collect();
    let i = rng.below(v.len());
    match rng.below(4) {
        0 => v[i] = letters[rng.below(26)],
        1 if v.len() > 1 => {
            v.remove(i);
        }
        2 => v.insert(i, letters[rng.below(26)]),
        _ if i + 1 < v.len() => v.swap(i, i + 1),
        _ => v[i] = letters[rng.below(26)],
    }
    v
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let k = ((p / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[k.min(sorted.len() - 1)]
}

fn summary(v: &mut [f64]) -> J {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    json!({
        "n": v.len(),
        "p50_ms": percentile(v, 50.0),
        "p95_ms": percentile(v, 95.0),
        "p99_ms": percentile(v, 99.0),
        "max_ms": v.last().copied().unwrap_or(0.0),
    })
}

fn search(ds: &Dataset, q: &str) -> J {
    wire::call(ds, "search", &json!({ "q": q }), &Default::default()).expect("search")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut file = None;
    let mut per_bucket = 60usize;
    let mut rounds = 5usize;
    let mut seed = 1u64;
    let mut commits = 50usize;
    let mut only: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let v = |i: usize| args.get(i + 1).cloned().expect("a value");
        match args[i].as_str() {
            "--per-bucket" => {
                per_bucket = v(i).parse().unwrap();
                i += 1;
            }
            "--rounds" => {
                rounds = v(i).parse().unwrap();
                i += 1;
            }
            "--seed" => {
                seed = v(i).parse().unwrap();
                i += 1;
            }
            "--query" => {
                only.push(v(i));
                i += 1;
            }
            "--commits" => {
                commits = v(i).parse().unwrap();
                i += 1;
            }
            f => file = Some(f.to_string()),
        }
        i += 1;
    }
    let file = file.expect("the ontology file");
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path().join("db")).unwrap();
    let t = Instant::now();
    let quads = ds.load_file(&file).unwrap();
    let load_s = t.elapsed().as_secs_f64();
    eprintln!("loaded {quads} quads in {load_s:.1} s");

    // the texts the queries come from: labels and exact synonyms of classes
    let mut texts: Vec<(String, String)> = Vec::new();
    let mut labels: Vec<(String, String)> = Vec::new();
    let sols = ds
        .select(
            "PREFIX owl: <http://www.w3.org/2002/07/owl#>
             PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             PREFIX oio: <http://www.geneontology.org/formats/oboInOwl#>
             SELECT ?s ?l ?syn WHERE { ?s a owl:Class .
               { ?s rdfs:label ?l } UNION { ?s oio:hasExactSynonym ?l BIND(true AS ?syn) } }",
        )
        .unwrap();
    for row in sols.iter() {
        let (Some(oxrdf::Term::NamedNode(s)), Some(oxrdf::Term::Literal(l))) =
            (row.get("s"), row.get("l"))
        else {
            continue;
        };
        texts.push((s.as_str().to_string(), l.value().to_string()));
        if row.get("syn").is_none() && l.language().is_none() {
            labels.push((s.as_str().to_string(), l.value().to_string()));
        }
    }
    texts.sort();
    texts.dedup();
    labels.sort();
    labels.dedup();
    eprintln!("{} label and synonym texts", texts.len());

    let rss_before = rss_kib();
    let t = Instant::now();
    let first = search(&ds, "a");
    let build_ms = t.elapsed().as_secs_f64() * 1000.0;
    let rss_after = rss_kib();
    let stats = ds.ontology().search_stats();
    eprintln!(
        "built the index in {build_ms:.0} ms: {} entities, {} values, {} words, ~{} MiB, RSS +{} MiB",
        stats.entities,
        stats.values,
        stats.words,
        stats.bytes >> 20,
        (rss_after.saturating_sub(rss_before)) >> 10
    );
    assert_eq!(first["index"], "built");
    if !only.is_empty() {
        // diagnosis: time the given queries and print their first hits
        for q in &only {
            for _ in 0..3 {
                let t = Instant::now();
                let j = search(&ds, q);
                eprintln!("{q:?}: {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
                if let Some(r) = j["results"].as_array() {
                    for h in r.iter().take(3) {
                        eprintln!("   {} {}", h["match"]["text"], h["match"]["tier"]);
                    }
                }
            }
        }
        return;
    }

    // the queries: (length, typos) buckets
    let mut rng = Rng(seed.max(1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut queries: Vec<(usize, usize, String, String)> = Vec::new();
    for len in 1..=12usize {
        for typos in 0..=2usize {
            if (typos == 1 && len < 4) || (typos == 2 && len < 8) {
                continue;
            }
            let mut made = 0;
            let mut tries = 0;
            while made < per_bucket && tries < per_bucket * 100 {
                tries += 1;
                let (iri, text) = &texts[rng.below(texts.len())];
                let chars: Vec<char> = text.chars().collect();
                if chars.len() < len {
                    continue;
                }
                let mut q: Vec<char> = chars[..len].to_vec();
                for _ in 0..typos {
                    q = typo(&q, &mut rng);
                }
                let q: String = q.into_iter().collect();
                if q.trim().is_empty() {
                    continue;
                }
                queries.push((len, typos, q, iri.clone()));
                made += 1;
            }
        }
    }
    eprintln!("{} queries", queries.len());
    for (_, _, q, _) in &queries {
        search(&ds, q);
    }
    let mut by_bucket: BTreeMap<(usize, usize), Vec<f64>> = BTreeMap::new();
    let mut by_typos: BTreeMap<usize, Vec<f64>> = BTreeMap::new();
    let mut all: Vec<f64> = Vec::new();
    let mut found: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
    let mut found_bucket: BTreeMap<(usize, usize), (usize, usize)> = BTreeMap::new();
    let mut slowest: Vec<(f64, String)> = Vec::new();
    for round in 0..rounds {
        for (len, typos, q, iri) in &queries {
            let t = Instant::now();
            let j = search(&ds, q);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            by_bucket.entry((*len, *typos)).or_default().push(ms);
            by_typos.entry(*typos).or_default().push(ms);
            all.push(ms);
            if round == 0 {
                let hit = j["results"]
                    .as_array()
                    .is_some_and(|r| r.iter().any(|h| h["iri"] == iri.as_str()));
                let e = found.entry(*typos).or_default();
                e.0 += hit as usize;
                e.1 += 1;
                let e = found_bucket.entry((*len, *typos)).or_default();
                e.0 += hit as usize;
                e.1 += 1;
            }
            slowest.push((ms, q.clone()));
        }
    }
    let buckets: Vec<J> = by_bucket
        .iter_mut()
        .map(|((len, typos), v)| {
            let mut s = summary(v);
            s["length"] = json!(len);
            s["typos"] = json!(typos);
            let (h, n) = found_bucket[&(*len, *typos)];
            s["source_in_top10"] = json!(h as f64 / n.max(1) as f64);
            s
        })
        .collect();
    let typo_rows: Vec<J> = by_typos
        .iter_mut()
        .map(|(typos, v)| {
            let mut s = summary(v);
            s["typos"] = json!(typos);
            let (h, n) = found[typos];
            s["source_in_top10"] = json!(h as f64 / n.max(1) as f64);
            s
        })
        .collect();
    let overall = summary(&mut all);
    slowest.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    slowest.dedup_by(|a, b| a.1 == b.1);
    slowest.truncate(20);
    eprintln!("overall {overall}");

    // commits, each followed by a search that applies its changes
    let mut after_commit: Vec<f64> = Vec::new();
    let mut commit_ms: Vec<f64> = Vec::new();
    for k in 0..commits {
        let u = if k % 2 == 0 {
            format!(
                "PREFIX owl: <http://www.w3.org/2002/07/owl#>
                 PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
                 INSERT DATA {{ <http://example.org/bench/C{k}> a owl:Class ;
                   rdfs:label \"benchmark class {k}\" ; rdfs:label \"zzbench {k}\"@en }}"
            )
        } else {
            let (iri, text) = &labels[rng.below(labels.len())];
            format!(
                "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
                 DELETE DATA {{ <{iri}> rdfs:label {} }}",
                serde_json::to_string(text).unwrap()
            )
        };
        let t = Instant::now();
        ds.update(&u).unwrap();
        commit_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        let j = search(&ds, "zzbench");
        after_commit.push(t.elapsed().as_secs_f64() * 1000.0);
        assert_ne!(j["index"], "built", "commit {k} rebuilt the index");
    }
    let j = search(&ds, "zzbench");
    let expected = commits.div_ceil(2).min(10);
    assert_eq!(
        j["results"].as_array().map(Vec::len),
        Some(expected),
        "the added classes are found"
    );
    let stats_after = ds.ontology().search_stats();
    let report = json!({
        "file": file,
        "quads": quads,
        "load_s": load_s,
        "texts": texts.len(),
        "index": {
            "build_ms": build_ms,
            "entities": stats.entities,
            "values": stats.values,
            "texts": stats.texts,
            "words": stats.words,
            "estimated_bytes": stats.bytes,
            "rss_before_kib": rss_before,
            "rss_after_kib": rss_after,
            "rss_delta_kib": rss_after.saturating_sub(rss_before),
        },
        "settings": { "per_bucket": per_bucket, "rounds": rounds, "seed": seed, "commits": commits },
        "queries": queries.len(),
        "overall": overall,
        "by_typos": typo_rows,
        "by_bucket": buckets,
        "slowest": slowest.iter().map(|(ms, q)| json!({"ms": ms, "q": q})).collect::<Vec<_>>(),
        "maintenance": {
            "commit": summary(&mut commit_ms),
            "search_after_commit": summary(&mut after_commit),
            "updates": stats_after.updates,
            "builds": stats_after.builds,
        },
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
