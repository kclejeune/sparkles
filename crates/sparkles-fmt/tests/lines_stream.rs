//! Streaming N-Triples and N-Quads: [`format_lines`] prints what [`format`] prints, in
//! any chunking and on any number of threads; sorting past the memory budget spills
//! sorted runs and gives the in-memory order; the spill files go on success, on errors
//! and when the run is cancelled (a failing reader, a passed deadline); canonicalizing
//! isomorphic inputs gives the same bytes; memory stays bounded on a large input
//! (measured by a counting allocator in a process of its own); and generated documents
//! (statements, comments, blank lines, pragmas, `VERSION` lines, both line breaks)
//! format to fixpoints that keep their quads and comments.

use proptest::prelude::*;
use sparkles_fmt::check::{comments, graph};
use sparkles_fmt::lex::LexMode;
use sparkles_fmt::lines::format_stream_chunked;
use sparkles_fmt::{FormatError, Language, LinesConfig, Options, format, format_lines};
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

// ------------------------------------------------------------- the allocator ------

/// The system allocator, counting the bytes in use and their peak.
struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(by: usize) {
    let now = CURRENT.fetch_add(by, Ordering::Relaxed) + by;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
            grew(new_size);
        }
        p
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

// ------------------------------------------------------------- the inputs ------

/// A generated N-Triples document, produced as it is read: `lines` lines of statements
/// (some duplicated, some with blank nodes and comments).
struct Generated {
    next: u64,
    lines: u64,
    buf: Vec<u8>,
    at: usize,
}

impl Generated {
    fn new(lines: u64) -> Generated {
        Generated {
            next: 0,
            lines,
            buf: Vec::new(),
            at: 0,
        }
    }

    fn line(i: u64, out: &mut Vec<u8>) {
        // a scrambled order, so sorting moves lines a long way
        let k = i.wrapping_mul(2_654_435_761) % 1_000_003;
        let _ = match i % 10 {
            0 => writeln!(out, "# section {i}"),
            1 => writeln!(out, "_:b{k} <http://example.org/p>  \"blank {k}\"@EN ."),
            2 => writeln!(
                out,
                "<http://example.org/s{}> <http://example.org/p> \"dup\" .",
                k % 100
            ),
            3 => writeln!(
                out,
                "<http://example.org/s{k}> <http://example.org/q> \"caf\\u00E9 {k}\" . # c{i}"
            ),
            _ => writeln!(
                out,
                "<http://example.org/s{k}>\t<http://example.org/p{}> \"value {i}\"^^<http://www.w3.org/2001/XMLSchema#string> .",
                i % 7
            ),
        };
    }
}

