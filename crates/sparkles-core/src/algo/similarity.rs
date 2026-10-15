//! Node similarity over distinct outgoing neighbour sets. Call-owned fixed workspaces
//! are reused across contiguous source ranges; every retained output capacity is admitted.
use super::memory::{self, Buffer, Reservation, Work};
use super::{Control, Csr, Graph, Par, bad};
use crate::error::{Error, Result};
use std::cmp::Ordering as Cmp;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Jaccard,
    Overlap,
}
#[derive(Clone, Debug)]
pub struct Params {
    pub metric: Metric,
    pub top_k: usize,
    pub similarity_cutoff: f64,
    /// Nodes with fewer neighbours are left out.
    pub degree_cutoff: usize,
    /// The most distinct ordered candidate pairs one run examines, before filters.
    pub max_pairs: u64,
}
impl Default for Params {
    fn default() -> Self {
        Self {
            metric: Metric::Jaccard,
            top_k: 10,
            similarity_cutoff: 1e-42,
            degree_cutoff: 1,
            max_pairs: 10_000_000,
        }
    }
}

// Data drops before its reservation, including partial construction and unwind.
struct DistinctCsr {
    csr: Csr,
    _reservation: Reservation,
}
impl Deref for DistinctCsr {
    type Target = Csr;
    fn deref(&self) -> &Csr {
        &self.csr
    }
}
fn distinct(c: &Csr, ctl: &Control) -> Result<Option<DistinctCsr>> {
    let mut work = Work::new(ctl)?;
    let mut repeat = false;
    'detect: for u in 0..c.n() as u32 {
        work.tick("similarity-distinct-detect")?;
        for pair in c.row(u).windows(2) {
            work.tick("similarity-distinct-detect")?;
            if pair[0] == pair[1] {
                repeat = true;
                break 'detect;
            }
        }
    }
    if !repeat {
        return Ok(None);
    }
    let offsets = c
        .n()
        .checked_add(1)
        .ok_or_else(|| bad("nodeSimilarity", "offset count overflows"))?;
    let bytes = memory::sum(&[
        memory::bytes::<u64>(offsets)?,
        memory::bytes::<u32>(c.nnz())?,
    ])?;
    let mut owned = DistinctCsr {
        csr: Csr::default(),
        _reservation: Reservation::new(ctl, bytes)?,
    };
    owned.csr.off = memory::allocate("similarity-distinct-offsets", offsets, ctl)?;
    for _ in 0..offsets {
        work.tick("similarity-distinct-fill")?;
        owned.csr.off.push(0);
    }
    owned.csr.adj = memory::allocate("similarity-distinct-adjacency", c.nnz(), ctl)?;
    for u in 0..c.n() as u32 {
        work.tick("similarity-distinct-copy")?;
        let start = owned.csr.adj.len();
        for &v in c.row(u) {
            work.tick("similarity-distinct-copy")?;
            if owned.csr.adj.len() == start || owned.csr.adj.last() != Some(&v) {
                owned.csr.adj.push(v);
            }
        }
        owned.csr.off[u as usize + 1] = u64::try_from(owned.csr.adj.len())
            .map_err(|_| bad("nodeSimilarity", "offset overflows"))?;
    }
    ctl.check()?;
    Ok(Some(owned))
}

type Pair = (u32, u32, f64);
type Candidate = (u32, f64);
struct Scratch {
    count: Buffer<u32>,
    touched: Buffer<u32>,
    heap: Buffer<Candidate>,
}
struct Workspace {
    scratch: Option<Scratch>,
    output: Buffer<Pair>,
    error: Option<Error>,
    lo: u32,
    hi: u32,
    limit: usize,
}

fn range(n: usize, w: usize, i: usize) -> Result<(usize, usize)> {
    let base = n / w;
    let rest = n % w;
    let lo = base
        .checked_mul(i)
        .and_then(|v| v.checked_add(i.min(rest)))
        .ok_or_else(|| bad("nodeSimilarity", "range overflows"))?;
    let hi = lo
        .checked_add(base + usize::from(i < rest))
        .ok_or_else(|| bad("nodeSimilarity", "range overflows"))?;
    Ok((lo, hi))
}
fn dimension(n: usize) -> Result<()> {
    if n >= u32::MAX as usize {
        return Err(bad("nodeSimilarity", "more than 4 billion nodes"));
    }
    Ok(())
}
fn admit_pairs(pairs: &AtomicU64, count: usize, max: u64) -> Result<()> {
    let count =
        u64::try_from(count).map_err(|_| bad("nodeSimilarity", "candidate count overflows"))?;
    pairs.fetch_update(Ordering::Relaxed,Ordering::Relaxed,|n|n.checked_add(count).filter(|&v|v<=max))
        .map(|_|()).map_err(|_|bad("nodeSimilarity",format!("more than {max} candidate pairs; raise algo:maxPairs, or project fewer predicates")))
}

