//! Measures runs of a kept classifier after single-axiom edits against a classifier
//! that starts again, for roadmap item 753.
//!
//! ```text
//! incremental_classify [--threads N] [--edits N] [--seed N] [--counting] [--check] [--abox] INPUT
//! ```
//!
//! INPUT is functional-style syntax. The classifier runs once over the file, the cold
//! run. Each edit then removes one class axiom, chosen by the seed among the
//! `SubClassOf`, `EquivalentClasses` and `DisjointClasses` axioms, classifies, adds the
//! axiom back and classifies again. A classifier that starts again over the same
//! axioms gives the time of a full run after each removal, and with `--check` its
//! taxonomy must equal the kept classifier's after every run. The output is one JSON
//! line: the cold run, the medians and the extremes of the kept and the full runs, how
//! many kept runs kept contexts, the contexts they cleared, the median time of the
//! index with the normalization, of the saturation and of the taxonomy in the kept and
//! in the full runs, and the classifier's estimate of the bytes it keeps and of its
//! peak, with the steps of the kept runs that started again and of the cold run. The
//! steps also give the part of the saturation after its first pass, the
//! marking of real contexts with the facts, the key merges and the worlds. With `--abox`
//! the edited axiom is chosen among the assertions about individuals instead.

use sparkles_owl::classify::{Classifier, Limits};
use sparkles_owl::model::AxiomKind;
use sparkles_owl::{Axiom, ofn_read};
use std::time::{Duration, Instant};

#[global_allocator]
static GLOBAL: sparkles_alloc::Mimalloc = sparkles_alloc::Mimalloc;

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// The median, the least and the greatest of the times, in milliseconds.
fn spread(mut v: Vec<f64>) -> (f64, f64, f64) {
    if v.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    v.sort_by(|a, b| a.total_cmp(b));
    (v[v.len() / 2], v[0], v[v.len() - 1])
}