impl Read for Generated {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.at == self.buf.len() {
            self.buf.clear();
            self.at = 0;
            while self.buf.len() < 64 << 10 && self.next < self.lines {
                Generated::line(self.next, &mut self.buf);
                self.next += 1;
            }
        }
        let n = out.len().min(self.buf.len() - self.at);
        out[..n].copy_from_slice(&self.buf[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

fn generated(lines: u64) -> String {
    let mut s = String::new();
    Generated::new(lines).read_to_string(&mut s).unwrap();
    s
}

fn opts(sort: bool, canonicalize: bool) -> Options {
    Options {
        sort,
        canonicalize,
        ..Options::default()
    }
}

/// An empty directory for spill files, unique to the test.
fn spill_parent(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "sparkles-fmt-lines-stream-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn entries(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |d| d.count())
}

// ------------------------------------------------------------- the tests ------

#[test]
fn streaming_prints_what_formatting_in_memory_prints() {
    let mut docs: Vec<(Language, String)> = Vec::new();
    for lang in [Language::NTriples, Language::NQuads] {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(lang.name());
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            if e.file_name().to_string_lossy().contains(".in.") {
                docs.push((lang, std::fs::read_to_string(e.path()).unwrap()));
            }
        }
    }
    assert!(!docs.is_empty(), "no line-format golden inputs");
    docs.push((Language::NTriples, generated(3_000)));
    for (lang, text) in &docs {
        for o in [opts(false, false), opts(true, false), opts(false, true)] {
            let expected = format(text, *lang, &o).unwrap();
            for (chunk, threads) in [(1, 1), (13, 3), (4096, 2), (1 << 20, 0)] {
                let mut out = Vec::new();
                let cfg = LinesConfig {
                    sort_memory: 1 << 10,
                    threads,
                    ..LinesConfig::default()
                };
                let stats =
                    format_stream_chunked(text.as_bytes(), &mut out, *lang, &o, &cfg, chunk)
                        .unwrap();
                assert_eq!(
                    String::from_utf8(out).unwrap(),
                    expected.text,
                    "chunks of {chunk}, {threads} threads, {o:?}"
                );
                assert_eq!(stats.changed, expected.changed);
                assert_eq!(stats.warnings, expected.warnings);
            }
        }
    }
}

#[test]
fn sorting_spills_past_its_budget_and_cleans_up() {
    let text = generated(50_000);
    let o = opts(true, false);
    let in_memory = format(&text, Language::NTriples, &o).unwrap().text;
    let parent = spill_parent("spills");
    let cfg = LinesConfig {
        spill_dir: parent.clone(),
        sort_memory: 4 << 10,
        threads: 2,
        ..LinesConfig::default()
    };
    let mut out = Vec::new();
    let stats = format_lines(text.as_bytes(), &mut out, Language::NTriples, &o, &cfg).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), in_memory);
    assert!(stats.changed);
    assert_eq!(stats.statements, 45_000);
    assert_eq!(entries(&parent), 0, "spill files left behind");
    // it does spill: a spill directory that cannot be made fails the run
    let missing = LinesConfig {
        spill_dir: parent.join("missing/deeper"),
        ..cfg.clone()
    };
    assert!(
        format_lines(
            text.as_bytes(),
            io::sink(),
            Language::NTriples,
            &o,
            &missing
        )
        .is_err()
    );
    // within the budget it does not
    let roomy = LinesConfig {
        spill_dir: parent.join("missing/deeper"),
        sort_memory: 1 << 30,
        ..cfg
    };
    assert!(format_lines(text.as_bytes(), io::sink(), Language::NTriples, &o, &roomy).is_ok());
    std::fs::remove_dir_all(&parent).unwrap();
}

/// A reader of `text` that, after `fail_after` bytes, checks that spill files exist and
/// then fails or waits past a deadline.
struct Interrupted<'a> {
    text: &'a [u8],
    at: usize,
    fail_after: usize,
    parent: &'a Path,
    sleep: Option<Duration>,
    saw_spills: bool,
}

impl Read for Interrupted<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.at >= self.fail_after {
            self.saw_spills = entries(self.parent) > 0;
            match self.sleep.take() {
                Some(d) => std::thread::sleep(d),
                None if self.fail_after < self.text.len() => {
                    return Err(io::Error::other("the connection went away"));
                }
                None => {}
            }
            self.fail_after = usize::MAX;
        }
        let end = self
            .text
            .len()
            .min(self.at + out.len())
            .min(self.fail_after);
        let n = end - self.at;
        out[..n].copy_from_slice(&self.text[self.at..end]);
        self.at = end;
        Ok(n)
    }
}

#[test]
fn spill_files_go_on_errors_and_cancellation() {
    let mut text = generated(50_000);
    let o = opts(true, false);
    let parent = spill_parent("errors");
    let cfg = LinesConfig {
        spill_dir: parent.clone(),
        sort_memory: 4 << 10,
        threads: 1,
        ..LinesConfig::default()
    };
    let half = text.len() / 2;

    // the reader fails half way
    let mut r = Interrupted {
        text: text.as_bytes(),
        at: 0,
        fail_after: half,
        parent: &parent,
        sleep: None,
        saw_spills: false,
    };
    let e = format_lines(
        BufReader::new(&mut r),
        io::sink(),
        Language::NTriples,
        &o,
        &cfg,
    );
    assert!(e.is_err());
    assert!(r.saw_spills, "nothing spilled before the failure");
    assert_eq!(entries(&parent), 0, "spill files left after a read error");

    // the deadline passes half way
    let o_late = Options {
        deadline: Some(Instant::now() + Duration::from_millis(1500)),
        ..o.clone()
    };
    let mut r = Interrupted {
        text: text.as_bytes(),
        at: 0,
        fail_after: half,
        parent: &parent,
        sleep: Some(Duration::from_millis(2000)),
        saw_spills: false,
    };
    let e = format_lines(
        BufReader::new(&mut r),
        io::sink(),
        Language::NTriples,
        &o_late,
        &cfg,
    );
    assert_eq!(e.unwrap_err(), FormatError::Timeout);
    assert!(r.saw_spills);
    assert_eq!(entries(&parent), 0, "spill files left after a timeout");

    // the output fails (sorted output is written once the input is read)
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    assert!(format_lines(text.as_bytes(), Broken, Language::NTriples, &o, &cfg).is_err());
    assert_eq!(entries(&parent), 0, "spill files left after a write error");

    // a syntax error at the very end
    text.push_str("<http://example.org/s> <http://example.org/p> .\n");
    let e = format_lines(text.as_bytes(), io::sink(), Language::NTriples, &o, &cfg);
    assert!(
        matches!(e, Err(FormatError::Syntax { line: 50_001, .. })),
        "{e:?}"
    );
    assert_eq!(entries(&parent), 0, "spill files left after a syntax error");
    std::fs::remove_dir_all(&parent).unwrap();
}