enum Failed {
    Original(Error),
    Stopped,
}
impl From<Error> for Failed {
    fn from(e: Error) -> Self {
        Self::Original(e)
    }
}
type WorkerResult<T> = std::result::Result<T, Failed>;
struct Poll<'a> {
    work: Work<'a>,
    stop: &'a AtomicBool,
    left: usize,
}
impl<'a> Poll<'a> {
    fn new(ctl: &'a Control, stop: &'a AtomicBool) -> WorkerResult<Self> {
        if stop.load(Ordering::Acquire) {
            return Err(Failed::Stopped);
        }
        Ok(Self {
            work: Work::new(ctl)?,
            stop,
            left: super::CHECK_EVERY,
        })
    }
    fn tick(&mut self, phase: &'static str) -> WorkerResult<()> {
        self.work.tick(phase)?;
        self.left -= 1;
        if self.left == 0 {
            self.left = super::CHECK_EVERY;
            if self.stop.load(Ordering::Acquire) {
                return Err(Failed::Stopped);
            }
        }
        Ok(())
    }
}
// Less means better: larger similarity, then smaller destination ID.
fn rank(a: Candidate, b: Candidate) -> Cmp {
    b.1.total_cmp(&a.1).then(a.0.cmp(&b.0))
}
fn sift_down(
    heap: &mut [Candidate],
    len: usize,
    mut root: usize,
    poll: &mut Poll<'_>,
) -> WorkerResult<()> {
    loop {
        poll.tick("similarity-heap")?;
        let Some(left) = root
            .checked_mul(2)
            .and_then(|v| v.checked_add(1))
            .filter(|&v| v < len)
        else {
            return Ok(());
        };
        let right = left + 1;
        let worst = if right < len && rank(heap[right], heap[left]).is_gt() {
            right
        } else {
            left
        };
        if !rank(heap[worst], heap[root]).is_gt() {
            return Ok(());
        }
        heap.swap(root, worst);
        root = worst;
    }
}
fn select(
    heap: &mut Buffer<Candidate>,
    candidate: Candidate,
    poll: &mut Poll<'_>,
) -> WorkerResult<()> {
    poll.tick("similarity-select")?;
    if heap.len() < heap.capacity() {
        heap.push(candidate)?;
        let mut at = heap.len() - 1;
        while at > 0 {
            poll.tick("similarity-heap")?;
            let parent = (at - 1) / 2;
            if !rank(heap[at], heap[parent]).is_gt() {
                break;
            }
            heap.swap(at, parent);
            at = parent;
        }
    } else if !heap.is_empty() && rank(candidate, heap[0]).is_lt() {
        heap[0] = candidate;
        let len = heap.len();
        sift_down(heap, len, 0, poll)?;
    }
    Ok(())
}
fn best_first(heap: &mut [Candidate], poll: &mut Poll<'_>) -> WorkerResult<()> {
    let mut len = heap.len();
    while len > 1 {
        poll.tick("similarity-heap-sort")?;
        heap.swap(0, len - 1);
        len -= 1;
        sift_down(heap, len, 0, poll)?;
    }
    Ok(())
}
fn grow_output(
    output: &mut Buffer<Pair>,
    need: usize,
    limit: usize,
    ctl: &Control,
    poll: &mut Poll<'_>,
) -> WorkerResult<()> {
    if need <= output.capacity() {
        return Ok(());
    }
    if need > limit {
        return Err(bad("nodeSimilarity", "workspace output bound exceeded").into());
    }
    let capacity = output
        .capacity()
        .checked_mul(2)
        .map(|n| n.max(4).max(need).min(limit))
        .ok_or_else(|| bad("nodeSimilarity", "output growth overflows"))?;
    let mut next = Buffer::new("similarity-worker-output", capacity, ctl)?;
    for pair in output.drain() {
        poll.tick("similarity-output-move")?;
        next.push(pair)?;
    }
    ctl.check()?;
    *output = next;
    Ok(())
}
fn worker(
    slot: &mut Workspace,
    out: &Csr,
    inn: &Csr,
    p: &Params,
    pairs: &AtomicU64,
    stop: &AtomicBool,
    ctl: &Control,
) -> WorkerResult<()> {
    let mut poll = Poll::new(ctl, stop)?;
    let scratch = slot
        .scratch
        .as_mut()
        .expect("workspace scratch remains owned through joins");
    for u in slot.lo..slot.hi {
        poll.tick("similarity-source")?;
        let du = out.deg(u);
        if du < p.degree_cutoff.max(1) {
            continue;
        }
        scratch.touched.clear();
        scratch.heap.clear();
        for &x in out.row(u) {
            poll.tick("similarity-neighbour")?;
            for &v in inn.row(x) {
                poll.tick("similarity-intersections")?;
                if v == u {
                    continue;
                }
                let c = &mut scratch.count[v as usize];
                if *c == 0 {
                    scratch.touched.push(v)?;
                }
                *c = c
                    .checked_add(1)
                    .ok_or_else(|| bad("nodeSimilarity", "intersection count overflows"))?;
            }
        }
        admit_pairs(pairs, scratch.touched.len(), p.max_pairs)?;
        for &v in &*scratch.touched {
            poll.tick("similarity-candidates")?;
            let inter = scratch.count[v as usize] as f64;
            scratch.count[v as usize] = 0;
            let dv = out.deg(v);
            if dv < p.degree_cutoff.max(1) {
                continue;
            }
            let sim = match p.metric {
                Metric::Jaccard => {
                    inter
                        / (du
                            .checked_add(dv)
                            .ok_or_else(|| bad("nodeSimilarity", "degree sum overflows"))?
                            as f64
                            - inter)
                }
                Metric::Overlap => inter / du.min(dv) as f64,
            };
            if sim >= p.similarity_cutoff {
                select(&mut scratch.heap, (v, sim), &mut poll)?;
            }
        }
        best_first(&mut scratch.heap, &mut poll)?;
        for &(v, sim) in &*scratch.heap {
            poll.tick("similarity-output")?;
            let need = slot
                .output
                .len()
                .checked_add(1)
                .ok_or_else(|| bad("nodeSimilarity", "output count overflows"))?;
            grow_output(&mut slot.output, need, slot.limit, ctl, &mut poll)?;
            slot.output.push((u, v, sim))?;
        }
        #[cfg(test)]
        probe::at(
            probe::Event::Scratch {
                bytes: memory::sum(&[
                    memory::bytes::<u32>(scratch.count.capacity())?,
                    memory::bytes::<u32>(scratch.touched.capacity())?,
                    memory::bytes::<Candidate>(scratch.heap.capacity())?,
                    memory::bytes::<Pair>(slot.output.capacity())?,
                ])?,
            },
            ctl,
        );
    }
    ctl.check()?;
    Ok(())
}

