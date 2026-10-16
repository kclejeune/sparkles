//! PDF and Office conversions in a child process (spec C18 §7.1.1).
//!
//! A converter reads a file that comes from outside. The converters bound what they
//! allocate and check the task's deadline, but a parser bug could still loop or allocate
//! without a check, and a thread cannot be stopped from outside. So the `sparkles` binary
//! runs each conversion in a child process of its own: the same binary started with the
//! hidden first argument [`CHILD_ARG`], the file on its standard input and the result on
//! its standard output. The child runs under `RLIMIT_AS` (`--convert-memory`), a CPU
//! limit, `RLIMIT_FSIZE` of 0 (it can write no file, unless OCR is configured, whose
//! libraries are not audited for it) and no core file, and on Linux it is killed when the
//! server dies. The parent kills it when the task's deadline passes, so a conversion
//! never outlives its deadline by more than [`GRACE`], its worker slot is always released,
//! and its memory is bounded whatever the converter does. The parent reads at most a
//! bounded number of bytes of output.
//!
//! Only the server binary can serve as the child, so the child process is used only when
//! `main` has called [`enable`]. Library use of the crate and its unit tests run the
//! conversion on a thread of the calling process, as does `--convert-in-process`.
//!
//! The reply is a small binary encoding of the result: a tag byte, then fields that are
//! each a little-endian `u64` or a length-prefixed byte string. Nothing in it is trusted
//! beyond its framing: a reply that cannot be read is a failed conversion.

use super::convert::{Converted, EmbeddedImage, Format, Options, PageReason, Refusal};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The first argument that makes the binary a conversion child.
pub const CHILD_ARG: &str = "__convert-child";

/// How long past the task's deadline the parent waits for the child, which checks the
/// same deadline itself and then reports a timeout of its own.
pub const GRACE: Duration = Duration::from_millis(250);

static EXE: OnceLock<PathBuf> = OnceLock::new();

/// Let conversions run in child processes of this binary. Called by `main` of the
/// `sparkles` binary only, which answers [`CHILD_ARG`].
pub fn enable() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = EXE.set(exe);
    }
}

/// The binary that serves as the child, when [`enable`] has been called.
pub fn child_exe() -> Option<PathBuf> {
    EXE.get().cloned()
}

/// How a server runs its conversions in child processes.
#[derive(Clone, Debug)]
pub struct Isolation {
    pub exe: PathBuf,
    /// `RLIMIT_AS` of the child, in bytes
    pub memory: u64,
    /// arguments put before [`CHILD_ARG`]'s own, for tests that run another program
    pub prefix: Vec<OsString>,
}

/// What a child converts.
#[derive(Clone, Copy, Debug)]
pub enum Job {
    /// an Office document to Markdown
    #[cfg(feature = "office")]
    Text(Format),
    /// the sheets of a spreadsheet as CSV
    #[cfg(feature = "office")]
    Sheets(Format),
    /// a PDF to Markdown, keeping what can be read under `allowPartial`
    #[cfg(feature = "pdf")]
    Pdf { allow_partial: bool },
}

/// A child's result.
#[derive(Debug, PartialEq)]
pub enum Reply {
    Text(Converted),
    #[cfg(feature = "office")]
    Sheets(Vec<sparkles_office::Sheet>),
}

/// The most bytes of reply a job may write: its text and pictures, or the CSV of its
/// sheets, which the converter's output budget bounds, with room for the framing.
fn reply_cap(job: Job, o: &Options) -> usize {
    let slack = 16 << 20;
    match job {
        #[cfg(feature = "office")]
        Job::Text(_) => {
            let l = sparkles_office::Limits::default();
            o.max_text
                .saturating_mul(2)
                .saturating_add(o.max_images.saturating_mul(l.max_image_bytes + 4096))
                .saturating_add(slack)
        }
        #[cfg(feature = "office")]
        Job::Sheets(_) => sparkles_office::Limits::default()
            .max_output
            .saturating_add(slack),
        #[cfg(feature = "pdf")]
        Job::Pdf { .. } => o.max_text.saturating_mul(2).saturating_add(slack),
    }
}

