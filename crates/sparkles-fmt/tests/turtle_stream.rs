//! Streaming Turtle and TriG ([`format_stream`]): it prints what [`format`] prints, byte
//! for byte, and fails where it fails, for every input of the W3C Turtle and TriG suites,
//! the SHACL files and the golden inputs, under the default options, every key flipped,
//! `prune-prefixes` and the conventional layout, with windows of one statement and reads
//! of a few bytes as well as the defaults; with a comment after every token of the golden
//! inputs; and for generated documents (directives anywhere, comments, blank lines,
//! pragmas, graph blocks, blank nodes). Errors of the reader and the writer, the deadline
//! and the statement size limit stop it; memory stays bounded on a large input (measured
//! by a counting allocator in a process of its own).

mod corpus;

use corpus::rdf;
use proptest::prelude::*;
use sparkles_fmt::turtle::stream::{StreamConfig, can_stream, format_stream, reads_twice};
use sparkles_fmt::{
    DirectiveStyle, FormatError, Language, Options, QuoteStyle, TurtleLayout, format,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{self, BufReader, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
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

// ------------------------------------------------------------------ helpers ------

fn lang(trig: bool) -> Language {
    match trig {
        true => Language::TriG,
        false => Language::Turtle,
    }
}

fn cfg(window: usize, read: usize) -> StreamConfig {
    StreamConfig {
        window_bytes: window,
        read_bytes: read,
        ..StreamConfig::default()
    }
}

/// The window and read sizes every input streams with.
const SIZES: [(usize, usize); 4] = [(1, 1), (1, 5), (64, 3), (256 << 10, 64 << 10)];

/// Stream `text`: the output, or the error.
fn stream(text: &str, trig: bool, opts: &Options, c: &StreamConfig) -> Result<String, FormatError> {
    let mut out = Vec::new();
    format_stream(|| Ok(text.as_bytes()), &mut out, lang(trig), opts, c)?;
    Ok(String::from_utf8(out).expect("UTF-8 output"))
}

/// Whether streaming `text` gives what `format` gives (the text, or the same error), in
/// every size; the first difference otherwise.
fn agrees(text: &str, trig: bool, opts: &Options) -> Result<(), String> {
    let expected = format(text, lang(trig), opts).map(|f| f.text);
    for (window, read) in SIZES {
        let got = catch_unwind(AssertUnwindSafe(|| {
            stream(text, trig, opts, &cfg(window, read))
        }))
        .map_err(|_| format!("window {window}, read {read}: panicked"))?;
        let same = match (&expected, &got) {
            (Ok(a), Ok(b)) => a == b,
            (Err(a), Err(b)) => a == b,
            _ => false,
        };
        if !same {
            return Err(match (&expected, &got) {
                (Ok(a), Ok(b)) => format!(
                    "window {window}, read {read}: the output differs\n--- format\n{a}\n--- stream\n{b}"
                ),
                _ => format!("window {window}, read {read}: format {expected:?}, stream {got:?}"),
            });
        }
    }
    Ok(())
}

/// Every style key that acts on Turtle and TriG away from its default, sort aside.
fn flipped() -> Options {
    Options {
        line_width: 40,
        indent_width: 4,
        directive_style: DirectiveStyle::Turtle,
        prefix_groups: vec![
            vec!["rdf".into(), "rdfs".into(), "xsd".into(), "owl".into()],
            vec!["sh".into(), "".into()],
        ],
        type_shorthand: false,
        compact_iris: false,
        quote_style: QuoteStyle::Preserve,
        prune_prefixes: true,
        turtle_layout: TurtleLayout::Conventional,
        ..Options::default()
    }
}

/// The option sets every input streams under.
fn option_sets() -> Vec<(&'static str, Options)> {
    vec![
        ("defaults", Options::default()),
        ("flipped", flipped()),
        (
            "pruned",
            Options {
                prune_prefixes: true,
                ..Options::default()
            },
        ),
        (
            "conventional",
            Options {
                turtle_layout: TurtleLayout::Conventional,
                line_width: 60,
                ..Options::default()
            },
        ),
    ]
}

fn read(path: &std::path::Path) -> Option<String> {
    String::from_utf8(std::fs::read(path).ok()?).ok()
}

/// The golden Turtle and TriG inputs: `(name, text, trig)`.
fn golden() -> Vec<(String, String, bool)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let mut v = Vec::new();
    for (sub, trig) in [("turtle", false), ("trig", true)] {
        for e in std::fs::read_dir(dir.join(sub)).unwrap().flatten() {
            let p = e.path();
            if p.to_string_lossy().contains(".in.") {
                v.push((p.display().to_string(), read(&p).unwrap(), trig));
            }
        }
    }
    v.sort();
    v
}

/// Run `agrees` over `inputs` under every option set; the failures.
fn check_all(inputs: &[(String, String, bool)]) -> Vec<String> {
    let sets = option_sets();
    corpus::par_map(inputs, |(name, text, trig)| {
        sets.iter()
            .filter_map(|(set, opts)| {
                agrees(text, *trig, opts)
                    .err()
                    .map(|e| format!("{name} ({set}): {e}"))
            })
            .collect::<Vec<_>>()
    })
    .into_iter()
    .flatten()
    .collect()
}

// -------------------------------------------------------------- the corpora ------

#[test]
fn golden_inputs_stream_like_they_format() {
    let inputs = golden();
    assert!(inputs.len() > 10);
    let failures = check_all(&inputs);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn w3c_suites_stream_like_they_format() {
    let Some(dir) = rdf::suite_dir() else {
        eprintln!(
            "W3C RDF suite not found (set SPARKLES_RDF_TESTS_DIR or SPARKLES_W3C_DIR): skipped"
        );
        return;
    };
    let mut inputs = Vec::new();
    let (mut positive, mut negative) = (0, 0);
    for lang in [Language::Turtle, Language::TriG] {
        for c in rdf::cases(&dir, lang) {
            if let Some(text) = read(&c.path) {
                match c.kind.is_positive() {
                    true => positive += 1,
                    false => negative += 1,
                }
                inputs.push((c.rel, text, lang == Language::TriG));
            }
        }
    }
    let failures = check_all(&inputs);
    eprintln!(
        "W3C Turtle and TriG streamed: {} inputs ({positive} positive, {negative} negative), \
         {} option sets, {} sizes, {} failures",
        inputs.len(),
        option_sets().len(),
        SIZES.len(),
        failures.len()
    );
    assert!(positive > 300 && negative > 100);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn shacl_files_stream_like_they_format() {
    let Some(dir) = rdf::shacl_dir() else {
        eprintln!("SHACL tests not found (set SPARKLES_SHACL_TESTS): skipped");
        return;
    };
    let inputs: Vec<(String, String, bool)> = rdf::shacl_files(&dir)
        .into_iter()
        .filter_map(|(rel, path)| Some((format!("shacl:{rel}"), read(&path)?, false)))
        .collect();
    let failures = check_all(&inputs);
    eprintln!(
        "SHACL files streamed: {} inputs, {} failures",
        inputs.len(),
        failures.len()
    );
    assert!(inputs.len() > 100);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// A trailing comment, and a comment on a line of its own, after every significant token
/// of the golden inputs: streaming in windows of one statement still agrees.
#[test]
fn comment_sweeps_stream_like_they_format() {
    use sparkles_fmt::lex::{LexMode, TokenKind, lex};
    let mut docs = Vec::new();
    for (name, text, trig) in golden() {
        let ends: Vec<usize> = lex(&text, LexMode::Turtle)
            .iter()
            .filter(|t| !t.kind.is_trivia() && t.kind != TokenKind::Eof)
            .map(|t| t.end())
            .collect();
        for end in ends {
            for comment in [" # c\n", "\n# c\n", " # sparkles-fmt: ignore\n"] {
                let mut s = text.clone();
                s.insert_str(end, comment);
                docs.push((format!("{name} with {comment:?} at {end}"), s, trig));
            }
        }
    }
    let opts = Options {
        prune_prefixes: true,
        ..Options::default()
    };
    let c = cfg(1, 7);
    let failures: Vec<String> = corpus::par_map(&docs, |(name, text, trig)| {
        let expected = format(text, lang(*trig), &opts).map(|f| f.text);
        let got = catch_unwind(AssertUnwindSafe(|| stream(text, *trig, &opts, &c)));
        match got {
            Ok(got) if got == expected => None,
            Ok(got) => Some(format!("{name}: format {expected:?}\nstream {got:?}")),
            Err(_) => Some(format!("{name}: panicked")),
        }
    })
    .into_iter()
    .flatten()
    .collect();
    eprintln!(
        "comment sweeps streamed: {} documents, {} failures",
        docs.len(),
        failures.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

// ------------------------------------------------------- generated documents ------

fn term() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("ex:a".to_string()),
        Just("<http://example.org/b>".to_string()),
        Just("<http://other.org/c>".to_string()),
        Just("o:d".to_string()),
        Just("_:x".to_string()),
        Just("[]".to_string()),
        Just("[ ex:p 1 ]".to_string()),
        Just("( 1 ex:a )".to_string()),
        Just("\"x\"".to_string()),
        Just("'''two\nlines'''".to_string()),
        Just("\"1\"^^<http://www.w3.org/2001/XMLSchema#integer>".to_string()),
    ]
}

fn subject() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("ex:s".to_string()),
        Just("<http://example.org/s>".to_string()),
        Just("_:x".to_string()),
        Just("[ ex:q ex:a ]".to_string()),
        Just("o:t".to_string()),
    ]
}

fn statement() -> impl Strategy<Value = String> {
    (
        subject(),
        prop::collection::vec(
            (
                prop_oneof![
                    Just("ex:p"),
                    Just("a"),
                    Just("<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>")
                ],
                prop::collection::vec(term(), 1..3),
            ),
            1..3,
        ),
    )
        .prop_map(|(s, entries)| {
            let entries: Vec<String> = entries
                .into_iter()
                .map(|(p, os)| format!("{p} {}", os.join(", ")))
                .collect();
            format!("{s} {} .", entries.join(" ; "))
        })
}

fn trivia() -> impl Strategy<Value = &'static str> {
    prop_oneof![
        Just(" "),
        Just("\n"),
        Just("\n\n"),
        Just(" # c\n"),
        Just("\n# c\n"),
        Just("\n\n# section\n\n"),
        Just("\n# sparkles-fmt: ignore\n"),
    ]
}

fn item(trig: bool) -> impl Strategy<Value = String> {
    let directive = prop_oneof![
        Just("PREFIX ex: <http://example.org/>".to_string()),
        Just("@prefix o: <http://other.org/> .".to_string()),
        Just("PREFIX o: <http://example.org/o/>".to_string()),
        Just("PREFIX unused: <http://unused.org/>".to_string()),
        Just("BASE <http://example.org/base/>".to_string()),
    ];
    let block = prop::collection::vec((trivia(), statement()), 0..4).prop_map(|ss| {
        let body: String = ss.into_iter().map(|(t, s)| format!("{t}{s}")).collect();
        format!("GRAPH ex:g {{{body}\n}}")
    });
    match trig {
        true => prop_oneof![4 => statement(), 2 => directive, 2 => block].boxed(),
        false => prop_oneof![4 => statement(), 2 => directive].boxed(),
    }
}

fn document(trig: bool) -> impl Strategy<Value = String> {
    prop::collection::vec((trivia(), item(trig)), 0..12).prop_map(|items| {
        let mut s = String::from("PREFIX ex: <http://example.org/>\n");
        for (t, i) in items {
            s.push_str(t);
            s.push_str(&i);
        }
        s.push('\n');
        s
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(256),
        ..ProptestConfig::default()
    })]

    #[test]
    fn generated_documents_stream_like_they_format(
        trig in any::<bool>(),
        doc in any::<bool>().prop_flat_map(document),
        window in prop_oneof![Just(1usize), 1usize..400],
        read in 1usize..64,
        prune in any::<bool>(),
        conventional in any::<bool>(),
    ) {
        let opts = Options {
            prune_prefixes: prune,
            turtle_layout: match conventional {
                true => TurtleLayout::Conventional,
                false => TurtleLayout::Diff,
            },
            ..Options::default()
        };
        let expected = format(&doc, lang(trig), &opts).map(|f| f.text);
        let got = stream(&doc, trig, &opts, &cfg(window, read));
        prop_assert_eq!(got, expected, "{}", doc);
    }
}