#[test]
fn canonical_form_does_not_depend_on_labels_or_order() {
    let a = "# two people who know each other\n\
             _:alice <http://xmlns.com/foaf/0.1/knows> _:bob <http://example.org/g> .\n\
             _:bob <http://xmlns.com/foaf/0.1/knows> _:alice <http://example.org/g> .\n\
             _:alice <http://xmlns.com/foaf/0.1/name> \"Alice\" .\n\
             _:bob <http://xmlns.com/foaf/0.1/name> \"Bob\"   .\n";
    let b = "_:n2 <http://xmlns.com/foaf/0.1/name> \"Bob\" .\n\
             _:n1 <http://xmlns.com/foaf/0.1/knows> _:n2 <http://example.org/g> .\n\n\
             _:n1 <http://xmlns.com/foaf/0.1/name> \"Alice\" .\n\
             _:n2 <http://xmlns.com/foaf/0.1/knows> _:n1 <http://example.org/g> . # dup below\n\
             _:n2 <http://xmlns.com/foaf/0.1/knows> _:n1 <http://example.org/g> .\n";
    let o = opts(false, true);
    let fa = format(a, Language::NQuads, &o).unwrap();
    let fb = format(b, Language::NQuads, &o).unwrap();
    assert_eq!(fa.text, fb.text);
    assert_eq!(fa.text.lines().count(), 4);
    assert!(fa.text.contains("_:c14n0") && fa.text.contains("_:c14n1"));
    let mut sorted: Vec<&str> = fa.text.lines().collect();
    sorted.sort_unstable();
    assert_eq!(sorted, fa.text.lines().collect::<Vec<_>>());
    // streamed: the same bytes
    let mut out = Vec::new();
    format_lines(
        b.as_bytes(),
        &mut out,
        Language::NQuads,
        &o,
        &LinesConfig::default(),
    )
    .unwrap();
    assert_eq!(out, fa.text.as_bytes());
}

/// The output with `VERSION` lines left out (oxttl does not read them).
fn without_versions(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with("VERSION"))
        .map(|l| format!("{l}\n"))
        .collect()
}

fn statement() -> impl Strategy<Value = String> {
    let subject = prop::sample::select(vec!["<http://e/a>", "<http://e/b>", "_:x", "_:y"]);
    let object = prop::sample::select(vec![
        "<http://e/o>",
        "\"lit\"",
        "\"Lit\"@EN-gb",
        "\"x\"^^<http://www.w3.org/2001/XMLSchema#string>",
        "\"caf\\u00e9\\t\"",
        "<<( _:x <http://e/p> \"t\" )>>",
        "_:y",
    ]);
    let ws = prop::sample::select(vec![" ", "  ", "\t", " \t "]);
    let comment = prop::option::of((0..50u32, prop::sample::select(vec!["", " ", "   "])));
    (subject, ws.clone(), object, ws, comment).prop_map(|(s, w1, o, w2, c)| {
        let mut l = format!("{s}{w1}<http://e/p>{w2}{o} .");
        if let Some((n, sp)) = c {
            l.push_str(&format!("{sp}# t{n}"));
        }
        l
    })
}

