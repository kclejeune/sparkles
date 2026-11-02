//! Classifies an ontology file with the OWL 2 EL classifier, for the comparison with a
//! reference reasoner and the measurements of `docs/benchmarks/el-classification`.
//!
//! ```text
//! el_classify [--threads N] [--ofn-out FILE] [--taxonomy FILE] [--repeat N] INPUT
//! ```
//!
//! INPUT is functional-style syntax (`.ofn`, or any file that starts with `Prefix(` or
//! `Ontology(`), RDF/XML (`.owl`, `.rdf`), Turtle (`.ttl`) or N-Triples (`.nt`).
//! `--ofn-out` writes the axioms read as functional-style syntax, which ELK reads, so
//! that both reasoners classify the same axioms. `--taxonomy` writes the taxonomy as
//! lines `EQ c1 c2 …` for each node of several classes and `SUB c d` for each direct
//! subsumption between the first classes of two nodes, sorted. A JSON line on standard
//! output gives the counts, the times of each step and the peak resident memory.

use oxrdf::Triple;
use oxrdfio::{RdfFormat, RdfParser};
use sparkles_owl::classify::{Classifier, Gap, Limits};
use sparkles_owl::{Axiom, ofn, ofn_read, read};
use std::io::Write;
use std::time::Instant;

fn peak_rss_kib() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines()
        .find(|l| l.starts_with("VmHWM:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut threads = 0usize;
    let mut ofn_out = None;
    let mut tax_out = None;
    let mut repeat = 1usize;
    let mut input = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--threads" => threads = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--ofn-out" => ofn_out = args.next(),
            "--taxonomy" => tax_out = args.next(),
            "--repeat" => repeat = args.next().and_then(|v| v.parse().ok()).unwrap_or(1),
            _ => input = Some(a),
        }
    }
    let Some(input) = input else {
        eprintln!("usage: el_classify [--threads N] [--ofn-out FILE] [--taxonomy FILE] INPUT");
        std::process::exit(2);
    };
    let t = Instant::now();
    let bytes = std::fs::read(&input).expect("read input");
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_string();
    let functional = input.ends_with(".ofn")
        || head.trim_start().starts_with("Prefix(")
        || head.trim_start().starts_with("Ontology(");
    let (axioms, header): (Vec<Axiom>, sparkles_owl::Header) = if functional {
        let text = String::from_utf8(bytes).expect("UTF-8");
        let doc = ofn_read::parse(&text).unwrap_or_else(|e| panic!("{input}: {e}"));
        (doc.axioms, doc.header)
    } else {
        let format = if input.ends_with(".ttl") {
            RdfFormat::Turtle
        } else if input.ends_with(".nt") {
            RdfFormat::NTriples
        } else {
            RdfFormat::RdfXml
        };
        let mut triples = Vec::new();
        for q in RdfParser::from_format(format).lenient().for_slice(&bytes) {
            let q = q.unwrap_or_else(|e| panic!("{input}: {e}"));
            triples.push(Triple::new(q.subject, q.predicate, q.object));
        }
        drop(bytes);
        let o = read::read_graph(triples, read::ReadOptions::default());
        let h = o.header.clone();
        (o.axioms.into_keys().collect(), h)
    };
    let read_ms = t.elapsed().as_secs_f64() * 1000.0;
    if let Some(path) = &ofn_out {
        let r = ofn::Renderer::default();
        let logical: Vec<&Axiom> = axioms
            .iter()
            .filter(|a| a.axiom_type() != sparkles_owl::AxiomType::AnnotationAssertion)
            .collect();
        std::fs::write(path, ofn::document(&r, &header, logical.iter().copied()))
            .expect("write OFN");
    }
    let rss_read = peak_rss_kib();
    let limits = Limits {
        threads,
        ..Limits::default()
    };
    let mut times = Vec::new();
    let mut out = None;
    let mut k = None;
    for _ in 0..repeat {
        let t = Instant::now();
        let mut c = Classifier::of(&axioms);
        let o = c.classify(&limits).expect("classify");
        times.push(t.elapsed().as_secs_f64() * 1000.0);
        out = Some(o);
        k = Some(c);
    }
    let out = out.expect("one run");
    let k = k.expect("one run");
    let excluded = k.excluded();
    let outside = excluded
        .iter()
        .filter(|e| matches!(e.gap, Gap::Outside { .. }))
        .count();
    let mut partial: std::collections::BTreeMap<&str, usize> = Default::default();
    for e in &excluded {
        if let Gap::Partial(why) = e.gap {
            *partial.entry(why).or_default() += 1;
        }
    }
    if let Some(path) = &tax_out {
        let t = &out.taxonomy;
        let mut lines: Vec<String> = Vec::new();
        for n in t.nodes() {
            if n.classes.len() > 1 {
                let names: Vec<&str> = n.classes.iter().map(|c| c.as_str()).collect();
                lines.push(format!("EQ {}", names.join(" ")));
            }
            if let Some(first) = n.classes.first() {
                for &p in &n.parents {
                    lines.push(format!(
                        "SUB {} {}",
                        first.as_str(),
                        t.node(p).classes[0].as_str()
                    ));
                }
            }
        }
        lines.sort();
        let mut f = std::fs::File::create(path).expect("taxonomy file");
        for l in lines {
            writeln!(f, "{l}").expect("write taxonomy");
        }
    }
    let s = &out.stats;
    let median = {
        let mut v = times.clone();
        v.sort_by(|a, b| a.total_cmp(b));
        v[v.len() / 2]
    };
    println!(
        "{}",
        serde_json::json!({
            "input": input,
            "axioms": axioms.len(),
            "classes": out.taxonomy.classes().count(),
            "nodes": out.taxonomy.nodes().len(),
            "unsatisfiable": out.taxonomy.unsatisfiable().count(),
            "consistent": out.consistent,
            "outside": outside,
            "partial": partial,
            "threads": s.threads,
            "readMs": read_ms,
            "classifyMs": times,
            "classifyMedianMs": median,
            "indexMs": s.index.as_secs_f64() * 1000.0,
            "saturationMs": s.saturation.as_secs_f64() * 1000.0,
            "taxonomyMs": s.taxonomy.as_secs_f64() * 1000.0,
            "contexts": s.contexts,
            "conclusions": s.conclusions,
            "estimatedBytes": s.memory,
            "peakRssKibAfterRead": rss_read,
            "peakRssKib": peak_rss_kib(),
        })
    );
}