// --------------------------------------------------------------- the edges ------

#[test]
fn what_streams() {
    let sorted = Options {
        sort: true,
        ..Options::default()
    };
    assert!(can_stream(&Options::default()));
    assert!(!can_stream(&sorted) || !sparkles_fmt::turtle::sort::IMPLEMENTED);
    assert!(reads_twice(&Options {
        prune_prefixes: true,
        ..Options::default()
    }));
    assert!(!reads_twice(&Options::default()));
    let e = format_stream(
        || Ok(&b"<a> <b> <c> .\n"[..]),
        io::sink(),
        Language::NTriples,
        &Options::default(),
        &StreamConfig::default(),
    )
    .unwrap_err();
    assert_eq!(e, FormatError::unsupported_language(Language::NTriples));
    if sparkles_fmt::turtle::sort::IMPLEMENTED {
        let e = format_stream(
            || Ok(&b"<a> <b> <c> .\n"[..]),
            io::sink(),
            Language::Turtle,
            &sorted,
            &StreamConfig::default(),
        )
        .unwrap_err();
        assert_eq!(e.code(), "unsupported-language");
    }
}

#[test]
fn stats_and_ignore_file() {
    let text =
        "PREFIX ex: <http://e/>\nex:a ex:b ex:c .\nGRAPH ex:g { ex:a ex:b 1 . ex:a ex:b 2 }\n";
    let mut out = Vec::new();
    let stats = format_stream(
        || Ok(text.as_bytes()),
        &mut out,
        Language::TriG,
        &Options::default(),
        &cfg(1, 3),
    )
    .unwrap();
    assert_eq!(stats.statements, 3);
    assert!(stats.changed);
    let formatted = String::from_utf8(out).unwrap();
    let stats = format_stream(
        || Ok(formatted.as_bytes()),
        io::sink(),
        Language::TriG,
        &Options::default(),
        &cfg(1, 3),
    )
    .unwrap();
    assert!(!stats.changed);
    // kept byte for byte
    let text = "\u{feff}# sparkles-fmt: ignore-file\n<a>   <b>   <c> .\nnot turtle at all";
    let mut out = Vec::new();
    let stats = format_stream(
        || Ok(text.as_bytes()),
        &mut out,
        Language::Turtle,
        &Options::default(),
        &cfg(1, 2),
    )
    .unwrap();
    assert_eq!(out, text.as_bytes());
    assert!(!stats.changed);
    // a BOM is dropped
    let mut out = Vec::new();
    let stats = format_stream(
        || Ok("\u{feff}<http://a> <http://b> <http://c> .\n".as_bytes()),
        &mut out,
        Language::Turtle,
        &Options::default(),
        &cfg(1, 2),
    )
    .unwrap();
    assert_eq!(out, b"<http://a> <http://b> <http://c> .\n");
    assert!(stats.changed);
}

