//! The crash tests of X09 sections 10.2 and 10.4 (docs/specs/X09-group-commit-by-default.md):
//! a write load that logs every acknowledged receipt, a replay of a device-mapper
//! `log-writes` log into crash images, and the checks each recovered store must pass.
//! scripts/power-loss.sh drives them. Build (std only, one file):
//!
//!     rustc --edition 2024 -O -o crash-harness scripts/crash-harness.rs
//!
//! Commands
//!
//!   drive --addr A --ds D --receipts FILE [--clients 16] [--seconds S] [--run R]
//!         [--message-every N] [--mark-cmd CMD] [--mark-ms 50] [--allow-empty]
//!       `--clients` writers insert one new triple per request, `<urn:x09:R:W:K>
//!       <urn:x09:p> "v-R-W-K"`, every `--message-every`th with a commit message, until
//!       `--seconds` pass or the server stops answering. Each acknowledged receipt (the
//!       `Sparkles-Commit` header of a 200 answer) is appended to FILE as
//!       `R <commit> <subject> <object> <message or ->`. With `--mark-cmd`, every
//!       `--mark-ms` the receipts acknowledged so far are written and synced, the line
//!       `M <name> <receipts so far>` follows, and then `CMD <name>` runs (for example
//!       `dmsetup message lw 0 mark`). Every receipt counted in a mark was acknowledged
//!       before the mark reached the log, so it must survive a crash at any later point.
//!       Prints `{"receipts": N, "failed": F}`, with `"marks"` and `"failedMarks"` under
//!       `--mark-cmd`. A 4xx answer or a 507 ends the run with status 2. Another 5xx ends
//!       that writer and counts in F. The status is 3 when F is above 0, when no receipt was
//!       acknowledged and `--allow-empty` is not given, or under `--mark-cmd` when a mark
//!       failed or none was made.
//!
//!   verify --sparkles BIN --loc DIR --receipts FILE [--mark NAME] [--write]
//!       Checks a recovered store: `sparkles check` reports no error before and after the
//!       store is opened; the head is at or after the last receipt counted by mark NAME
//!       (every receipt in the file without `--mark`); every receipt at or below the head
//!       has its triple and, for a message, its message; with `--write`, a new commit
//!       continues the sequence at head + 1. Prints one JSON line and exits 1 on failure.
//!
//!   replay --log FILE --size BYTES --out DIR --points N [--seed S] [--from-mark NAME]
//!         [--jobs J] [--keep-failed K] [--only N,...] -- CMD...
//!       Reads a `log-writes` log (kernel Documentation/admin-guide/device-mapper/
//!       log-writes.rst) and builds N crash images of the logged device, at random entries
//!       after mark NAME. An image holds every write logged before the last flush at or
//!       before its entry and every FUA write up to it. Each other write logged between
//!       that flush and the entry is kept or dropped per 4 KiB block, at random with a
//!       probability drawn per image. For each image, `CMD IMAGE MARK` runs, where MARK is
//!       the last mark before the entry, and its exit status and output are recorded in
//!       DIR/results.tsv. `--keep-failed K` keeps the images of the first K failing points
//!       as DIR/fail-N.img, as the command left them. `--only N,...` builds and checks only
//!       those points, the same images that a full run with the same seed builds.
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::FileExt;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn die(m: impl std::fmt::Display) -> ! {
    eprintln!("crash-harness: {m}");
    std::process::exit(2)
}

struct Args(Vec<String>);

impl Args {
    fn get(&self, name: &str) -> Option<String> {
        let i = self.0.iter().position(|a| a == name)?;
        Some(
            self.0
                .get(i + 1)
                .unwrap_or_else(|| die(format!("{name} needs a value")))
                .clone(),
        )
    }
    fn req(&self, name: &str) -> String {
        self.get(name)
            .unwrap_or_else(|| die(format!("{name} is required")))
    }
    fn num(&self, name: &str, default: u64) -> u64 {
        self.get(name)
            .map(|v| {
                v.parse()
                    .unwrap_or_else(|_| die(format!("{name}: not a number")))
            })
            .unwrap_or(default)
    }
    fn flag(&self, name: &str) -> bool {
        self.0.iter().any(|a| a == name)
    }
}

/// A small deterministic generator (SplitMix64).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

// ------------------------------------------------------------------ drive ------