/// Similar pairs in ascending source order, each source's best scores first.
/// Scratch is bounded by admitted logical workspaces, including sequential fallback.
pub fn run(g: &Graph, p: &Params, par: &Par, ctl: &Control) -> Result<Vec<(u32, u32, f64)>> {
    if p.top_k == 0 {
        return Err(bad("nodeSimilarity", "topK must be above 0"));
    }
    #[cfg(test)]
    let _relay = probe::relay(ctl);
    let n = g.n();
    dimension(n)?;
    let owned = distinct(&g.out, ctl)?;
    let out = owned.as_deref().unwrap_or(&g.out);
    let local;
    let shared;
    let inn: &Csr = if g.symmetric {
        out
    } else if owned.is_some() {
        local = out.transpose(ctl)?;
        &local
    } else {
        shared = g.inn(ctl)?;
        &shared
    };
    let w = par.threads().min(n);
    let k = p.top_k.min(n.saturating_sub(1));
    let mut slots = Buffer::new("similarity-workspaces", w, ctl)?;
    for i in 0..w {
        let (lo, hi) = range(n, w, i)?;
        let limit = (hi - lo)
            .checked_mul(k)
            .ok_or_else(|| bad("nodeSimilarity", "workspace output bound overflows"))?;
        let count = Buffer::filled("similarity-count", n, 0u32, ctl)?;
        #[cfg(test)]
        probe::at(
            probe::Event::Counter {
                capacity: count.capacity(),
            },
            ctl,
        );
        let touched = Buffer::new("similarity-touched", n.saturating_sub(1), ctl)?;
        let heap = Buffer::new("similarity-heap", k, ctl)?;
        slots.push(Workspace {
            scratch: Some(Scratch {
                count,
                touched,
                heap,
            }),
            output: Buffer::new("similarity-empty-output", 0, ctl)?,
            error: None,
            lo: lo as u32,
            hi: hi as u32,
            limit,
        })?;
    }
    let pairs = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    par.chunks_mut(&mut slots, 1, |_, chunk| {
        #[cfg(test)]
        let _relay = probe::relay(ctl);
        #[cfg(test)]
        probe::at(
            probe::Event::Worker {
                slot: chunk[0].lo as usize,
            },
            ctl,
        );
        let slot = &mut chunk[0];
        match worker(slot, out, inn, p, &pairs, &stop, ctl) {
            Ok(()) | Err(Failed::Stopped) => {}
            Err(Failed::Original(error)) => {
                slot.error = Some(error);
                stop.store(true, Ordering::Release);
            }
        }
        Ok(())
    })?;
    for slot in &mut *slots {
        if let Some(error) = slot.error.take() {
            return Err(error);
        }
    }
    let mut total = 0usize;
    let mut old_bytes = 0u64;
    for slot in &mut *slots {
        drop(slot.scratch.take());
        total = total
            .checked_add(slot.output.len())
            .ok_or_else(|| bad("nodeSimilarity", "result count overflows"))?;
        old_bytes = old_bytes
            .checked_add(memory::bytes::<Pair>(slot.output.capacity())?)
            .ok_or_else(|| bad("nodeSimilarity", "output capacities overflow"))?;
    }
    let mut output = Buffer::new("similarity-result", total, ctl)?;
    #[cfg(test)]
    probe::at(
        probe::Event::Merge {
            old_bytes,
            new_bytes: memory::bytes::<Pair>(output.capacity())?,
        },
        ctl,
    );
    let _ = old_bytes;
    let mut work = Work::new(ctl)?;
    for slot in &mut *slots {
        for pair in slot.output.drain() {
            work.tick("similarity-merge")?;
            output.push(pair)?;
        }
        let empty = Buffer::new("similarity-empty-output", 0, ctl)?;
        drop(std::mem::replace(&mut slot.output, empty));
    }
    drop(slots);
    ctl.check()?;
    Ok(output.into_output())
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    #[test]
    fn jaccard_of_the_spec_example() {
        // u1 = 0, u2 = 1, u3 = 2; x = 3, y = 4
        let g = digraph(5, &[(0, 3), (0, 4), (1, 3), (1, 4), (2, 3)]);
        let r = run(
            &g,
            &Params::default(),
            &Par::single(),
            &Control::unlimited(),
        )
        .unwrap();
        assert!(r.contains(&(0, 1, 1.0)));
        assert!(r.contains(&(0, 2, 0.5)));
        assert!(r.contains(&(1, 2, 0.5)));
        let o = run(
            &g,
            &Params {
                metric: Metric::Overlap,
                ..Params::default()
            },
            &Par::single(),
            &Control::unlimited(),
        )
        .unwrap();
        // u3's single neighbour is inside u1's set
        assert!(o.contains(&(0, 2, 1.0)));
    }

    #[test]
    fn top_k_and_cutoff() {
        let g = digraph(5, &[(0, 3), (0, 4), (1, 3), (1, 4), (2, 3)]);
        let p = Params {
            top_k: 1,
            similarity_cutoff: 0.75,
            ..Params::default()
        };
        let r = run(&g, &p, &Par::single(), &Control::unlimited()).unwrap();
        assert_eq!(r, vec![(0, 1, 1.0), (1, 0, 1.0)]);
    }

    #[test]
    fn candidate_limit_fails() {
        let g = digraph(5, &[(0, 3), (0, 4), (1, 3), (1, 4), (2, 3)]);
        let p = Params {
            max_pairs: 1,
            ..Params::default()
        };
        assert!(run(&g, &p, &Par::single(), &Control::unlimited()).is_err());
    }

    #[test]
    fn matches_set_operations() {
        let mut x = 21u64;
        let mut rnd = |m: u32| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) as u32 % m
        };
        let n = 20u32;
        let mut e: Vec<(u32, u32)> = (0..80).map(|_| (rnd(n), rnd(n))).collect();
        e.sort_unstable();
        e.dedup();
        let g = digraph(n, &e);
        let p = Params {
            top_k: 100,
            ..Params::default()
        };
        let r = run(&g, &p, &Par::with_threads(2), &Control::unlimited()).unwrap();
        for u in 0..n {
            for v in 0..n {
                if u == v {
                    continue;
                }
                let a: std::collections::BTreeSet<u32> =
                    e.iter().filter(|x| x.0 == u).map(|x| x.1).collect();
                let b: std::collections::BTreeSet<u32> =
                    e.iter().filter(|x| x.0 == v).map(|x| x.1).collect();
                let inter = a.intersection(&b).count() as f64;
                let want = if inter > 0.0 {
                    inter / ((a.len() + b.len()) as f64 - inter)
                } else {
                    0.0
                };
                let got = r.iter().find(|x| x.0 == u && x.1 == v).map(|x| x.2);
                match got {
                    Some(s) => assert!((s - want).abs() < 1e-12),
                    None => assert_eq!(inter, 0.0),
                }
            }
        }
    }
    #[test]
    fn r7_deduplicated_transpose_reserves_retention_and_scratch() {
        let g = digraph(4, &[(0, 2), (0, 2), (1, 2)]);
        let ctl = Control::new(Default::default(), None, 100);
        assert!(matches!(
            run(&g, &Params::default(), &Par::single(), &ctl),
            Err(crate::Error::BudgetExceeded(_))
        ));
        // R8 owns deduplicated outgoing storage too; failed scratch admission rolls it back.
        assert_eq!(ctl.remaining(), 100);
        let got = run(
            &g,
            &Params::default(),
            &Par::single(),
            &Control::unlimited(),
        )
        .unwrap();
        assert!(got.iter().any(|&(u, v, s)| u == 0 && v == 1 && s == 1.0));
        let sym = ugraph(3, &[(0, 1), (0, 1), (1, 2)]);
        let _ = run(
            &sym,
            &Params::default(),
            &Par::single(),
            &Control::unlimited(),
        )
        .unwrap();
    }
}

