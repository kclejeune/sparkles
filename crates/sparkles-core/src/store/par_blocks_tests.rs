//! The parallel block reads of a snapshot merge its delta: they visit the quads of the
//! sequential merged scan, in the same order, for any ranges, piece size and delta.

use super::*;
use crate::index::ALL_COLS;
use crate::io::RdfFormat;
use crate::sparql::QueryOptions;
use crate::sparql::update::update;

const SUBJECTS: usize = 40_000;

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let mut nt = String::new();
    for i in 0..SUBJECTS {
        for p in 0..3 {
            nt.push_str(&format!(
                "<urn:s{i}> <urn:p{p}> <urn:o{}> .\n",
                (i * 7 + p) % 5000
            ));
        }
    }
    s.load(&[Source::from_bytes(
        nt.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    s
}

fn upd(s: &Store, u: &str) {
    update(s, u, &QueryOptions::default()).unwrap_or_else(|e| panic!("{u}: {e}"));
}

/// The keys of the ranges as the sequential merge visits them.
fn merged(snap: &Snapshot, perm: Perm, ranges: &[(Key, Key)]) -> Vec<Key> {
    let mut out = Vec::new();
    for &(lo, hi) in ranges {
        snap.scan_between(perm, lo, hi, |c| {
            match c {
                Chunk::Block(b, s, e) => out.extend((s..e).map(|i| b.key(i))),
                Chunk::Row(k) => out.push(k),
            }
            Ok(true)
        })
        .unwrap();
    }
    out
}

/// The keys of the ranges as the parallel reads visit them, with the columns of `mask`.
fn parallel(
    snap: &Snapshot,
    perm: Perm,
    ranges: &[(Key, Key)],
    mask: crate::index::ColMask,
    piece: usize,
) -> Vec<Key> {
    let parts = snap
        .par_blocks_in_ranges(perm, ranges, mask, piece, |b, s, e| {
            Ok((s..e).map(|i| b.key(i)).collect::<Vec<Key>>())
        })
        .unwrap();
    parts.concat()
}

fn masked(keys: &[Key], mask: crate::index::ColMask) -> Vec<Key> {
    keys.iter()
        .map(|k| std::array::from_fn(|c| if mask & (1 << c) != 0 { k[c] } else { 0 }))
        .collect()
}

fn xorshift(mut x: u64) -> impl FnMut() -> u64 {
    move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    }
}

/// Sorted, disjoint ranges between keys of `keys`, some of them ending between two keys.
fn ranges(next: &mut impl FnMut() -> u64, keys: &[Key], n: usize) -> Vec<(Key, Key)> {
    let mut ends: Vec<Key> = (0..2 * n)
        .map(|_| {
            let mut k = keys[(next() % keys.len() as u64) as usize];
            if next().is_multiple_of(3) {
                k[3] = k[3].wrapping_add(1);
            }
            k
        })
        .collect();
    ends.sort();
    ends.dedup();
    ends.chunks_exact(2).map(|w| (w[0], w[1])).collect()
}

fn check(snap: &Snapshot, seed: u64) {
    let mut next = xorshift(seed);
    for perm in Perm::ALL {
        let all = merged(snap, perm, &[([0; 4], [u64::MAX; 4])]);
        let mut cases = vec![vec![([0; 4], [u64::MAX; 4])]];
        // each block alone, and up to the first key of the block after it
        let blocks = &snap.perm(perm).blocks;
        for (b, m) in blocks.iter().enumerate() {
            cases.push(vec![(m.first, m.last)]);
            if let Some(n) = blocks.get(b + 1) {
                let mut before = n.first;
                before[3] = before[3].wrapping_sub(1);
                cases.push(vec![(m.first, before)]);
            }
        }
        for n in [1, 3, 12] {
            cases.push(ranges(&mut next, &all, n));
        }
        for rs in &cases {
            let want = merged(snap, perm, rs);
            for piece in [1, 700, 8192] {
                assert_eq!(
                    parallel(snap, perm, rs, ALL_COLS, piece),
                    want,
                    "{perm:?} {rs:?} piece {piece}"
                );
            }
            let mask = 1 << (next() % 4);
            assert_eq!(
                masked(&parallel(snap, perm, rs, mask, 8192), mask),
                masked(&want, mask),
                "{perm:?} {rs:?} mask {mask}"
            );
        }
    }
}

#[test]
fn parallel_block_reads_match_the_merged_scan() {
    let s = store();
    let before = s.snapshot();
    assert!(before.perm(Perm::Spo).blocks.len() >= 3);
    check(&before, 0x9e37_79b9_7f4a_7c15);

    // the last quad of every SPO block but the last gets a neighbour with a new literal,
    // which sorts after its object and before the next block's first quad
    let term = |id: u64| before.term(Id(id)).unwrap().to_string();
    let blocks = before.perm(Perm::Spo).blocks.clone();
    let mut ins = String::new();
    let mut del = String::new();
    for (b, m) in blocks.iter().enumerate() {
        let (s0, p0, o0) = (term(m.first[0]), term(m.first[1]), term(m.first[2]));
        let (s1, p1, o1) = (term(m.last[0]), term(m.last[1]), term(m.last[2]));
        // the first and last quads of the block are deleted
        del.push_str(&format!("{s0} {p0} {o0} . {s1} {p1} {o1} . "));
        if b + 1 < blocks.len() {
            ins.push_str(&format!("{s1} {p1} \"gap {b}\" . "));
        }
    }
    // known terms in new combinations, inside the blocks
    let mut next = xorshift(0x2545_f491_4f6c_dd1d);
    for _ in 0..600 {
        let (i, p, o) = (next() % SUBJECTS as u64, next() % 3, next() % 5000);
        ins.push_str(&format!("<urn:s{i}> <urn:p{p}> <urn:o{o}> . "));
    }
    // a run of deleted quads, and new subjects after every base subject
    for i in 1000..1400 {
        del.push_str(&format!(
            "<urn:s{i}> <urn:p1> <urn:o{}> . ",
            (i * 7 + 1) % 5000
        ));
    }
    for i in 0..300 {
        ins.push_str(&format!("<urn:new{i}> <urn:p{}> <urn:o{i}> . ", i % 4));
    }
    upd(
        &s,
        &format!("DELETE DATA {{ {del} }} ; INSERT DATA {{ {ins} }}"),
    );
    let after = s.snapshot();
    // some inserted SPO keys lie between two base blocks, and some after the last one
    let spo = Perm::Spo.index();
    let between = |lo: Key, hi: Key| after.delta.ins[spo].range(lo..=hi).next().is_some();
    let gaps = blocks
        .windows(2)
        .filter(|w| {
            let (mut lo, mut hi) = (w[0].last, w[1].first);
            lo[3] = lo[3].wrapping_add(1);
            hi[3] = hi[3].wrapping_sub(1);
            between(lo, hi)
        })
        .count();
    assert!(gaps > 0, "no inserted key between two blocks");
    let mut tail = blocks.last().unwrap().last;
    tail[3] = tail[3].wrapping_add(1);
    assert!(between(tail, [u64::MAX; 4]));
    check(&after, 0xd1b5_4a32_d192_ed03);
    // the snapshot before the update reads as it did
    check(&before, 0x8cb9_2ba7_2f3d_8dd7);
}