fn main() {
    let mut threads = 4usize;
    let mut edits = 20usize;
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut counting = false;
    let mut check = false;
    let mut abox = false;
    let mut input = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut number = || args.next().and_then(|s| s.parse::<u64>().ok());
        match a.as_str() {
            "--threads" => threads = number().expect("--threads N") as usize,
            "--edits" => edits = number().expect("--edits N") as usize,
            "--seed" => seed = number().expect("--seed N") | 1,
            "--counting" => counting = true,
            "--check" => check = true,
            "--abox" => abox = true,
            _ => input = Some(a),
        }
    }
    let input = input.expect(
        "usage: incremental_classify [--threads N] [--edits N] [--seed N] [--counting] [--check] [--abox] INPUT",
    );
    let text = std::fs::read_to_string(&input).expect("the input file");
    let doc = ofn_read::parse(&text).expect("functional-style syntax");
    let mut held: Vec<Axiom> = doc.axioms;
    let limits = Limits {
        threads,
        counting,
        ..Limits::default()
    };
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut kept = Classifier::of(&held);
    let t = Instant::now();
    let cold = kept.classify(&limits).expect("the cold run");
    let cold_ms = ms(t.elapsed());
    let candidates: Vec<usize> = held
        .iter()
        .enumerate()
        .filter(|(_, a)| {
            if abox {
                matches!(
                    a.kind,
                    AxiomKind::ClassAssertion(..)
                        | AxiomKind::ObjectPropertyAssertion(..)
                        | AxiomKind::DataPropertyAssertion(..)
                        | AxiomKind::SameIndividual(_)
                )
            } else {
                matches!(
                    a.kind,
                    AxiomKind::SubClassOf(..)
                        | AxiomKind::EquivalentClasses(_)
                        | AxiomKind::DisjointClasses(_)
                )
            }
        })
        .map(|(i, _)| i)
        .collect();
    let (mut removals, mut returns, mut full) = (Vec::new(), Vec::new(), Vec::new());
    let (mut incremental, mut cleared, mut runs) = (0usize, Vec::new(), 0usize);
    let (mut memory, mut peak) = (cold.stats.memory, cold.stats.peak_memory);
    // the steps of the kept runs and of the full runs: the index with the
    // normalization, the saturation, the taxonomy and the part of the saturation after
    // its first pass
    let (mut kept_steps, mut full_steps, mut restart_steps) = (
        [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
    );
    let steps = |into: &mut [Vec<f64>; 4], s: &sparkles_owl::classify::Stats| {
        into[0].push(ms(s.index));
        into[1].push(ms(s.saturation));
        into[2].push(ms(s.taxonomy));
        into[3].push(ms(s.marking));
    };
    let contexts = cold.stats.contexts;
    for _ in 0..edits {
        if candidates.is_empty() {
            break;
        }
        let at = candidates[next() as usize % candidates.len()];
        let axiom = held[at].clone();
        // the removal
        kept.remove([&axiom]);
        let t = Instant::now();
        let out = kept.classify(&limits).expect("the run after a removal");
        removals.push(ms(t.elapsed()));
        runs += 1;
        incremental += usize::from(out.stats.incremental);
        if out.stats.incremental {
            cleared.push(out.stats.cleared as f64);
            steps(&mut kept_steps, &out.stats);
        } else {
            steps(&mut restart_steps, &out.stats);
        }
        memory = memory.max(out.stats.memory);
        peak = peak.max(out.stats.peak_memory);
        let rest: Vec<Axiom> = held
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != at)
            .map(|(_, a)| a.clone())
            .collect();
        let mut fresh = Classifier::of(&rest);
        let t = Instant::now();
        let want = fresh.classify(&limits).expect("the full run");
        full.push(ms(t.elapsed()));
        steps(&mut full_steps, &want.stats);
        if check {
            assert!(
                out.taxonomy == want.taxonomy,
                "the taxonomies differ after a removal"
            );
            assert!(kept.excluded() == fresh.excluded());
        }
        drop(fresh);
        // the return
        kept.add([&axiom]);
        let t = Instant::now();
        let out = kept.classify(&limits).expect("the run after a return");
        returns.push(ms(t.elapsed()));
        runs += 1;
        incremental += usize::from(out.stats.incremental);
        if out.stats.incremental {
            cleared.push(out.stats.cleared as f64);
            steps(&mut kept_steps, &out.stats);
        } else {
            steps(&mut restart_steps, &out.stats);
        }
        memory = memory.max(out.stats.memory);
        peak = peak.max(out.stats.peak_memory);
        if check {
            assert!(
                out.taxonomy == cold.taxonomy,
                "the taxonomies differ after a return"
            );
        }
        held[at] = axiom;
    }
    let (rm, rlo, rhi) = spread(removals);
    let (am, alo, ahi) = spread(returns);
    let (fm, flo, fhi) = spread(full);
    let (cm, clo, chi) = spread(cleared);
    let [ki, ks, kt, km] = kept_steps.map(|v| spread(v).0);
    let [fi, fs, ft, fmk] = full_steps.map(|v| spread(v).0);
    let [xi, xs, xt, xm] = restart_steps.map(|v| spread(v).0);
    let c = &cold.stats;
    let (ci, cs, ct, cmk) = (ms(c.index), ms(c.saturation), ms(c.taxonomy), ms(c.marking));
    println!(
        "{{\"file\":\"{}\",\"threads\":{threads},\"counting\":{counting},\"tier\":\"{:?}\",\
         \"axioms\":{},\"classes\":{},\"contexts\":{contexts},\"cold_ms\":{cold_ms:.2},\
         \"runs\":{runs},\"incremental\":{incremental},\
         \"removal_ms\":[{rm:.3},{rlo:.3},{rhi:.3}],\"return_ms\":[{am:.3},{alo:.3},{ahi:.3}],\
         \"full_ms\":[{fm:.2},{flo:.2},{fhi:.2}],\"cleared\":[{cm},{clo},{chi}],\
         \"kept_steps_ms\":[{ki:.3},{ks:.3},{kt:.3},{km:.3}],\
         \"full_steps_ms\":[{fi:.3},{fs:.3},{ft:.3},{fmk:.3}],\"abox\":{abox},\
         \"restart_steps_ms\":[{xi:.3},{xs:.3},{xt:.3},{xm:.3}],\
         \"cold_steps_ms\":[{ci:.3},{cs:.3},{ct:.3},{cmk:.3}],\
         \"cold_memory\":{},\"cold_peak\":{},\"kept_memory\":{memory},\"kept_peak\":{peak},\
         \"checked\":{check}}}",
        std::path::Path::new(&input)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(""),
        cold.stats.tier,
        held.len(),
        cold.taxonomy.classes().count(),
        cold.stats.memory,
        cold.stats.peak_memory,
    );
}