#[cfg(test)]
mod probe {
    use super::*;
    use std::sync::{Arc, Mutex};
    #[derive(Clone, Copy, Debug)]
    pub(super) enum Event {
        Counter {
            capacity: usize,
        },
        Worker {
            slot: usize,
        },
        Scratch {
            bytes: u64,
        },
        Merge {
            old_bytes: u64,
            new_bytes: u64,
        },
        Admission {
            name: &'static str,
            capacity: usize,
            bytes: u64,
        },
        Work(&'static str),
    }
    type Callback = Arc<dyn Fn(Event, &Control) + Send + Sync>;
    static HOOK: Mutex<Option<(Control, Callback)>> = Mutex::new(None);
    pub(super) struct Scope;
    pub(super) fn install(
        ctl: &Control,
        f: impl Fn(Event, &Control) + Send + Sync + 'static,
    ) -> Scope {
        let mut hook = HOOK.lock().unwrap();
        assert!(hook.is_none());
        *hook = Some((ctl.clone(), Arc::new(f)));
        Scope
    }
    impl Drop for Scope {
        fn drop(&mut self) {
            *HOOK.lock().unwrap() = None;
        }
    }
    pub(super) fn relay(ctl: &Control) -> Option<super::super::memory::probe::Scope> {
        if !HOOK
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(target, _)| target.same(ctl))
        {
            return None;
        }
        let ctl = ctl.clone();
        Some(super::super::memory::probe::install(
            move |point| match point {
                super::super::memory::probe::Point::Allocation(name, capacity) => {
                    let bytes = match name {
                        "similarity-workspaces" => {
                            super::super::memory::bytes::<Workspace>(capacity)
                        }
                        "similarity-count"
                        | "similarity-touched"
                        | "similarity-distinct-adjacency" => {
                            super::super::memory::bytes::<u32>(capacity)
                        }
                        "similarity-heap" => super::super::memory::bytes::<Candidate>(capacity),
                        "similarity-worker-output"
                        | "similarity-empty-output"
                        | "similarity-result" => super::super::memory::bytes::<Pair>(capacity),
                        "incoming-offsets" | "incoming-cursors" => {
                            super::super::memory::bytes::<u64>(capacity)
                        }
                        "incoming-edges" => super::super::memory::bytes::<u32>(capacity),
                        "incoming-weights" => super::super::memory::bytes::<f32>(capacity),
                        "similarity-distinct-offsets" => {
                            super::super::memory::bytes::<u64>(capacity)
                        }
                        _ => return,
                    }
                    .unwrap();
                    at(
                        Event::Admission {
                            name,
                            capacity,
                            bytes,
                        },
                        &ctl,
                    );
                }
                super::super::memory::probe::Point::Work(phase) => at(Event::Work(phase), &ctl),
                _ => {}
            },
            None,
        ))
    }
    pub(super) fn at(event: Event, ctl: &Control) {
        let hook = HOOK
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(c, _)| c.same(ctl))
            .map(|(_, f)| f.clone());
        if let Some(f) = hook {
            f(event, ctl);
        }
    }
}
#[cfg(test)]
mod r8_before {
    use super::super::testutil::*;
    use super::*;
    use std::sync::{Arc, atomic::AtomicUsize};
    pub(super) fn isolated(name: &str, body: impl FnOnce()) {
        if std::env::var("F14_R8_CASE").as_deref() == Ok(name) {
            body();
            return;
        }
        let full = if name.contains("::") {
            format!("algo::similarity::{name}")
        } else {
            format!("algo::similarity::r8_before::{name}")
        };
        let logs = tempfile::tempdir().unwrap();
        let stdout = logs.path().join("stdout");
        let stderr = logs.path().join("stderr");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &full, "--nocapture", "--test-threads=1"])
            .env("F14_R8_CASE", name)
            .stdout(std::fs::File::create(&stdout).unwrap())
            .stderr(std::fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let status = child.wait().unwrap();
                panic!(
                    "owned R8 probe {name} exceeded 30s; reaped {status:?}\n{}\n{}",
                    std::fs::read_to_string(&stdout).unwrap(),
                    std::fs::read_to_string(&stderr).unwrap()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        let output = std::process::Output {
            status,
            stdout: std::fs::read(stdout).unwrap(),
            stderr: std::fs::read(stderr).unwrap(),
        };
        println!(
            "R8 isolated {name} status {:?}\n{}\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success(),
            "child {:?}\n{}\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn counters(threads: usize) {
        let g = digraph(9 * 1024 + 1, &[(0, 1)]);
        let ctl = Control::unlimited();
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let main = std::thread::current().id();
        let rayon = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let witness = rayon.clone();
        let scope = probe::install(&ctl, move |event, _| match event {
            probe::Event::Counter { capacity } => {
                assert_eq!(capacity, g_n());
                observed.fetch_add(1, Ordering::Relaxed);
            }
            probe::Event::Worker { slot } => {
                println!(
                    "R8 worker slot={slot} thread={:?}",
                    std::thread::current().id()
                );
                if std::thread::current().id() != main {
                    witness.store(true, Ordering::Relaxed);
                }
            }
            _ => {}
        });
        let par = Par::with_threads(threads);
        assert_eq!(par.threads(), threads);
        assert!(run(&g, &Params::default(), &par, &ctl).unwrap().is_empty());
        drop(scope);
        assert!(
            rayon.load(Ordering::Relaxed),
            "actual Rayon worker witness missing"
        );
        let actual = count.load(Ordering::Relaxed);
        println!("R8 actual counters={actual} admitted={threads}");
        assert!(
            actual <= threads,
            "counter allocations {actual} exceed workspace bound {threads}"
        );
    }
    fn g_n() -> usize {
        9 * 1024 + 1
    }
    #[test]
    fn r8_counter_allocations_two() {
        isolated("r8_counter_allocations_two", || counters(2));
    }
    #[test]
    fn r8_counter_allocations_four() {
        isolated("r8_counter_allocations_four", || counters(4));
    }
    fn qualifying() -> Graph {
        digraph(65, &(0..64).map(|u| (u, 64)).collect::<Vec<_>>())
    }
    #[test]
    fn r8_scratch_growth_is_admitted() {
        isolated("r8_scratch_growth_is_admitted", || {
            let g = qualifying();
            let ctl = Control::unlimited();
            let scope = probe::install(&ctl, |event, ctl| {
                if let probe::Event::Scratch { bytes } = event {
                    assert!(
                        ctl.observed_used() >= bytes,
                        "known live scratch {bytes} exceeds all admitted {}",
                        ctl.observed_used()
                    );
                }
            });
            run(
                &g,
                &Params {
                    top_k: 64,
                    ..Default::default()
                },
                &Par::single(),
                &ctl,
            )
            .unwrap();
            drop(scope);
        });
    }
    #[test]
    fn r8_merge_overlap_is_admitted() {
        isolated("r8_merge_overlap_is_admitted", || {
            let g = qualifying();
            let ctl = Control::unlimited();
            let scope = probe::install(&ctl, |event, ctl| {
                if let probe::Event::Merge {
                    old_bytes,
                    new_bytes,
                } = event
                {
                    assert!(
                        ctl.observed_used() >= old_bytes + new_bytes,
                        "old outputs {old_bytes} + final {new_bytes} exceed all admitted {}",
                        ctl.observed_used()
                    );
                }
            });
            let output = run(
                &g,
                &Params {
                    top_k: 64,
                    ..Default::default()
                },
                &Par::single(),
                &ctl,
            )
            .unwrap();
            drop(scope);
            assert_eq!(output.len(), 64 * 63);
        });
    }
}

#[cfg(test)]
mod owned_tests {
    use super::super::testutil::*;
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    fn isolated(name: &str, f: impl FnOnce()) {
        super::r8_before::isolated(&format!("owned_tests::{name}"), f);
    }
    fn graph() -> Graph {
        digraph(
            7,
            &[
                (0, 3),
                (0, 3),
                (0, 4),
                (1, 3),
                (1, 4),
                (2, 3),
                (3, 3),
                (4, 5),
                (5, 4),
            ],
        )
    }
    fn oracle(g: &Graph, p: &Params) -> (Vec<Pair>, u64) {
        let rows: Vec<BTreeSet<u32>> = (0..g.n() as u32)
            .map(|u| g.out.row(u).iter().copied().collect())
            .collect();
        let mut output = Vec::new();
        let mut candidates = 0;
        for u in 0..g.n() as u32 {
            let a = &rows[u as usize];
            if a.len() < p.degree_cutoff.max(1) {
                continue;
            }
            let mut mine = Vec::new();
            for v in 0..g.n() as u32 {
                if u == v {
                    continue;
                }
                let b = &rows[v as usize];
                let inter = a.intersection(b).count();
                if inter == 0 {
                    continue;
                }
                candidates += 1;
                if b.len() < p.degree_cutoff.max(1) {
                    continue;
                }
                let sim = match p.metric {
                    Metric::Jaccard => inter as f64 / (a.len() + b.len() - inter) as f64,
                    Metric::Overlap => inter as f64 / a.len().min(b.len()) as f64,
                };
                if sim >= p.similarity_cutoff {
                    mine.push((u, v, sim));
                }
            }
            // Independent full sorting, with no shared heap comparator/helper.
            mine.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.1.cmp(&b.1)));
            mine.truncate(p.top_k);
            output.extend(mine);
        }
        (output, candidates)
    }
    fn bits(rows: &[Pair]) -> Vec<(u32, u32, u64)> {
        rows.iter().map(|&(u, v, s)| (u, v, s.to_bits())).collect()
    }
    #[test]
    fn complete_order_matches_full_set_oracle_at_one_two_four_threads() {
        let g = graph();
        for threads in [1, 2, 4] {
            let par = Par::with_threads(threads);
            for metric in [Metric::Jaccard, Metric::Overlap] {
                for top_k in [1, 3, 100] {
                    for degree_cutoff in [0, 1, 2, 4] {
                        for similarity_cutoff in [0., 0.5, 1., 1.1] {
                            let p = Params {
                                metric,
                                top_k,
                                degree_cutoff,
                                similarity_cutoff,
                                ..Default::default()
                            };
                            let (expected, candidates) = oracle(&g, &p);
                            let ctl = Control::unlimited();
                            let got = run(&g, &p, &par, &ctl).unwrap();
                            assert_eq!(bits(&got), bits(&expected), "threads={threads} {p:?}");
                            assert_eq!(
                                u64::MAX - ctl.remaining(),
                                memory::bytes::<Pair>(got.capacity()).unwrap()
                            );
                            let mut exact = p.clone();
                            exact.max_pairs = candidates;
                            assert_eq!(
                                bits(&run(&g, &exact, &par, &Control::unlimited()).unwrap()),
                                bits(&expected)
                            );
                            if candidates > 0 {
                                exact.max_pairs -= 1;
                                assert!(
                                    run(&g, &exact, &par, &Control::unlimited())
                                        .unwrap_err()
                                        .to_string()
                                        .contains("candidate pairs")
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn candidates_count_before_filters_and_checked_arithmetic() {
        let g = digraph(6, &[(0, 4), (0, 5), (1, 4), (2, 4), (3, 5)]);
        for threads in [1, 2, 4] {
            let p = Params {
                degree_cutoff: 2,
                similarity_cutoff: 1.1,
                max_pairs: 3,
                ..Default::default()
            };
            assert_eq!(oracle(&g, &p).1, 3);
            assert!(
                run(&g, &p, &Par::with_threads(threads), &Control::unlimited())
                    .unwrap()
                    .is_empty()
            );
            assert!(
                run(
                    &g,
                    &Params { max_pairs: 2, ..p },
                    &Par::with_threads(threads),
                    &Control::unlimited()
                )
                .is_err()
            );
        }
        let total = AtomicU64::new(u64::MAX - 1);
        admit_pairs(&total, 1, u64::MAX).unwrap();
        assert!(admit_pairs(&total, 1, u64::MAX).is_err());
        assert_eq!(total.load(Ordering::Relaxed), u64::MAX);
        dimension(u32::MAX as usize - 1).unwrap();
        assert!(dimension(u32::MAX as usize).is_err());
        let ranges = (0..4).map(|i| range(17, 4, i).unwrap()).collect::<Vec<_>>();
        assert_eq!(ranges, vec![(0, 5), (5, 9), (9, 13), (13, 17)]);
        for g in [digraph(0, &[]), digraph(5, &[]), ugraph(1, &[(0, 0)])] {
            let ctl = Control::unlimited();
            let output = run(
                &g,
                &Params {
                    max_pairs: 0,
                    ..Default::default()
                },
                &Par::single(),
                &ctl,
            )
            .unwrap();
            assert!(output.is_empty());
            assert_eq!(output.capacity(), 0);
            assert_eq!(ctl.remaining(), u64::MAX);
        }
    }
    #[test]
    fn allocation_errors_and_unwind_release_and_retry() {
        isolated("allocation_errors_and_unwind_release_and_retry", || {
            for name in [
                "similarity-distinct-offsets",
                "similarity-distinct-adjacency",
                "similarity-workspaces",
                "similarity-count",
                "similarity-touched",
                "similarity-heap",
                "similarity-worker-output",
                "similarity-result",
            ] {
                for unwind in [false, true] {
                    let g = graph();
                    let ctl = Control::new(Default::default(), None, 1 << 20);
                    let hook = memory::probe::install(
                        move |point| {
                            if let memory::probe::Point::Allocation(stage, capacity) = point
                                && stage == name
                                && capacity > 0
                                && unwind
                            {
                                panic!("owned R8 allocation unwind {stage}");
                            }
                        },
                        (!unwind).then_some(name),
                    );
                    let got = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run(&g, &Params::default(), &Par::single(), &ctl)
                    }));
                    drop(hook);
                    if unwind {
                        assert!(got.is_err(), "{name}");
                    } else {
                        assert!(matches!(got.unwrap(), Err(Error::Invalid(_))), "{name}");
                    }
                    assert_eq!(ctl.remaining(), 1 << 20, "{name}");
                    let output = run(&g, &Params::default(), &Par::single(), &ctl).unwrap();
                    assert_eq!(
                        (1 << 20) - ctl.remaining(),
                        memory::bytes::<Pair>(output.capacity()).unwrap()
                    );
                }
            }

            let g = digraph(65, &(0..64).map(|u| (u, 64)).collect::<Vec<_>>());
            let ctl = Control::new(Default::default(), None, 1 << 20);
            let hook = probe::install(&ctl, |event, _| {
                if let probe::Event::Admission {
                    name: "similarity-worker-output",
                    ..
                } = event
                {
                    panic!("Rayon output allocation unwind");
                }
            });
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&g, &Params::default(), &Par::with_threads(4), &ctl)
            }));
            drop(hook);
            assert!(result.is_err());
            assert_eq!(ctl.remaining(), 1 << 20);
            let output = run(&g, &Params::default(), &Par::with_threads(4), &ctl).unwrap();
            assert_eq!(
                (1 << 20) - ctl.remaining(),
                memory::bytes::<Pair>(output.capacity()).unwrap()
            );
        });
    }
    #[test]
    fn preallocation_observations_exact_budget_and_retained_output() {
        isolated(
            "preallocation_observations_exact_budget_and_retained_output",
            || {
                let g = graph();
                let ctl = Control::unlimited();
                let seen = Arc::new(Mutex::new(BTreeSet::new()));
                let observed = seen.clone();
                let hook = probe::install(&ctl, move |event, ctl| {
                    if let probe::Event::Admission {
                        name,
                        capacity,
                        bytes,
                    } = event
                    {
                        assert!(ctl.observed_used() >= bytes, "{name} {capacity}");
                        observed.lock().unwrap().insert(name);
                    }
                });
                let output = run(
                    &g,
                    &Params {
                        top_k: 100,
                        ..Default::default()
                    },
                    &Par::single(),
                    &ctl,
                )
                .unwrap();
                drop(hook);
                assert_eq!(
                    u64::MAX - ctl.remaining(),
                    memory::bytes::<Pair>(output.capacity()).unwrap()
                );
                for name in [
                    "similarity-workspaces",
                    "similarity-count",
                    "similarity-touched",
                    "similarity-heap",
                    "similarity-worker-output",
                    "similarity-result",
                    "similarity-distinct-offsets",
                    "similarity-distinct-adjacency",
                ] {
                    assert!(
                        seen.lock().unwrap().contains(name),
                        "missing actual allocation {name}"
                    );
                }
                let peak = ctl.peak();
                println!("R8 exact requested-capacity peak={peak}");
                let exact = Control::new(Default::default(), None, peak);
                run(
                    &g,
                    &Params {
                        top_k: 100,
                        ..Default::default()
                    },
                    &Par::single(),
                    &exact,
                )
                .unwrap();
                assert_eq!(exact.peak(), peak);
                let short = Control::new(Default::default(), None, peak - 1);
                assert!(matches!(
                    run(
                        &g,
                        &Params {
                            top_k: 100,
                            ..Default::default()
                        },
                        &Par::single(),
                        &short
                    ),
                    Err(Error::BudgetExceeded(_))
                ));
                assert_eq!(short.remaining(), peak - 1);
                let g = digraph(65, &(0..64).map(|u| (u, 64)).collect::<Vec<_>>());
                let parallel = Control::new(Default::default(), None, 1 << 20);
                let main = std::thread::current().id();
                let worker = Arc::new(AtomicBool::new(false));
                let observed = worker.clone();
                let hook = probe::install(&parallel, move |event, ctl| {
                    if let probe::Event::Admission {
                        name: "similarity-worker-output",
                        capacity,
                        bytes,
                    } = event
                    {
                        assert!(capacity <= 65usize.div_ceil(4) * 64);
                        assert!(ctl.observed_used() >= bytes);
                        if std::thread::current().id() != main {
                            observed.store(true, Ordering::Relaxed);
                        }
                    }
                });
                let params = Params {
                    top_k: 64,
                    ..Default::default()
                };
                let output = run(&g, &params, &Par::with_threads(4), &parallel).unwrap();
                drop(hook);
                assert!(
                    worker.load(Ordering::Relaxed),
                    "parallel output allocation observer never reached Rayon"
                );
                assert_eq!(bits(&output), bits(&oracle(&g, &params).0));
                assert_eq!(
                    (1 << 20) - parallel.remaining(),
                    memory::bytes::<Pair>(output.capacity()).unwrap()
                );
            },
        );
    }
    #[test]
    fn phase_cancellation_and_actual_deadline_keep_original_errors() {
        isolated(
            "phase_cancellation_and_actual_deadline_keep_original_errors",
            || {
                for phase in [
                    "similarity-distinct-detect",
                    "similarity-distinct-copy",
                    "buffer-fill",
                    "similarity-intersections",
                    "similarity-candidates",
                    "similarity-select",
                    "similarity-heap",
                    "similarity-output-move",
                    "similarity-merge",
                ] {
                    let g = graph();
                    let flag = Arc::new(AtomicBool::new(false));
                    let ctl = Control::new(flag.clone(), None, 1 << 20);
                    let seen = Arc::new(AtomicBool::new(false));
                    let observed = seen.clone();
                    let hook = probe::install(&ctl, move |event, _| {
                        if let probe::Event::Work(at) = event
                            && at == phase
                        {
                            observed.store(true, Ordering::Relaxed);
                            flag.store(true, Ordering::Relaxed);
                        }
                    });
                    let result = run(
                        &g,
                        &Params {
                            top_k: 100,
                            ..Default::default()
                        },
                        &Par::with_threads(2),
                        &ctl,
                    );
                    drop(hook);
                    assert!(seen.load(Ordering::Relaxed), "phase not exercised {phase}");
                    assert!(
                        matches!(result, Err(Error::Cancelled)),
                        "{phase}: {result:?}"
                    );
                    assert_eq!(ctl.remaining(), 1 << 20, "{phase}");
                }
                let g = digraph(260, &(0..259).map(|u| (u, 259)).collect::<Vec<_>>());
                let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
                let ctl = Control::new(Default::default(), Some(deadline), 1 << 20);
                let seen = Arc::new(AtomicBool::new(false));
                let observed = seen.clone();
                let hook = probe::install(&ctl, move |event, _| {
                    if let probe::Event::Work("similarity-intersections") = event
                        && !observed.swap(true, Ordering::Relaxed)
                        && let Some(wait) =
                            deadline.checked_duration_since(std::time::Instant::now())
                    {
                        std::thread::sleep(wait);
                    }
                });
                let result = run(&g, &Params::default(), &Par::with_threads(2), &ctl);
                drop(hook);
                assert!(seen.load(Ordering::Relaxed));
                assert!(matches!(result, Err(Error::Timeout)));
                assert_eq!(ctl.remaining(), 1 << 20);
            },
        );
    }
    #[test]
    fn symmetric_and_weighted_duplicates_preserve_incoming_ownership() {
        isolated(
            "symmetric_and_weighted_duplicates_preserve_incoming_ownership",
            || {
                let g = ugraph(7, &[(0, 3), (0, 3), (1, 3), (1, 4), (2, 4)]);
                let ctl = Control::unlimited();
                let incoming = Arc::new(AtomicU64::new(0));
                let observed = incoming.clone();
                let hook = probe::install(&ctl, move |event, _| {
                    if let probe::Event::Admission { name, .. } = event
                        && name.starts_with("incoming-")
                    {
                        observed.fetch_add(1, Ordering::Relaxed);
                    }
                });
                let got = run(&g, &Params::default(), &Par::with_threads(4), &ctl).unwrap();
                drop(hook);
                assert_eq!(bits(&got), bits(&oracle(&g, &Params::default()).0));
                assert_eq!(incoming.load(Ordering::Relaxed), 0);
                let edges = [(0, 3), (0, 3), (0, 4), (1, 3), (1, 4)];
                let weighted = Graph::build(
                    super::super::RawEdges {
                        nodes: (0..6).collect(),
                        src: edges.iter().map(|e| e.0).collect(),
                        dst: edges.iter().map(|e| e.1).collect(),
                        w: Some(vec![9., 1., 2., 3., 4.]),
                        symmetric: false,
                    },
                    super::super::Aggregate::None,
                    &Par::single(),
                    &Control::unlimited(),
                )
                .unwrap();
                let got = run(
                    &weighted,
                    &Params::default(),
                    &Par::with_threads(2),
                    &Control::unlimited(),
                )
                .unwrap();
                assert_eq!(bits(&got), bits(&oracle(&weighted, &Params::default()).0));
                let g = digraph(5, &[(0, 3), (0, 4), (1, 3), (1, 4), (2, 3)]);
                for _ in 0..2 {
                    let ctl = Control::unlimited();
                    let output = run(&g, &Params::default(), &Par::single(), &ctl).unwrap();
                    assert_eq!(bits(&output), bits(&oracle(&g, &Params::default()).0));
                    assert_eq!(
                        u64::MAX - ctl.remaining(),
                        memory::bytes::<Pair>(output.capacity()).unwrap()
                    );
                }
            },
        );
    }
}
