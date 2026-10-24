//! Time the decoding of a base vocabulary in the access patterns that queries use, to
//! compare codecs (spec X11 §4.3, roadmap item 108).
//!
//! ```text
//! cargo run --release -p sparkles-core --example vocab_decode -- GENERATION_DIR [ROUNDS]
//! ```
//!
//! GENERATION_DIR holds `vocab.dat`. Each pattern runs ROUNDS times (default 5) and the
//! median is printed in nanoseconds per decoded key:
//!
//! * `seq-all` decodes every key in order (`Vocab::for_each`).
//! * `dense-lit` decodes every literal through `get_sorted`, as a filter over a literal
//!   range does.
//! * `scatter-N` decodes sorted batches of N ids drawn at random from the whole
//!   vocabulary, as a result writer or an expression cache does with scattered objects.
//! * `single` decodes random ids one at a time (`Vocab::get_sorted` of one id), as a
//!   lookup of a few terms does.

use sparkles_core::vocab::Vocab;
use std::hint::black_box;
use std::path::Path;
use std::time::Instant;

fn xorshift(x: &mut u64) -> u64 {
    *x ^= *x << 13;
    *x ^= *x >> 7;
    *x ^= *x << 17;
    *x
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn time(rounds: usize, keys: u64, mut f: impl FnMut() -> u64) -> f64 {
    let mut out = Vec::new();
    for _ in 0..rounds {
        let t = Instant::now();
        black_box(f());
        out.push(t.elapsed().as_nanos() as f64 / keys as f64);
    }
    median(out)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = Path::new(args.get(1).expect("GENERATION_DIR"));
    let rounds: usize = args.get(2).map_or(5, |s| s.parse().unwrap());
    let v = Vocab::open(dir).unwrap();
    let n = v.len();
    let lits = v.first_triple();
    println!(
        "{}: {} keys, {} literals, fsst {}",
        dir.display(),
        n,
        lits,
        v.is_fsst()
    );
    let ns = time(rounds, n, || {
        let mut s = 0u64;
        v.for_each(|_, k| s += k.len() as u64);
        s
    });
    println!("seq-all     {ns:8.2} ns/key");
    let ids: Vec<u64> = (0..lits).collect();
    let ns = time(rounds, lits.max(1), || {
        let mut s = 0u64;
        v.get_sorted(&ids, |_, k| s += k.len() as u64);
        s
    });
    println!("dense-lit   {ns:8.2} ns/key");
    for batch in [64usize, 1024, 16384] {
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let total = 1_000_000usize;
        let batches: Vec<Vec<u64>> = (0..total / batch)
            .map(|_| {
                let mut b: Vec<u64> = (0..batch).map(|_| xorshift(&mut x) % n).collect();
                b.sort_unstable();
                b.dedup();
                b
            })
            .collect();
        let keys: usize = batches.iter().map(Vec::len).sum();
        let ns = time(rounds, keys as u64, || {
            let mut s = 0u64;
            for b in &batches {
                v.get_sorted(b, |_, k| s += k.len() as u64);
            }
            s
        });
        println!("scatter-{batch:<5}{ns:8.2} ns/key");
    }
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    let singles: Vec<u64> = (0..200_000).map(|_| xorshift(&mut x) % n).collect();
    let ns = time(rounds, singles.len() as u64, || {
        let mut s = 0u64;
        for &id in &singles {
            v.get_sorted(&[id], |_, k| s += k.len() as u64);
        }
        s
    });
    println!("single      {ns:8.2} ns/key");
    let lsingles: Vec<u64> = singles.iter().map(|&i| i % lits.max(1)).collect();
    let ns = time(rounds, lsingles.len() as u64, || {
        let mut s = 0u64;
        for &id in &lsingles {
            v.get_sorted(&[id], |_, k| s += k.len() as u64);
        }
        s
    });
    println!("single-lit  {ns:8.2} ns/key");
}