fn job_args(job: Job) -> Vec<String> {
    match job {
        #[cfg(feature = "office")]
        Job::Text(f) => vec!["job=text".into(), format!("format={}", format_name(f))],
        #[cfg(feature = "office")]
        Job::Sheets(f) => vec!["job=sheets".into(), format!("format={}", format_name(f))],
        #[cfg(feature = "pdf")]
        Job::Pdf { allow_partial } => {
            vec![
                "job=pdf".into(),
                format!("partial={}", u8::from(allow_partial)),
            ]
        }
    }
}

#[cfg(feature = "office")]
fn format_name(f: Format) -> &'static str {
    match f {
        Format::Docx => "docx",
        Format::Pptx => "pptx",
        Format::Xlsx => "xlsx",
        Format::Odt => "odt",
        Format::Odp => "odp",
        _ => "ods",
    }
}

fn timeout() -> Refusal {
    Refusal::new(
        "conversion-timeout",
        504,
        "the document's conversion did not end before the deadline",
    )
}

fn failed(m: impl Into<String>) -> Refusal {
    Refusal::new("conversion-failed", 422, m)
}

/// Run `job` on `bytes` in a child process and wait for it until `o`'s deadline and
/// [`GRACE`]. The caller holds the worker slot. `Err(None)` means no child could be
/// started, and the caller may convert in process instead.
pub fn run(iso: &Isolation, job: Job, bytes: &[u8], o: &Options) -> Result<Reply, Option<Refusal>> {
    let left = o.deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(Some(timeout()));
    }
    let ocr = o.pdf.ocr_config();
    let mut cmd = Command::new(&iso.exe);
    cmd.args(&iso.prefix)
        .arg(CHILD_ARG)
        .args(job_args(job))
        .arg(format!("max_text={}", o.max_text))
        .arg(format!("max_images={}", o.max_images))
        // the child reads at most this many bytes, which the callers have checked
        .arg(format!("max_input={}", o.max_input.max(bytes.len())))
        .arg(format!("millis={}", left.as_millis()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(feature = "pdf-ocr")]
    if let Some(c) = ocr {
        cmd.arg(OsString::from("ocr_models=").tap_push(&c.models));
        if let Some(p) = &c.pdfium {
            cmd.arg(OsString::from("pdfium=").tap_push(p));
        }
    }
    #[cfg(unix)]
    limit(&mut cmd, iso.memory, left, ocr.is_none());
    #[cfg(not(unix))]
    let _ = ocr;
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        // the binary is gone, as after an upgrade that replaced it
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(
                "the conversion child {} is missing, converting in process",
                iso.exe.display()
            );
            return Err(None);
        }
        Err(e) => {
            return Err(Some(Refusal::new(
                "conversion-failed",
                500,
                format!("no process could be started for the conversion: {e}"),
            )));
        }
    };
    let mut stdin = child.stdin.take().expect("piped");
    let mut stdout = child.stdout.take().expect("piped");
    let cap = reply_cap(job, o);
    let wait_until = o.deadline + GRACE;
    let (tx, rx) = std::sync::mpsc::channel();
    let outcome = std::thread::scope(|s| {
        // the input is written and the output read on threads of their own, so that a
        // child that writes before it has read everything cannot block the parent
        s.spawn(move || {
            let _ = stdin.write_all(bytes);
        });
        s.spawn(move || {
            let mut out = Vec::new();
            let r = (&mut stdout).take(cap as u64 + 1).read_to_end(&mut out);
            let _ = tx.send(r.map(|_| out));
        });
        let left = wait_until.saturating_duration_since(Instant::now());
        let got = rx.recv_timeout(left);
        // a child that has answered or overrun ends here, which closes both pipes and
        // ends the two threads
        let status = match &got {
            Ok(Ok(out)) if out.len() <= cap => {
                // it has closed its output: give it a moment to exit, then make sure
                let end = Instant::now() + Duration::from_secs(1);
                let mut pause = Duration::from_micros(20);
                loop {
                    match child.try_wait() {
                        Ok(Some(st)) => break Some(st),
                        Ok(None) if Instant::now() < end => {
                            std::thread::sleep(pause);
                            pause = (pause * 2).min(Duration::from_millis(10));
                        }
                        _ => {
                            let _ = child.kill();
                            break child.wait().ok();
                        }
                    }
                }
            }
            _ => {
                let _ = child.kill();
                child.wait().ok()
            }
        };
        (got, status)
    });
    let (got, status) = outcome;
    match got {
        Err(_) => Err(Some(timeout())),
        Ok(Err(e)) => Err(Some(failed(format!(
            "the conversion's output could not be read: {e}"
        )))),
        Ok(Ok(out)) if out.len() > cap => Err(Some(Refusal::new(
            "too-large",
            413,
            format!("the converted document is larger than {cap} bytes"),
        ))),
        Ok(Ok(out)) => {
            if out.is_empty() {
                return Err(Some(ended(status, iso.memory)));
            }
            match decode(&out) {
                Some(Ok(r)) => Ok(r),
                Some(Err(r)) => Err(Some(r)),
                None => Err(Some(failed(
                    "the document could not be read: the converter's reply was damaged",
                ))),
            }
        }
    }
}

