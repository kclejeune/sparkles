//! Classifies an ontology file with the OWL 2 EL classifier, for the comparison with a
//! reference reasoner and the measurements of `docs/benchmarks/el-classification`.
//!
//! ```text
//! el_classify [--threads N] [--counting] [--rooted] [--dl-work N] [--ofn-out FILE] [--taxonomy FILE] [--repeat N] [--heap] [--owned] INPUT
//! ```
//!
//! INPUT is functional-style syntax (`.ofn`, or any file that starts with `Prefix(` or
//! `Ontology(`), RDF/XML (`.owl`, `.rdf`), Turtle (`.ttl`) or N-Triples (`.nt`).
//! `--ofn-out` writes the axioms read as functional-style syntax, which ELK reads, so
//! that both reasoners classify the same axioms. `--taxonomy` writes the taxonomy as
//! lines `EQ c1 c2 …` for each nonempty node, including singletons and `SUB c d` for each direct
//! subsumption between the first classes of two nodes, sorted. A JSON line on standard
//! output gives the counts, the times of each step and the peak resident memory.
//! The classification reads the axioms in place, as the server's does from an open
//! ontology, and `--owned` gives them to a classifier that keeps its own copy instead.
//! `--heap` also counts the bytes allocated, which slows the run, and reports the most
//! that the last run's classification held at once beyond the axioms read, for the
//! comparison with the classifier's estimate.

#![recursion_limit = "256"]

use oxrdf::Triple;
use oxrdfio::{RdfFormat, RdfParser};
use sparkles_owl::classify::{Classifier, Gap, Limits, Saturation, logical};
use sparkles_owl::{Axiom, ofn, ofn_read, read};
use std::alloc::{GlobalAlloc, Layout};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::Instant;

// the server's allocator, so that the times and the resident memory are the server's,
// with a count of the bytes held that `--heap` turns on
#[global_allocator]
static GLOBAL: Counting = Counting;

struct Counting;
static COUNT: AtomicBool = AtomicBool::new(false);
static HELD: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);

fn count(n: isize) {
    if COUNT.load(Ordering::Relaxed) {
        let now = HELD.fetch_add(n, Ordering::Relaxed) + n;
        PEAK.fetch_max(now, Ordering::Relaxed);
    }
}