/// One request on its own connection: the status and the `Sparkles-Commit` header.
fn post(
    addr: &str,
    ds: &str,
    update: &str,
    message: Option<&str>,
) -> Result<(u16, Option<u64>), String> {
    let mut s = TcpStream::connect(addr).map_err(|e| format!("connect: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(60))).ok();
    let message = message
        .map(|m| format!("Sparkles-Commit-Message: {m}\r\n"))
        .unwrap_or_default();
    let req = format!(
        "POST /{ds}/update HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n{message}Content-Type: application/sparql-update\r\nContent-Length: {}\r\n\r\n{update}",
        update.len()
    );
    s.write_all(req.as_bytes())
        .map_err(|e| format!("send: {e}"))?;
    let mut out = Vec::new();
    s.read_to_end(&mut out).map_err(|e| format!("read: {e}"))?;
    let text = String::from_utf8_lossy(&out);
    let head = text.split("\r\n\r\n").next().unwrap_or("");
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or("no status line")?;
    let commit = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("sparkles-commit"))
        .and_then(|(_, v)| v.trim().parse().ok());
    Ok((status, commit))
}

fn drive(a: &Args) {
    let addr = a.req("--addr");
    let ds = a.req("--ds");
    let path = a.req("--receipts");
    let clients = a.num("--clients", 16);
    let seconds = a.num("--seconds", 20);
    let run = a.get("--run").unwrap_or_else(|| "0".into());
    let every = a.num("--message-every", 4);
    let mark_cmd = a.get("--mark-cmd");
    let marking = mark_cmd.is_some();
    let mark_ms = a.num("--mark-ms", 50);
    let allow_empty = a.flag("--allow-empty");
    let file = Arc::new(Mutex::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap_or_else(|e| die(format!("{path}: {e}"))),
    ));
    let acked: Arc<Mutex<Vec<String>>> = Default::default();
    let written = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let failed = Arc::new(AtomicUsize::new(0));
    let (marks_ok, marks_failed) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    // write the receipts acknowledged so far, synced, and return how many there are
    let flush = {
        let (file, acked, written) = (file.clone(), acked.clone(), written.clone());
        move |extra: Option<&str>| -> usize {
            let mut f = file.lock().unwrap();
            let a = acked.lock().unwrap();
            let from = written.load(Ordering::SeqCst);
            let mut out = String::new();
            for r in &a[from..] {
                out.push_str(r);
                out.push('\n');
            }
            let n = a.len();
            if let Some(name) = extra {
                out.push_str(&format!("M {name} {n}\n"));
            }
            f.write_all(out.as_bytes()).unwrap();
            f.sync_data().unwrap();
            written.store(n, Ordering::SeqCst);
            n
        }
    };
    let flush = Arc::new(flush);
    let mut handles = Vec::new();
    for w in 0..clients {
        let (addr, ds, run, acked, stop, failed) = (
            addr.clone(),
            ds.clone(),
            run.clone(),
            acked.clone(),
            stop.clone(),
            failed.clone(),
        );
        handles.push(std::thread::spawn(move || {
            let mut k = 0u64;
            while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                let s = format!("urn:x09:{run}:{w}:{k}");
                let o = format!("v-{run}-{w}-{k}");
                let m = (every > 0 && k % every == 0).then(|| format!("m-{run}-{w}-{k}"));
                let update = format!("INSERT DATA {{ <{s}> <urn:x09:p> \"{o}\" }}");
                match post(&addr, &ds, &update, m.as_deref()) {
                    Ok((200, Some(c))) => acked
                        .lock()
                        .unwrap()
                        .push(format!("R {c} {s} {o} {}", m.as_deref().unwrap_or("-"))),
                    // 507 is the server's free-space reserve refusing every write, a
                    // setup error that would make the whole run vacuous
                    Ok((status, _)) if status >= 500 && status != 507 => {
                        failed.fetch_add(1, Ordering::Relaxed);
                        break;
                    }
                    Ok((status, c)) => die(format!("{s}: HTTP {status}, receipt {c:?}")),
                    // the server stopped: this request's outcome is unknown
                    Err(_) => break,
                }
                k += 1;
            }
        }));
    }
    let marker = mark_cmd.map(|cmd| {
        let (stop, flush) = (stop.clone(), flush.clone());
        let (marks_ok, marks_failed) = (marks_ok.clone(), marks_failed.clone());
        std::thread::spawn(move || {
            let mut n = 0u64;
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(mark_ms));
                let name = format!("m{n}");
                flush(Some(&name));
                let mut parts = cmd.split_whitespace();
                let prog = parts.next().unwrap();
                let ok = Command::new(prog)
                    .args(parts)
                    .arg(&name)
                    .status()
                    .is_ok_and(|s| s.success());
                if ok {
                    marks_ok.fetch_add(1, Ordering::SeqCst);
                } else {
                    eprintln!("crash-harness: mark {name} failed");
                    marks_failed.fetch_add(1, Ordering::SeqCst);
                }
                n += 1;
            }
        })
    });
    for h in handles {
        h.join().unwrap();
    }
    stop.store(true, Ordering::SeqCst);
    if let Some(m) = marker {
        m.join().unwrap();
    }
    let n = flush(None);
    let failed = failed.load(Ordering::Relaxed);
    let (marks, marks_failed) = (
        marks_ok.load(Ordering::SeqCst),
        marks_failed.load(Ordering::SeqCst),
    );
    if marking {
        println!(
            "{{\"receipts\": {n}, \"failed\": {failed}, \"marks\": {marks}, \"failedMarks\": {marks_failed}}}"
        );
    } else {
        println!("{{\"receipts\": {n}, \"failed\": {failed}}}");
    }
    // A run without receipts checks nothing, and a failed write is not a crash outcome. A
    // failed mark leaves receipts that no image is checked against, and a run without
    // marks checks every image against no receipts at all.
    let unmarked = marking && (marks_failed > 0 || marks == 0);
    if failed > 0 || (n == 0 && !allow_empty) || unmarked {
        eprintln!(
            "crash-harness: {n} receipts, {failed} failed writes, {marks} marks, {marks_failed} failed marks"
        );
        std::process::exit(3);
    }
}