/// Why a child that wrote nothing ended.
fn ended(status: Option<std::process::ExitStatus>, memory: u64) -> Refusal {
    #[cfg(unix)]
    if let Some(sig) = status.and_then(|s| std::os::unix::process::ExitStatusExt::signal(&s)) {
        if sig == libc::SIGXCPU || sig == libc::SIGKILL {
            return timeout();
        }
        return failed(format!(
            "the document could not be read: the converter stopped on it (signal {sig}), which it also does when the document needs more than its memory limit of {} MiB",
            memory >> 20
        ));
    }
    let _ = (status, memory);
    failed("the document could not be read: the converter stopped on it")
}

/// The limits of the child, set between `fork` and `exec`.
#[cfg(unix)]
fn limit(cmd: &mut Command, memory: u64, left: Duration, no_files: bool) {
    use std::os::unix::process::CommandExt;
    // a backstop for a parent that cannot kill the child: CPU time of every thread, so
    // that a converter using all cores is not stopped before its wall-clock deadline
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as u64;
    let cpu = (left.as_secs() + 1).saturating_mul(cores).saturating_add(1);
    let set = move |res, soft: u64, hard: u64| {
        let l = libc::rlimit {
            rlim_cur: soft as libc::rlim_t,
            rlim_max: hard as libc::rlim_t,
        };
        // SAFETY: setrlimit only reads the struct, and it is async-signal-safe
        unsafe { libc::setrlimit(res, &l) }
    };
    // SAFETY: the closure calls only setrlimit and prctl, which are async-signal-safe,
    // and allocates nothing
    unsafe {
        cmd.pre_exec(move || {
            if set(libc::RLIMIT_AS, memory, memory) != 0
                || set(libc::RLIMIT_CPU, cpu, cpu + 1) != 0
                || set(libc::RLIMIT_CORE, 0, 0) != 0
                || (no_files && set(libc::RLIMIT_FSIZE, 0, 0) != 0)
            {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(target_os = "linux")]
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(feature = "pdf-ocr")]
trait TapPush {
    fn tap_push(self, p: &std::path::Path) -> OsString;
}

#[cfg(feature = "pdf-ocr")]
impl TapPush for OsString {
    fn tap_push(mut self, p: &std::path::Path) -> OsString {
        self.push(p);
        self
    }
}

// ---- the child --------------------------------------------------------------------

/// The child's `main`: read the request from the arguments after [`CHILD_ARG`] and the
/// file from standard input, convert, and write the reply. The exit code is 0 when a
/// reply was written.
pub fn child_main(args: Vec<OsString>) -> i32 {
    let started = Instant::now();
    let mut kv: std::collections::HashMap<String, OsString> = Default::default();
    for a in args {
        let Some(s) = a.to_str() else {
            // only a path can be other than UTF-8, and only under a known key
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                let b = a.as_bytes();
                if let Some(i) = b.iter().position(|c| *c == b'=') {
                    let k = String::from_utf8_lossy(&b[..i]).into_owned();
                    kv.insert(k, std::ffi::OsStr::from_bytes(&b[i + 1..]).to_owned());
                }
            }
            continue;
        };
        if let Some((k, v)) = s.split_once('=') {
            kv.insert(k.into(), v.into());
        }
    }
    let num = |k: &str| -> usize {
        kv.get(k)
            .and_then(|v| v.to_str())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    let millis = num("millis") as u64;
    let deadline = started + Duration::from_millis(millis);
    let max_input = num("max_input");
    let mut bytes = Vec::new();
    if std::io::stdin()
        .lock()
        .take(max_input as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > max_input
    {
        return 2;
    }
    let text = |k: &str| kv.get(k).and_then(|v| v.to_str()).unwrap_or("").to_string();
    let job = text("job");
    let format = Format::parse(&text("format"));
    #[cfg(feature = "pdf-ocr")]
    let ocr = kv.get("ocr_models").map(|m| super::OcrConfig {
        models: PathBuf::from(m),
        pdfium: kv.get("pdfium").map(PathBuf::from),
    });
    #[cfg(not(feature = "pdf-ocr"))]
    let ocr: Option<super::OcrConfig> = None;
    let o = Options {
        allow_partial: text("partial") == "1",
        max_input,
        max_text: num("max_text"),
        deadline,
        pdf: std::sync::Arc::new(super::PdfRuntime::new(1, ocr)),
        max_images: num("max_images"),
    };
    // the converters walk nested XML recursively, to the depth their limits allow
    let r = std::thread::Builder::new()
        .stack_size(16 << 20)
        .spawn(move || convert_here(&job, format, &bytes, &o))
        .ok()
        .and_then(|h| h.join().ok());
    let Some(r) = r else {
        // a panic: the parent reports that the converter stopped
        return 101;
    };
    let reply = encode(&r);
    let mut out = std::io::stdout().lock();
    if out.write_all(&reply).and_then(|_| out.flush()).is_err() {
        return 2;
    }
    0
}

fn convert_here(
    job: &str,
    format: Option<Format>,
    bytes: &[u8],
    o: &Options,
) -> Result<Reply, Refusal> {
    let _ = (bytes, o);
    match (job, format) {
        #[cfg(feature = "office")]
        ("text", Some(f)) => super::office::convert_here(bytes, f, o).map(Reply::Text),
        #[cfg(feature = "office")]
        ("sheets", Some(f)) => super::office::sheets_here(bytes, f, o).map(Reply::Sheets),
        #[cfg(feature = "pdf")]
        ("pdf", _) => super::pdf::inspect(bytes, o.allow_partial, &o.pdf).map(Reply::Text),
        _ => Err(failed("the conversion child was asked for an unknown job")),
    }
}

// ---- the reply's encoding -------------------------------------------------------------

const TAG_REFUSAL: u8 = 0;
const TAG_TEXT: u8 = 1;
#[cfg(feature = "office")]
const TAG_SHEETS: u8 = 2;

struct W(Vec<u8>);

impl W {
    fn num(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.num(b.len() as u64);
        self.0.extend_from_slice(b);
    }
    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    fn opt(&mut self, s: Option<&str>) {
        match s {
            Some(s) => {
                self.num(1);
                self.str(s);
            }
            None => self.num(0),
        }
    }
    fn strs(&mut self, v: &[String]) {
        self.num(v.len() as u64);
        for s in v {
            self.str(s);
        }
    }
    fn nums(&mut self, v: impl ExactSizeIterator<Item = u64>) {
        self.num(v.len() as u64);
        for n in v {
            self.num(n);
        }
    }
}

struct R<'a>(&'a [u8]);

impl<'a> R<'a> {
    fn num(&mut self) -> Option<u64> {
        let (h, t) = self.0.split_first_chunk::<8>()?;
        self.0 = t;
        Some(u64::from_le_bytes(*h))
    }
    /// A count of items that each take at least `min` bytes, checked against what is
    /// left, so that a damaged reply cannot ask for a large allocation.
    fn count(&mut self, min: usize) -> Option<usize> {
        let n = usize::try_from(self.num()?).ok()?;
        (n.checked_mul(min)? <= self.0.len()).then_some(n)
    }
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.count(1)?;
        let (h, t) = self.0.split_at(n);
        self.0 = t;
        Some(h)
    }
    fn str(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?.to_vec()).ok()
    }
    fn opt(&mut self) -> Option<Option<String>> {
        match self.num()? {
            0 => Some(None),
            1 => Some(Some(self.str()?)),
            _ => None,
        }
    }
    fn strs(&mut self) -> Option<Vec<String>> {
        let n = self.count(8)?;
        (0..n).map(|_| self.str()).collect()
    }
    fn nums(&mut self) -> Option<Vec<u64>> {
        let n = self.count(8)?;
        (0..n).map(|_| self.num()).collect()
    }
}

pub(crate) fn encode(r: &Result<Reply, Refusal>) -> Vec<u8> {
    let mut w = W(Vec::new());
    match r {
        Err(f) => {
            w.0.push(TAG_REFUSAL);
            w.str(f.code);
            w.num(f.status.into());
            w.str(&f.message);
            w.num(f.pages.len() as u64);
            for p in &f.pages {
                w.num(p.page.into());
                w.strs(&p.reasons);
            }
            w.strs(&f.fonts);
        }
        Ok(Reply::Text(c)) => {
            w.0.push(TAG_TEXT);
            w.str(&c.text);
            w.str(&c.media_type);
            w.opt(c.title.as_deref());
            w.nums(c.page_starts.iter().map(|n| *n as u64));
            w.num(c.images.len() as u64);
            for i in &c.images {
                w.str(i.media_type);
                w.bytes(&i.bytes);
                w.opt(i.alt.as_deref());
            }
            w.nums(c.ocr_pages.iter().map(|n| u64::from(*n)));
            w.nums(c.omitted_pages.iter().map(|n| u64::from(*n)));
            w.strs(&c.notes);
        }
        #[cfg(feature = "office")]
        Ok(Reply::Sheets(v)) => {
            w.0.push(TAG_SHEETS);
            w.num(v.len() as u64);
            for s in v {
                w.str(&s.name);
                w.num(s.index as u64);
                w.str(&s.csv);
                w.num(s.rows as u64);
                w.num(s.columns as u64);
            }
        }
    }
    w.0
}

/// The codes a refusal may carry, as the static strings the server uses.
const CODES: &[&str] = &[
    "unsupported-format",
    "too-large",
    "not-utf8",
    "empty",
    "needs-ocr",
    "conversion-failed",
    "conversion-timeout",
    "ocr-failed",
];

fn media_type(s: &str) -> Option<&'static str> {
    ["image/png", "image/jpeg", "image/webp"]
        .into_iter()
        .find(|m| *m == s)
}