// SAFETY: every call goes to mimalloc unchanged; the count only reads the layouts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        count(l.size() as isize);
        unsafe { sparkles_alloc::Mimalloc.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        count(l.size() as isize);
        unsafe { sparkles_alloc::Mimalloc.alloc_zeroed(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        count(-(l.size() as isize));
        unsafe { sparkles_alloc::Mimalloc.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, size: usize) -> *mut u8 {
        count(size as isize - l.size() as isize);
        unsafe { sparkles_alloc::Mimalloc.realloc(p, l, size) }
    }
}

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
    let mut heap = false;
    let mut el_only = false;
    let mut owned = false;
    let mut eager = false;
    let mut dl_work: Option<usize> = None;
    let mut counting = false;
    let mut rooted = false;
    let mut input = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--threads" => threads = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--ofn-out" => ofn_out = args.next(),
            "--taxonomy" => tax_out = args.next(),
            "--repeat" => repeat = args.next().and_then(|v| v.parse().ok()).unwrap_or(1),
            "--heap" => heap = true,
            "--owned" => owned = true,
            "--el-only" => el_only = true,
            "--eager" => eager = true,
            "--counting" => counting = true,
            "--rooted" => rooted = true,
            "--dl-work" => dl_work = args.next().and_then(|v| v.parse().ok()),
            _ => input = Some(a),
        }
    }
    let Some(input) = input else {
        eprintln!(
            "usage: el_classify [--threads N] [--counting] [--rooted] [--dl-work N] [--el-only] [--eager] [--ofn-out FILE] [--taxonomy FILE] INPUT"
        );
        std::process::exit(2);
    };
    let t = Instant::now();
    // the SWRL rules the reader passes over, which the run leaves out
    let mut rules = 0usize;
    let bytes = std::fs::read(&input).expect("read input");
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_string();
    let functional = input.ends_with(".ofn")
        || head.trim_start().starts_with("Prefix(")
        || head.trim_start().starts_with("Ontology(");
    let (axioms, header): (Vec<Axiom>, sparkles_owl::Header) = if functional {
        let text = String::from_utf8(bytes).expect("UTF-8");
        let doc = ofn_read::parse_with(&text, ofn_read::Options { skip_rules: true })
            .unwrap_or_else(|e| panic!("{input}: {e}"));
        rules = doc.rules;
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
        el_only,
        expansion: if eager {
            sparkles_owl::classify::Expansion::Eager
        } else {
            sparkles_owl::classify::Expansion::Cautious
        },
        dl_work,
        counting,
        rooted,
        ..Limits::default()
    };
    let mut times = Vec::new();
    let mut add_times = Vec::new();
    let mut out = None;
    let mut k: Option<Classifier> = None;
    let mut sat: Option<Saturation> = None;
    for _ in 0..repeat {
        // the previous run's classifier and answer go first, so that the peak resident
        // memory is that of one run
        drop(out.take());
        drop(k.take());
        drop(sat.take());
        if heap {
            HELD.store(0, Ordering::Relaxed);
            PEAK.store(0, Ordering::Relaxed);
            COUNT.store(true, Ordering::Relaxed);
        }
        let t = Instant::now();
        let o = if owned {
            let mut c = Classifier::of(&axioms);
            add_times.push(t.elapsed().as_secs_f64() * 1000.0);
            let o = c.classify(&limits).expect("classify");
            k = Some(c);
            o
        } else {
            let kinds: Vec<_> = axioms
                .iter()
                .map(|a| &a.kind)
                .filter(|k| logical(k))
                .collect();
            add_times.push(t.elapsed().as_secs_f64() * 1000.0);
            let mut s = Saturation::new();
            let o = s.classify(&kinds, None, &limits).expect("classify");
            sat = Some(s);
            o
        };
        times.push(t.elapsed().as_secs_f64() * 1000.0);
        COUNT.store(false, Ordering::Relaxed);
        out = Some(o);
    }
    let out = out.expect("one run");
    let excluded = match (&k, &sat) {
        (Some(k), _) => k.excluded(),
        (_, Some(s)) => s.excluded().to_vec(),
        _ => Vec::new(),
    };
    // the axioms left out, on standard error, when EL_CLASSIFY_EXCLUDED is set
    if std::env::var_os("EL_CLASSIFY_EXCLUDED").is_some() {
        for e in &excluded {
            eprintln!("excluded {:?}: {:?}", e.gap, e.axiom);
        }
    }
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
            if !n.classes.is_empty() {
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
    let median = |times: &[f64]| {
        let mut v = times.to_vec();
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
            "complete": out.complete,
            "incomplete": out.incomplete.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            "consistency": match out.consistency {
                sparkles_owl::classify::Consistency::Consistent => "consistent",
                sparkles_owl::classify::Consistency::Inconsistent => "inconsistent",
                sparkles_owl::classify::Consistency::Unknown => "unknown",
            },
            "outside": outside + rules,
            "rules": rules,
            "tier": s.tier.as_str(),
            "outsideHorn": s.outside_el.horn,
            "outsideDisjunctive": s.outside_el.disjunctive,
            "outsideGlobal": s.outside_el.global,
            "outsideTaken": s.outside_el.taken,
            "conjunctions": s.conjunctions,
            "hornOverBudget": s.horn_over_budget,
            "clauses": s.clauses,
            "dlContexts": s.dl_contexts,
            "dlOverBudget": s.dl_over_budget,
            "dlTests": s.dl_tests,
            "dlStored": s.dl_stored,
            "dlWork": s.dl_work,
            "dlCountingOver": s.dl_counting_over,
            "dlRootedOver": s.dl_rooted_over,
            "dlTestsHeld": s.dl_tests_held,
            "dlTestingMs": s.dl_testing.as_secs_f64() * 1000.0,
            "partial": partial,
            "threads": s.threads,
            "readMs": read_ms,
            "classifyMs": times,
            "classifyMedianMs": median(&times),
            "addMs": median(&add_times),
            "indexMs": s.index.as_secs_f64() * 1000.0,
            "saturationMs": s.saturation.as_secs_f64() * 1000.0,
            "taxonomyMs": s.taxonomy.as_secs_f64() * 1000.0,
            "contexts": s.contexts,
            "conclusions": s.conclusions,
            "estimatedBytes": s.memory,
            "peakEstimatedBytes": s.peak_memory,
            "peakRssKibAfterRead": rss_read,
            "peakRssKib": peak_rss_kib(),
            "peakHeapBytes": heap.then(|| PEAK.load(Ordering::Relaxed)),
        })
    );
}