fn document() -> impl Strategy<Value = String> {
    let line = prop_oneof![
        6 => statement(),
        2 => (0..50u32, prop::sample::select(vec!["", "  "])).prop_map(|(n, i)| format!("{i}# c{n}  ")),
        2 => prop::sample::select(vec!["", "  ", "\t"]).prop_map(str::to_string),
        1 => Just("# sparkles-fmt: ignore".to_string()),
        1 => Just("VERSION \"1.2\"".to_string()),
    ];
    let eol = prop::sample::select(vec!["\n", "\n", "\n", "\r\n"]);
    (prop::collection::vec((line, eol), 0..30), any::<bool>()).prop_map(|(lines, last)| {
        let mut s: String = lines.iter().map(|(l, e)| format!("{l}{e}")).collect();
        if last && s.ends_with('\n') {
            s.pop();
            if s.ends_with('\r') {
                s.pop();
            }
        }
        s
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(256),
        // the failure message shows the minimal document; no regression files
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn generated_documents_format_to_fixpoints(doc in document(), chunk in 1usize..40, mode in 0..3u8) {
        let o = opts(mode == 1, mode == 2);
        let f = format(&doc, Language::NTriples, &o).unwrap();
        let again = format(&f.text, Language::NTriples, &o).unwrap();
        prop_assert_eq!(&again.text, &f.text, "not a fixpoint");
        prop_assert!(!again.changed);
        prop_assert_eq!(f.changed, f.text != doc);
        let input = graph::parse(&without_versions(&doc), Language::NTriples).unwrap();
        let output = graph::parse(&without_versions(&f.text), Language::NTriples).unwrap();
        prop_assert!(graph::isomorphic(&input, &output));
        if mode == 0 {
            prop_assert_eq!(&input, &output);
        }
        if mode != 2 {
            prop_assert!(comments::same(&doc, &f.text, LexMode::Turtle).is_ok());
        }
        let mut out = Vec::new();
        let cfg = LinesConfig { sort_memory: 256, ..LinesConfig::default() };
        let stats = format_stream_chunked(doc.as_bytes(), &mut out, Language::NTriples, &o, &cfg, chunk).unwrap();
        prop_assert_eq!(String::from_utf8(out).unwrap(), f.text);
        prop_assert_eq!(stats.changed, f.changed);
    }
}

// ------------------------------------------------------------- memory ------

const MEMORY_CHILD: &str = "SPARKLES_FMT_MEMORY_CHILD";

/// Run in a process of its own by [`memory_stays_bounded`], so that no other test's
/// allocations count.
#[test]
#[ignore = "run by memory_stays_bounded in a process of its own"]
fn memory_child() {
    if std::env::var_os(MEMORY_CHILD).is_none() {
        return;
    }
    const LINES: u64 = 400_000;
    let measure = |o: &Options, cfg: &LinesConfig| {
        let base = CURRENT.load(Ordering::Relaxed);
        PEAK.store(base, Ordering::Relaxed);
        let reader = BufReader::new(Generated::new(LINES));
        let stats = format_lines(reader, io::sink(), Language::NTriples, o, cfg).unwrap();
        assert_eq!(stats.statements, LINES / 10 * 9);
        PEAK.load(Ordering::Relaxed) - base
    };
    let mut input = 0usize;
    let mut g = Generated::new(LINES);
    let mut buf = [0u8; 1 << 16];
    while let Ok(n @ 1..) = g.read(&mut buf) {
        input += n;
    }
    let parent = spill_parent("memory");
    let cfg = LinesConfig {
        spill_dir: parent.clone(),
        sort_memory: 4 << 20,
        threads: 2,
        ..LinesConfig::default()
    };
    let plain = measure(&Options::default(), &cfg);
    let sorted = measure(&opts(true, false), &cfg);
    eprintln!(
        "input {} MiB; peak heap: {} MiB streaming, {} MiB sorting with a 4 MiB budget",
        input >> 20,
        plain >> 20,
        sorted >> 20
    );
    assert!(input > 30 << 20);
    assert!(plain < 12 << 20, "streaming held {plain} bytes");
    assert!(sorted < 24 << 20, "sorting held {sorted} bytes");
    assert_eq!(entries(&parent), 0);
    std::fs::remove_dir_all(&parent).unwrap();
}

#[test]
fn memory_stays_bounded() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "memory_child",
            "--exact",
            "--include-ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(MEMORY_CHILD, "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("1 passed"), "{stdout}\n{stderr}");
    eprintln!(
        "{}",
        stderr
            .lines()
            .find(|l| l.starts_with("input"))
            .unwrap_or("")
    );
}