pub(crate) fn decode(b: &[u8]) -> Option<Result<Reply, Refusal>> {
    let (tag, rest) = b.split_first()?;
    let mut r = R(rest);
    let out = match *tag {
        TAG_REFUSAL => {
            let code = r.str()?;
            let code = CODES.iter().find(|c| **c == code).copied()?;
            let status = u16::try_from(r.num()?).ok()?;
            let message = r.str()?;
            let n = r.count(16)?;
            let mut pages = Vec::with_capacity(n);
            for _ in 0..n {
                let page = u32::try_from(r.num()?).ok()?;
                pages.push(PageReason {
                    page,
                    reasons: r.strs()?,
                });
            }
            let mut f = Refusal::new(code, status, message);
            f.pages = pages;
            f.fonts = r.strs()?;
            Err(f)
        }
        TAG_TEXT => {
            let text = r.str()?;
            let media = r.str()?;
            let title = r.opt()?;
            let page_starts = r
                .nums()?
                .into_iter()
                .map(usize::try_from)
                .collect::<Result<_, _>>()
                .ok()?;
            let n = r.count(24)?;
            let mut images = Vec::with_capacity(n);
            for _ in 0..n {
                images.push(EmbeddedImage {
                    media_type: media_type(&r.str()?)?,
                    bytes: r.bytes()?.to_vec(),
                    alt: r.opt()?,
                });
            }
            let pages = |v: Vec<u64>| -> Option<Vec<u32>> {
                v.into_iter().map(|n| u32::try_from(n).ok()).collect()
            };
            let ocr_pages = pages(r.nums()?)?;
            let omitted_pages = pages(r.nums()?)?;
            let notes = r.strs()?;
            Ok(Reply::Text(Converted {
                text,
                media_type: media,
                title,
                page_starts,
                images,
                ocr_pages,
                omitted_pages,
                notes,
            }))
        }
        #[cfg(feature = "office")]
        TAG_SHEETS => {
            let n = r.count(40)?;
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(sparkles_office::Sheet {
                    name: r.str()?,
                    index: usize::try_from(r.num()?).ok()?,
                    csv: r.str()?,
                    rows: usize::try_from(r.num()?).ok()?,
                    columns: usize::try_from(r.num()?).ok()?,
                });
            }
            Ok(Reply::Sheets(v))
        }
        _ => return None,
    };
    r.0.is_empty().then_some(out)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[cfg(feature = "office")]
    fn options(deadline: Duration, rt: super::super::PdfRuntime) -> Options {
        Options {
            allow_partial: false,
            max_input: 1 << 20,
            max_text: 1000,
            deadline: Instant::now() + deadline,
            pdf: std::sync::Arc::new(rt),
            max_images: 0,
        }
    }

    /// An isolation that runs `script` in `sh` in place of the binary.
    fn shell(script: &str) -> Isolation {
        Isolation {
            exe: "/bin/sh".into(),
            memory: 1 << 30,
            prefix: vec!["-c".into(), script.into(), "sh".into()],
        }
    }

    #[cfg(feature = "office")]
    const JOB: Job = Job::Text(Format::Docx);

    #[cfg(feature = "office")]
    #[test]
    fn replies_round_trip() {
        let mut f = Refusal::new("needs-ocr", 422, "two pages");
        f.pages = vec![PageReason {
            page: 3,
            reasons: vec!["scanned".into()],
        }];
        f.fonts = vec!["F1".into()];
        let text = Converted {
            text: "# T\n".into(),
            media_type: "application/pdf".into(),
            title: Some("T".into()),
            page_starts: vec![0, 7],
            images: vec![EmbeddedImage {
                media_type: "image/png",
                bytes: vec![1, 2, 3],
                alt: None,
            }],
            ocr_pages: vec![2],
            omitted_pages: vec![4],
            notes: vec!["n".into()],
        };
        let sheets = vec![sparkles_office::Sheet {
            name: "S".into(),
            index: 1,
            csv: "a,b\n".into(),
            rows: 1,
            columns: 2,
        }];
        for r in [Err(f), Ok(Reply::Text(text)), Ok(Reply::Sheets(sheets))] {
            let b = encode(&r);
            assert_eq!(decode(&b), Some(r));
            // every truncation and an extra byte are refused, never a panic
            for n in 0..b.len() {
                assert_eq!(decode(&b[..n]), None, "{n}");
            }
            let mut long = b.clone();
            long.push(0);
            assert_eq!(decode(&long), None);
        }
        // a count far larger than the reply does not allocate
        let mut b = vec![TAG_SHEETS];
        b.extend_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(decode(&b), None);
    }

    // A child that never answers is killed at the deadline, and the call returns then.
    #[cfg(feature = "office")]
    #[test]
    fn a_hung_child_is_killed_at_the_deadline() {
        let o = options(Duration::from_millis(300), Default::default());
        let started = Instant::now();
        let r = run(&shell("sleep 30"), JOB, b"x", &o);
        let e = r.unwrap_err().unwrap();
        assert_eq!(e.code, "conversion-timeout");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
    }

    // Output past the reply's cap is refused and the child killed, whatever it writes.
    #[cfg(feature = "office")]
    #[test]
    fn output_past_the_cap_is_refused() {
        let o = options(Duration::from_secs(20), Default::default());
        let r = run(
            &shell("head -c 100000000 /dev/zero; sleep 30"),
            JOB,
            b"x",
            &o,
        );
        assert_eq!(r.unwrap_err().unwrap().code, "too-large");
    }

    #[cfg(feature = "office")]
    #[test]
    fn a_child_that_dies_is_a_failed_conversion() {
        let o = options(Duration::from_secs(20), Default::default());
        let r = run(&shell("kill -SEGV $$"), JOB, b"x", &o);
        let e = r.unwrap_err().unwrap();
        assert_eq!(e.code, "conversion-failed");
        assert!(e.message.contains("memory limit"), "{}", e.message);
        let r = run(&shell("exit 101"), JOB, b"x", &o);
        assert_eq!(r.unwrap_err().unwrap().code, "conversion-failed");
        let r = run(&shell("printf garbage"), JOB, b"x", &o);
        assert!(r.unwrap_err().unwrap().message.contains("damaged"));
    }

    // The child's address space and file size are limited.
    #[test]
    fn the_child_runs_under_its_limits() {
        let iso = shell("ulimit -v; ulimit -f; cat >/dev/null");
        let mut cmd = Command::new(&iso.exe);
        cmd.args(&iso.prefix)
            .stdout(Stdio::piped())
            .stdin(Stdio::null());
        limit(&mut cmd, 256 << 20, Duration::from_secs(5), true);
        let out = cmd.output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "262144\n0\n");
    }

    #[cfg(feature = "office")]
    /// The `sparkles` binary next to this test binary, which `cargo test` builds for the
    /// integration tests.
    fn binary() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let b = exe.parent()?.parent()?.join("sparkles");
        b.is_file().then_some(b)
    }

    // The same document converts to the same text in a child of the real binary and in
    // this process.
    #[cfg(feature = "office")]
    #[test]
    fn the_binary_converts_in_a_child() {
        let Some(exe) = binary() else {
            eprintln!("skipped: no sparkles binary next to the test binary");
            return;
        };
        let docx = sparkles_office::fixtures::docx();
        let iso = Isolation {
            exe,
            memory: 512 << 20,
            prefix: Vec::new(),
        };
        let rt = super::super::PdfRuntime::new(1, None).with_isolation(Some(iso));
        let mut o = options(Duration::from_secs(60), rt);
        o.max_text = 1 << 20;
        let child = super::super::office::convert(&docx, Format::Docx, &o).unwrap();
        let here = super::super::office::convert_here(&docx, Format::Docx, &o).unwrap();
        assert_eq!(child, here);
        assert!(!child.text.is_empty());
    }

    // A PDF, a refused scanned PDF and the sheets of a workbook come back from the child
    // as they come from this process.
    #[cfg(all(feature = "pdf", feature = "office"))]
    #[test]
    fn the_binary_converts_pdfs_and_sheets_in_a_child() {
        let Some(exe) = binary() else {
            eprintln!("skipped: no sparkles binary next to the test binary");
            return;
        };
        let iso = Isolation {
            exe,
            memory: 1 << 30,
            prefix: Vec::new(),
        };
        let rt = super::super::PdfRuntime::new(1, None).with_isolation(Some(iso));
        let mut o = options(Duration::from_secs(60), rt);
        o.max_text = 1 << 20;
        o.max_input = 16 << 20;
        let here_rt = std::sync::Arc::new(super::super::PdfRuntime::new(1, None));
        for pdf in [
            super::super::fixtures::report(),
            super::super::fixtures::scanned(),
        ] {
            let child = super::super::pdf::convert(&pdf, &o);
            let here = super::super::pdf::inspect(&pdf, false, &here_rt);
            assert_eq!(child, here);
        }
        let xlsx = sparkles_office::fixtures::xlsx();
        let child = super::super::office::sheets(&xlsx, Format::Xlsx, &o).unwrap();
        let here = super::super::office::sheets_here(&xlsx, Format::Xlsx, &o).unwrap();
        assert_eq!(child, here);
        assert!(!child.is_empty());
    }

    // The cost of a child: `SPARKLES_CHILD_EXE=target/release/sparkles cargo test
    // --bin sparkles overhead -- --ignored --nocapture` converts a small DOCX 200 times
    // in a child and prints the median and the 90th percentile.
    #[cfg(feature = "office")]
    #[test]
    #[ignore = "a measurement, run by hand"]
    fn overhead() {
        let exe = std::env::var_os("SPARKLES_CHILD_EXE")
            .map(PathBuf::from)
            .or_else(binary)
            .expect("a sparkles binary");
        let docx = sparkles_office::fixtures::docx();
        let iso = Isolation {
            exe,
            memory: 2 << 30,
            prefix: Vec::new(),
        };
        let rt = super::super::PdfRuntime::new(1, None).with_isolation(Some(iso.clone()));
        let mut o = options(Duration::from_secs(60), rt);
        o.max_text = 1 << 20;
        let mut times = Vec::new();
        for _ in 0..200 {
            let t = Instant::now();
            run(&iso, JOB, &docx, &o).unwrap();
            times.push(t.elapsed());
        }
        times.sort();
        println!(
            "child: median {:?}, p90 {:?}, min {:?}",
            times[100], times[180], times[0]
        );
    }
}