// ----------------------------------------------------------------- verify ------

struct Receipt {
    commit: u64,
    subject: String,
    object: String,
    message: Option<String>,
}

/// The receipts of a file, and how many each mark counts.
fn read_receipts(path: &str) -> (Vec<Receipt>, BTreeMap<String, usize>) {
    let f = File::open(path).unwrap_or_else(|e| die(format!("{path}: {e}")));
    let (mut out, mut marks) = (Vec::new(), BTreeMap::new());
    for line in BufReader::new(f).lines() {
        let line = line.unwrap();
        let p: Vec<&str> = line.split(' ').collect();
        match p.as_slice() {
            ["R", c, s, o, m] => out.push(Receipt {
                commit: c.parse().unwrap(),
                subject: s.to_string(),
                object: o.to_string(),
                message: (*m != "-").then(|| m.to_string()),
            }),
            ["M", name, n] => {
                marks.insert(name.to_string(), n.parse().unwrap());
            }
            _ if line.is_empty() => {}
            _ => die(format!("{path}: bad line {line:?}")),
        }
    }
    (out, marks)
}

fn sparkles(bin: &str, args: &[&str]) -> (i32, String, String) {
    let o = Command::new(bin)
        .args(args)
        .output()
        .unwrap_or_else(|e| die(format!("{bin}: {e}")));
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn verify(a: &Args) {
    let bin = a.req("--sparkles");
    let loc = a.req("--loc");
    let path = a.req("--receipts");
    let (receipts, marks) = read_receipts(&path);
    let bound = match a.get("--mark") {
        Some(m) if m == "-" => 0,
        Some(m) => *marks
            .get(&m)
            .unwrap_or_else(|| die(format!("no mark {m} in {path}"))),
        None => receipts.len(),
    };
    let mut problems: Vec<String> = Vec::new();
    // before the store is opened: a torn tail is a warning, never an error
    let (code, out, err) = sparkles(&bin, &["check", "--loc", &loc, "--format", "json"]);
    if code != 0 && code != 2 {
        problems.push(format!(
            "check before open exited {code}: {}{}",
            whole(&out),
            tail(&err)
        ));
    }
    let q = "SELECT ?s ?o WHERE { ?s <urn:x09:p> ?o }";
    let (code, out, err) = sparkles(&bin, &["query", "--loc", &loc, "--results", "tsv", q]);
    if code != 0 {
        problems.push(format!("query exited {code}: {}", tail(&err)));
    }
    let mut present = std::collections::HashMap::new();
    for l in out.lines().skip(1) {
        if let Some((s, o)) = l.split_once('\t') {
            let s = s.trim_start_matches('<').trim_end_matches('>');
            let o = o.trim_matches('"');
            present.insert(s.to_string(), o.to_string());
        }
    }
    // the head and each commit's message, from the catalog and annotations
    let history = |problems: &mut Vec<String>| -> (u64, std::collections::HashMap<u64, String>) {
        let (code, out, err) = sparkles(&bin, &["log", "--loc", &loc, "--limit", "100000000"]);
        let mut messages = std::collections::HashMap::new();
        if code != 0 {
            problems.push(format!("log exited {code}: {}", tail(&err)));
            return (0, messages);
        }
        let mut lines = out.lines();
        let head = lines
            .next()
            .and_then(|l| l.split("head ").nth(1))
            .and_then(|h| h.split_whitespace().next())
            .and_then(|h| h.parse().ok())
            .unwrap_or_else(|| {
                problems.push(format!("no head in the log: {}", tail(&out)));
                0
            });
        for l in lines.skip(1) {
            let mut t = l.split_whitespace();
            let Some(seq) = t.next().and_then(|s| s.trim_end_matches('*').parse().ok()) else {
                continue;
            };
            if let Some(m) = l.split_whitespace().last().filter(|m| m.starts_with("m-")) {
                messages.insert(seq, m.to_string());
            }
        }
        (head, messages)
    };
    let (head, messages) = history(&mut problems);
    let need = receipts[..bound]
        .iter()
        .map(|r| r.commit)
        .max()
        .unwrap_or(0);
    if head < need {
        problems.push(format!("head {head} is before acknowledged commit {need}"));
    }
    let mut checked = 0usize;
    for r in receipts.iter().filter(|r| r.commit <= head) {
        checked += 1;
        if present.get(&r.subject) != Some(&r.object) {
            problems.push(format!(
                "commit {} lost its triple <{}>",
                r.commit, r.subject
            ));
        }
        if let Some(m) = &r.message
            && messages.get(&r.commit) != Some(m)
        {
            problems.push(format!("commit {} lost its message {m}", r.commit));
        }
        if problems.len() > 20 {
            break;
        }
    }
    let mut next = None;
    if a.flag("--write") && problems.is_empty() {
        let update = format!("INSERT DATA {{ <urn:x09:after:{head}> <urn:x09:p> \"after\" }}");
        let (code, out, err) = sparkles(&bin, &["update", "--loc", &loc, &update]);
        let (seq, _) = history(&mut problems);
        next = Some(seq);
        if code != 0 || seq != head + 1 {
            problems.push(format!(
                "the next commit is {seq}, not {} (update exited {code}: {}{})",
                head + 1,
                tail(&out),
                tail(&err)
            ));
        }
    }
    // After open only leftovers may remain: a newer generation of a compaction that a
    // crash interrupted stays until the next rebuild replaces it.
    let (code, out, err) = sparkles(&bin, &["check", "--loc", &loc, "--format", "json"]);
    if code != 0 && !(code == 2 && only_leftovers(&out)) {
        problems.push(format!(
            "check after open exited {code}: {}{}",
            whole(&out),
            tail(&err)
        ));
    }
    let ok = problems.is_empty();
    println!(
        "{{\"ok\": {ok}, \"head\": {head}, \"needed\": {need}, \"bound\": {bound}, \"checked\": {checked}, \"next\": {}, \"problems\": {:?}}}",
        next.map_or("null".to_string(), |n| n.to_string()),
        problems
    );
    std::process::exit(if ok { 0 } else { 1 });
}

/// Whether every issue in a `sparkles check --format json` report is a warning about a
/// leftover file, such as a generation newer than CURRENT. Each issue object names its
/// status before its message.
fn only_leftovers(json: &str) -> bool {
    let mut any = false;
    for (i, _) in json.match_indices("\"message\": \"") {
        let status = json[..i]
            .rfind("\"status\": \"")
            .and_then(|j| json[j + 11..].split('"').next());
        if status != Some("warning") || !json[i + 12..].starts_with("leftover:") {
            return false;
        }
        any = true;
    }
    any
}

// all of a failed check's report, so that its finding is never cut off
fn whole(s: &str) -> String {
    s.trim().replace('\n', " | ")
}

fn tail(s: &str) -> String {
    let s = s.trim();
    let start = s.len().saturating_sub(400);
    let start = (start..=s.len())
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(s.len());
    s[start..].replace('\n', " | ")
}

// ----------------------------------------------------------------- replay ------

const FLUSH: u64 = 1;
const FUA: u64 = 2;
const DISCARD: u64 = 4;
const MARK: u64 = 8;
const MAGIC: u64 = 0x6a73_6677_7368_72;

struct Entry {
    /// the first device sector written, in 512-byte sectors
    sector: u64,
    sectors: u64,
    flags: u64,
    /// where the data starts in the log file
    data: u64,
    mark: Option<String>,
}

/// The entries of a `log-writes` log.
fn read_log(path: &str) -> (File, Vec<Entry>) {
    let mut f = File::open(path).unwrap_or_else(|e| die(format!("{path}: {e}")));
    let mut sup = [0u8; 28];
    f.read_exact(&mut sup).unwrap();
    let le = |b: &[u8]| u64::from_le_bytes(b[..8].try_into().unwrap());
    if le(&sup[0..]) != MAGIC {
        die(format!("{path}: not a log-writes log"));
    }
    if le(&sup[8..]) != 1 {
        die(format!(
            "{path}: log-writes version {} is not 1",
            le(&sup[8..])
        ));
    }
    let count = le(&sup[16..]);
    let sectorsize = u32::from_le_bytes(sup[24..28].try_into().unwrap()) as u64;
    // entries count device sectors in units of the log's sector size
    let unit = sectorsize / 512;
    let mut at = sectorsize;
    let mut out = Vec::with_capacity(count as usize);
    let mut head = vec![0u8; sectorsize as usize];
    for _ in 0..count {
        f.read_exact_at(&mut head, at).unwrap();
        let (sector, nr, flags, len) = (
            le(&head[0..]),
            le(&head[8..]),
            le(&head[16..]),
            le(&head[24..]),
        );
        let mark = (flags & MARK != 0)
            .then(|| String::from_utf8_lossy(&head[32..32 + len as usize]).into_owned());
        out.push(Entry {
            sector: sector * unit,
            sectors: nr * unit,
            flags,
            data: at + sectorsize,
            mark,
        });
        at += sectorsize;
        if flags & (DISCARD | MARK) == 0 {
            at += nr * sectorsize;
        }
    }
    (f, out)
}

/// Apply entry `e` of `log` to the image `img`, all of it or the 4 KiB blocks `keep`
/// chooses.
fn apply(log: &File, img: &File, e: &Entry, keep: &mut dyn FnMut() -> bool) {
    const BLOCK: u64 = 4096;
    let (start, len) = (e.sector * 512, e.sectors * 512);
    if e.flags & MARK != 0 || len == 0 {
        return;
    }
    let mut buf = vec![0u8; len as usize];
    if e.flags & DISCARD == 0 {
        log.read_exact_at(&mut buf, e.data).unwrap();
    }
    let mut off = 0u64;
    while off < len {
        let end = ((start + off) / BLOCK + 1) * BLOCK - start;
        let end = end.min(len);
        if keep() {
            img.write_all_at(&buf[off as usize..end as usize], start + off)
                .unwrap();
        }
        off = end;
    }
}

fn replay(a: &Args, cmd: Vec<String>) {
    let path = a.req("--log");
    let size = a.num("--size", 0);
    let out = a.req("--out");
    let points = a.num("--points", 1000) as usize;
    let mut rng = Rng(a.num("--seed", 1));
    let jobs = a.num("--jobs", 4) as usize;
    let keep_failed = a.num("--keep-failed", 0) as usize;
    let only: Option<Vec<usize>> = a.get("--only").map(|l| {
        l.split(',')
            .map(|n| {
                n.trim()
                    .parse()
                    .unwrap_or_else(|_| die(format!("bad --only {l}")))
            })
            .collect()
    });
    if cmd.is_empty() {
        die("replay needs a command after --");
    }
    std::fs::create_dir_all(&out).unwrap();
    let (log, entries) = read_log(&path);
    let from = match a.get("--from-mark") {
        Some(m) => entries
            .iter()
            .position(|e| e.mark.as_deref() == Some(m.as_str()))
            .unwrap_or_else(|| die(format!("no mark {m} in the log"))),
        None => 0,
    };
    let mut chosen: Vec<usize> = (0..points)
        .map(|_| from + rng.below((entries.len() - from) as u64) as usize)
        .collect();
    chosen.sort_unstable();
    eprintln!(
        "crash-harness: {} entries ({} flushes, {} fua, {} marks), {points} points after entry {from}",
        entries.len(),
        entries.iter().filter(|e| e.flags & FLUSH != 0).count(),
        entries.iter().filter(|e| e.flags & FUA != 0).count(),
        entries.iter().filter(|e| e.mark.is_some()).count()
    );
    // the image of every write before the last flush, advanced point by point
    let base_path = format!("{out}/base.img");
    let base = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&base_path)
        .unwrap();
    base.set_len(size).unwrap();
    let mut applied = 0usize;
    let results = Arc::new(Mutex::new(
        File::create(format!("{out}/results.tsv")).unwrap(),
    ));
    let running: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>> = Default::default();
    let fails = Arc::new(AtomicUsize::new(0));
    for (n, &p) in chosen.iter().enumerate() {
        // writes before the last flush at or before p are durable
        let flush = (0..=p)
            .rev()
            .find(|&i| entries[i].flags & FLUSH != 0)
            .unwrap_or(0);
        while applied < flush {
            apply(&log, &base, &entries[applied], &mut || true);
            applied += 1;
        }
        let mark = entries[..=p]
            .iter()
            .rev()
            .find_map(|e| e.mark.clone())
            .unwrap_or_else(|| "-".into());
        // drawn for every point, so that --only rebuilds the same images
        let keep_p = rng.unit();
        let mut r = Rng(rng.next());
        if only.as_ref().is_some_and(|o| !o.contains(&n)) {
            continue;
        }
        let img_path = format!("{out}/img-{n}.img");
        base.sync_data().ok();
        std::fs::copy(&base_path, &img_path).unwrap();
        let img = OpenOptions::new().write(true).open(&img_path).unwrap();
        for e in &entries[flush..=p] {
            if e.flags & FUA != 0 {
                apply(&log, &img, e, &mut || true);
            } else {
                apply(&log, &img, e, &mut || r.unit() < keep_p);
            }
        }
        drop(img);
        // run the check, at most `jobs` at a time
        loop {
            let mut r = running.lock().unwrap();
            r.retain(|h| !h.is_finished());
            if r.len() < jobs {
                break;
            }
            drop(r);
            std::thread::sleep(Duration::from_millis(20));
        }
        let (cmd, results, fails) = (cmd.clone(), results.clone(), fails.clone());
        let h = std::thread::spawn(move || {
            let o = Command::new(&cmd[0])
                .args(&cmd[1..])
                .arg(&img_path)
                .arg(&mark)
                .output()
                .unwrap_or_else(|e| die(format!("{}: {e}", cmd[0])));
            let ok = o.status.success();
            // the image of one of the first --keep-failed failures stays as fail-N.img
            let mut kept = false;
            if !ok && fails.fetch_add(1, Ordering::SeqCst) < keep_failed {
                kept = std::fs::rename(&img_path, img_path.replace("img-", "fail-")).is_ok();
            }
            let text = String::from_utf8_lossy(&o.stdout).replace(['\n', '\t'], " ");
            let err = String::from_utf8_lossy(&o.stderr).replace(['\n', '\t'], " ");
            writeln!(
                results.lock().unwrap(),
                "{n}\t{p}\t{flush}\t{mark}\t{keep_p:.3}\t{}\t{}\t{}",
                if ok { "ok" } else { "FAIL" },
                text.trim(),
                err.trim()
            )
            .unwrap();
            if !kept {
                let _ = std::fs::remove_file(&img_path);
            }
        });
        running.lock().unwrap().push(h);
    }
    for h in std::mem::take(&mut *running.lock().unwrap()) {
        h.join().unwrap();
    }
    let _ = std::fs::remove_file(&base_path);
    let fails = fails.load(Ordering::SeqCst);
    println!("{{\"points\": {points}, \"failed\": {fails}}}");
    std::process::exit(if fails == 0 { 0 } else { 1 });
}

fn main() {
    let mut all: Vec<String> = std::env::args().skip(1).collect();
    if all.is_empty() {
        die("usage: crash-harness drive|verify|replay ... (see the file's header)");
    }
    let sub = all.remove(0);
    let (args, cmd) = match all.iter().position(|a| a == "--") {
        Some(i) => (all[..i].to_vec(), all[i + 1..].to_vec()),
        None => (all, Vec::new()),
    };
    let a = Args(args);
    match sub.as_str() {
        "drive" => drive(&a),
        "verify" => verify(&a),
        "replay" => replay(&a, cmd),
        _ => die(format!("unknown command {sub}")),
    }
}