#[test]
fn warnings_point_into_the_input() {
    // a comment where no element starts or ends is moved, and says where it was
    let text = "PREFIX ex: <http://e/>\nex:a ex:b ex:c .\nex:a ex:b \"x\" # moved\n ^^ex:t .\nex:a ex:b ex:c .\n";
    let f = format(text, Language::Turtle, &Options::default()).unwrap();
    assert!(!f.warnings.is_empty(), "{:?}", f.warnings);
    for (window, read) in SIZES {
        let stats = format_stream(
            || Ok(text.as_bytes()),
            io::sink(),
            Language::Turtle,
            &Options::default(),
            &cfg(window, read),
        )
        .unwrap();
        assert_eq!(stats.warnings, f.warnings, "window {window}, read {read}");
    }
}

/// A reader failing after `ok` bytes.
struct Failing {
    data: Vec<u8>,
    at: usize,
    ok: usize,
}

impl Read for Failing {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.at >= self.ok {
            return Err(io::Error::other("disk on fire"));
        }
        let n = out
            .len()
            .min(self.ok - self.at)
            .min(self.data.len() - self.at);
        out[..n].copy_from_slice(&self.data[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

#[test]
fn failures_stop_the_stream() {
    let text: String = (0..2000)
        .map(|i| format!("<http://e/s{i}> <http://e/p> {i} .\n"))
        .collect();
    // the reader
    let e = format_stream(
        || {
            Ok(BufReader::new(Failing {
                data: text.clone().into_bytes(),
                at: 0,
                ok: 5000,
            }))
        },
        io::sink(),
        Language::Turtle,
        &Options::default(),
        &cfg(512, 512),
    )
    .unwrap_err();
    assert!(e.to_string().contains("disk on fire"), "{e}");
    // the writer
    struct Full;
    impl Write for Full {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("no space left"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let e = format_stream(
        || Ok(text.as_bytes()),
        Full,
        Language::Turtle,
        &Options::default(),
        &cfg(512, 512),
    )
    .unwrap_err();
    assert!(e.to_string().contains("no space left"), "{e}");
    // the deadline
    let past = Options {
        deadline: Some(Instant::now() - Duration::from_secs(1)),
        ..Options::default()
    };
    let e = format_stream(
        || Ok(text.as_bytes()),
        io::sink(),
        Language::Turtle,
        &past,
        &cfg(512, 512),
    )
    .unwrap_err();
    assert_eq!(e, FormatError::Timeout);
    // a statement larger than the limit
    let big = format!(
        "<http://e/s> <http://e/p> {} .\n",
        (0..5000)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let small = StreamConfig {
        max_statement_bytes: 4096,
        ..cfg(512, 512)
    };
    let e = format_stream(
        || Ok(big.as_bytes()),
        io::sink(),
        Language::Turtle,
        &Options::default(),
        &small,
    )
    .unwrap_err();
    assert_eq!(e, FormatError::TooLarge);
    // an unterminated bracket is the syntax error it is, not a size problem
    let bad = format!("<http://e/s> <http://e/p> [ <http://e/q> 1 .\n{text}");
    let e = format_stream(
        || Ok(bad.as_bytes()),
        io::sink(),
        Language::Turtle,
        &Options::default(),
        &small,
    )
    .unwrap_err();
    assert!(matches!(e, FormatError::Syntax { .. }), "{e:?}");
    // the second read of `prune-prefixes` fails to open
    let mut opened = 0;
    let e = format_stream(
        || {
            opened += 1;
            match opened {
                1 => Ok(text.as_bytes()),
                _ => Err(io::Error::other("gone")),
            }
        },
        io::sink(),
        Language::Turtle,
        &Options {
            prune_prefixes: true,
            ..Options::default()
        },
        &cfg(512, 512),
    )
    .unwrap_err();
    assert!(e.to_string().contains("gone"), "{e}");
}

// ------------------------------------------------------------------- memory ------

/// A generated document, produced as it is read: `n` statements under a few prefixes,
/// with comments and blank nodes; in TriG, all in one graph block.
struct Generated {
    next: u64,
    n: u64,
    trig: bool,
    buf: Vec<u8>,
    at: usize,
}

impl Generated {
    fn new(n: u64, trig: bool) -> Generated {
        let mut head = b"# generated\nPREFIX ex: <http://example.org/>\nPREFIX xsd: <http://www.w3.org/2001/XMLSchema#>\n\n".to_vec();
        if trig {
            head.extend_from_slice(b"GRAPH ex:g {\n");
        }
        Generated {
            next: 0,
            n,
            trig,
            buf: head,
            at: 0,
        }
    }
}

impl Read for Generated {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.at == self.buf.len() {
            if self.next == self.n {
                if self.trig && self.next < u64::MAX {
                    self.buf = b"}\n".to_vec();
                    self.at = 0;
                    self.next = u64::MAX;
                    continue;
                }
                return Ok(0);
            }
            if self.next > self.n {
                return Ok(0);
            }
            let i = self.next;
            self.next += 1;
            self.buf.clear();
            self.at = 0;
            let line = match i % 4 {
                0 => format!(
                    "<http://example.org/s{i}> <http://example.org/p> \"{i}\"^^xsd:integer, _:b{i} .\n"
                ),
                1 => format!("# about {i}\nex:s{i} a ex:C ; ex:q [ ex:r {i} ] .\n"),
                2 => format!("ex:s{i}   ex:p   \"x{i}\" .   # trailing\n\n"),
                _ => format!("_:b{} ex:p ( 1 2 {i} ) .\n", i - 3),
            };
            self.buf.extend_from_slice(line.as_bytes());
        }
        let n = out.len().min(self.buf.len() - self.at);
        out[..n].copy_from_slice(&self.buf[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

const MEMORY_CHILD: &str = "SPARKLES_FMT_TURTLE_MEMORY_CHILD";

/// Run in a process of its own by [`memory_stays_bounded`], so that no other test's
/// allocations count.
#[test]
#[ignore = "run by memory_stays_bounded in a process of its own"]
fn memory_child() {
    if std::env::var_os(MEMORY_CHILD).is_none() {
        return;
    }
    // the formatter takes about 2 s per MB unoptimized: a few MB in windows of 16 KiB
    const N: u64 = 120_000;
    let mut input = 0usize;
    let mut g = Generated::new(N, false);
    let mut chunk = [0u8; 1 << 16];
    while let Ok(n @ 1..) = g.read(&mut chunk) {
        input += n;
    }
    let measure = |trig: bool, opts: &Options| {
        let base = CURRENT.load(Ordering::Relaxed);
        PEAK.store(base, Ordering::Relaxed);
        let stats = format_stream(
            || Ok(BufReader::new(Generated::new(N, trig))),
            io::sink(),
            lang(trig),
            opts,
            &cfg(16 << 10, 64 << 10),
        )
        .unwrap();
        assert_eq!(stats.statements, N);
        assert!(stats.changed);
        PEAK.load(Ordering::Relaxed) - base
    };
    let pruned = measure(
        false,
        &Options {
            prune_prefixes: true,
            ..Options::default()
        },
    );
    let trig = measure(true, &Options::default());
    eprintln!(
        "input {} KiB; peak heap: {} KiB Turtle with prune-prefixes (two passes), {} KiB TriG \
         (one graph block), in windows of 16 KiB",
        input >> 10,
        pruned >> 10,
        trig >> 10
    );
    assert!(input > 6 << 20);
    for (what, peak) in [("Turtle", pruned), ("TriG", trig)] {
        assert!(peak < 4 << 20, "{what}: streaming held {peak} bytes");
    }
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
