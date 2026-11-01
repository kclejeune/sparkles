//! PDF and Office conversions in a child process (spec C18 §7.1.1).
//!
//! A converter reads a file that comes from outside. The converters bound what they
//! allocate and check the task's deadline, but a parser bug could still loop or allocate
//! without a check, and a thread cannot be stopped from outside. So the `sparkles` binary
//! on Linux runs each conversion in a child process of its own: the same binary, started
//! through `/proc/self/exe` with the hidden first argument [`CHILD_ARG`], the file on its
//! standard input and the result on its standard output. The child sets `RLIMIT_AS`
//! (`--convert-memory`), a CPU limit, `RLIMIT_FSIZE` of 0 (unless OCR is configured, whose
//! libraries are not audited for it), no core file and the parent-death signal on itself
//! before it reads a byte of the file, and refuses to run without them, so that the
//! server can start it with `posix_spawn`. The parent kills it when the task's deadline
//! passes, so a conversion never outlives its deadline by more than [`GRACE`], its worker
//! slot is always released, and its address space is bounded whatever the converter does.
//! The parent reads at most a bounded number of bytes of output.
//!
//! This is resource containment and not a sandbox. The child runs as the server's user
//! with the server's privileges. Its environment is cleared down to a short allowlist, so
//! that it holds no provider key or token, but it could still read what that user can.
//!
//! Only the server binary can serve as the child, so the child process is used only when
//! `main` has called [`enable`], and only on Linux, where `RLIMIT_AS` is enforced. Library
//! use of the crate, its unit tests, other platforms and `--convert-in-process` run the
//! conversion on a thread of the calling process. A server checks at start that a child
//! can set its limits, and refuses conversions with a clear error when it cannot.
//!
//! The reply starts with [`MAGIC`] and the protocol version, then a tag byte and fields
//! that are each a little-endian `u64` or a length-prefixed byte string. Nothing in it is
//! trusted beyond its framing: a reply that cannot be read is a failed conversion.

use super::convert::{Converted, EmbeddedImage, Format, Options, PageReason, Refusal};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The first argument that makes the binary a conversion child.
pub const CHILD_ARG: &str = "__convert-child";

/// The version of the arguments and the reply. A child of another version refuses to run.
pub const PROTOCOL: u32 = 1;

/// The first bytes of a reply, before the protocol version.
pub const MAGIC: &[u8; 4] = b"SPKC";

/// Exit codes of a child that wrote no reply.
const EXIT_INPUT: i32 = 2;
const EXIT_LIMITS: i32 = 3;
const EXIT_PROTOCOL: i32 = 4;
const EXIT_THREADS: i32 = 5;

/// The most threads of the child's rayon pool, which the PDF parser's parallel loops use.
/// Without a bound the pool starts a thread per core, and each thread reserves its stack
/// out of the address-space limit.
const CHILD_THREADS: usize = 4;

/// The most malloc arenas glibc makes in the child. Every new thread calls
/// `pthread_getattr_np` when Rust's standard library installs its stack-overflow handler,
/// which allocates with glibc's malloc, and glibc then reserves an arena of 64 MiB of
/// address space for that thread. On a host with 20 cores that took 1.3 GiB of a 1 GiB
/// limit before the converter read a byte. The child's own allocations go through
/// mimalloc, so few arenas cost it nothing.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
const CHILD_MALLOC_ARENAS: libc::c_int = 2;

/// How many bytes of the child's standard error a refusal quotes.
const STDERR_QUOTE: usize = 400;

/// The variables a child keeps from the server's environment. Everything else, provider
/// keys and tokens included, is left out.
const ENV_ALLOW: &[&str] = &[
    "PATH",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "TMPDIR",
    "LD_LIBRARY_PATH",
    "PDFIUM_LIB_PATH",
    "PDFIUM_DYNAMIC_LIB_PATH",
    "ORT_DYLIB_PATH",
];

/// The environment of a child, from the server's.
pub fn child_env(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    let mut out: Vec<(OsString, OsString)> = vars
        .into_iter()
        .filter(|(k, _)| k.to_str().is_some_and(|k| ENV_ALLOW.contains(&k)))
        .collect();
    // mimalloc reserves 1 GiB of address space when it starts, before the child can set
    // its limit, which would leave the child no room under a smaller limit
    out.push(("MIMALLOC_ARENA_RESERVE".into(), "64MiB".into()));
    out
}

/// How long past the task's deadline the parent waits for the child, which checks the
/// same deadline itself and then reports a timeout of its own.
pub const GRACE: Duration = Duration::from_millis(250);

static EXE: OnceLock<PathBuf> = OnceLock::new();

/// Let conversions run in child processes of this binary. Called by `main` of the
/// `sparkles` binary only, which answers [`CHILD_ARG`]. On Linux the child is started
/// through `/proc/self/exe`, which names the file the server was started from even after
/// an upgrade replaces or deletes it.
pub fn enable() {
    let exe = if cfg!(target_os = "linux") && std::path::Path::new("/proc/self/exe").exists() {
        Some(PathBuf::from("/proc/self/exe"))
    } else {
        std::env::current_exe().ok()
    };
    if let Some(exe) = exe {
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
    /// nothing: the child sets its limits and answers, which a server checks at start
    Probe,
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
        Job::Probe => 1 << 16,
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
        Job::Probe => vec!["job=probe".into()],
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
        .arg(format!("protocol={PROTOCOL}"))
        .env_clear()
        .envs(child_env(std::env::vars_os()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(feature = "pdf-ocr")]
    if let Some(c) = ocr {
        cmd.arg(OsString::from("ocr_models=").tap_push(&c.models));
        if let Some(p) = &c.pdfium {
            cmd.arg(OsString::from("pdfium=").tap_push(p));
        }
    }
    cmd.args(limit_args(iso.memory, left, ocr.is_none()));
    // the child leads a process group of its own, so that a kill at the deadline also
    // ends any process it started, which could otherwise hold the pipes open
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
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
    let mut stderr = child.stderr.take().expect("piped");
    let cap = reply_cap(job, o);
    let wait_until = o.deadline + GRACE;
    let (tx, rx) = std::sync::mpsc::channel();
    let mut killed = false;
    let mut err_text = (Vec::new(), Vec::new());
    let outcome = std::thread::scope(|s| {
        // the first 2 KiB of the child's errors are kept for the log and its last bytes
        // for the refusal, and the rest is drained so that the child never blocks on a
        // full pipe
        let err_reader = s.spawn(move || read_stderr(&mut stderr));
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
                            kill(&mut child);
                            killed = true;
                            break child.wait().ok();
                        }
                    }
                }
            }
            _ => {
                kill(&mut child);
                killed = true;
                child.wait().ok()
            }
        };
        err_text = err_reader.join().unwrap_or_default();
        (got, status)
    });
    let (got, status) = outcome;
    // a refusal for a child that failed quotes the end of what it wrote to its standard
    // error, such as a panic's message, and the log keeps all of the first 2 KiB
    let failure = |what: &str, r: Refusal| {
        tracing::warn!(
            status = ?status,
            stderr = %String::from_utf8_lossy(&err_text.0),
            "a conversion child {what}"
        );
        with_stderr(r, &err_text.1)
    };
    match got {
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(Some(timeout())),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(Some(failure(
            "could not be read",
            Refusal::new(
                "conversion-failed",
                500,
                "the conversion child's output could not be read",
            ),
        ))),
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
                let r = ended(status, killed, iso.memory);
                if r.code == "conversion-timeout" {
                    return Err(Some(r));
                }
                return Err(Some(failure("ended without a result", r)));
            }
            if out.len() < 8 || &out[..4] != MAGIC || out[4..8] != PROTOCOL.to_le_bytes() {
                return Err(Some(failure("answered in another protocol", mismatch())));
            }
            match decode(&out[8..]) {
                Some(Ok(r)) => Ok(r),
                Some(Err(r)) => Err(Some(r)),
                None => Err(Some(failure(
                    "wrote a damaged reply",
                    failed("the document could not be read: the converter's reply was damaged"),
                ))),
            }
        }
    }
}

/// Start a child that only sets its limits and answers, as a server does once when it
/// starts. `Err` names why conversions cannot run in a child here.
pub fn probe(iso: &Isolation) -> Result<(), String> {
    let o = Options {
        allow_partial: false,
        max_input: 0,
        max_text: 0,
        deadline: Instant::now() + Duration::from_secs(10),
        pdf: Default::default(),
        max_images: 0,
    };
    match run(iso, Job::Probe, b"", &o) {
        Ok(Reply::Text(c)) if c.text == "ready" => Ok(()),
        Ok(_) => Err("the conversion child answered the probe wrongly".into()),
        Err(Some(r)) => Err(r.message),
        Err(None) => Err(format!("{} could not be started", iso.exe.display())),
    }
}

/// How many bytes of the start of the child's standard error the log keeps, and of its end
/// the parent keeps for [`with_stderr`], which quotes less than that once it has left out
/// Rust's backtrace hint.
const STDERR_HEAD: usize = 2048;
const STDERR_TAIL: usize = 1024;

/// Read the child's standard error to its end, and return its first [`STDERR_HEAD`] and
/// last [`STDERR_TAIL`] bytes.
fn read_stderr(r: &mut impl Read) -> (Vec<u8>, Vec<u8>) {
    let mut head = Vec::new();
    let mut tail = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let chunk = &buf[..n];
        let room = STDERR_HEAD.saturating_sub(head.len());
        head.extend_from_slice(&chunk[..room.min(n)]);
        tail.extend_from_slice(chunk);
        if tail.len() > STDERR_TAIL {
            tail.drain(..tail.len() - STDERR_TAIL);
        }
    }
    (head, tail)
}

/// `r` with the end of the child's standard error `err` quoted, when it wrote any. Rust's
/// hint to set `RUST_BACKTRACE` is left out, and the lines are joined into one.
fn with_stderr(mut r: Refusal, err: &[u8]) -> Refusal {
    let text = String::from_utf8_lossy(err);
    let line = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("note: run with `RUST_BACKTRACE"))
        .collect::<Vec<_>>()
        .join(" ");
    let line: String = line
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if line.is_empty() {
        return r;
    }
    let mut start = line.len().saturating_sub(STDERR_QUOTE);
    while !line.is_char_boundary(start) {
        start += 1;
    }
    let cut = if start > 0 { "..." } else { "" };
    r.message = format!("{}; the child reported: {cut}{}", r.message, &line[start..]);
    r
}

fn mismatch() -> Refusal {
    Refusal::new(
        "conversion-failed",
        500,
        "the conversion child is another version of sparkles than the server: restart the server",
    )
}

/// Kill the child and every process in its group.
fn kill(child: &mut std::process::Child) {
    // the child is not reaped yet, so its id still names its group
    #[cfg(unix)]
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: kill takes no pointers, and a negative pid names a process group
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// Why a child that wrote nothing ended. `killed` says that the parent killed it.
fn ended(status: Option<std::process::ExitStatus>, killed: bool, memory: u64) -> Refusal {
    let unsafe_start = |why: &str| {
        Refusal::new(
            "conversion-failed",
            500,
            format!("the conversion child could not start safely: {why}"),
        )
    };
    match status.and_then(|s| s.code()) {
        Some(EXIT_LIMITS) => {
            return unsafe_start("it could not set its resource limits");
        }
        Some(EXIT_PROTOCOL) => return mismatch(),
        Some(EXIT_THREADS) => {
            return Refusal::new(
                "conversion-failed",
                500,
                format!(
                    "the conversion child could not start its threads under its memory limit \
                     of {} MiB: raise --convert-memory",
                    memory >> 20
                ),
            );
        }
        Some(EXIT_INPUT) => return unsafe_start("it could not read the document"),
        _ => {}
    }
    #[cfg(unix)]
    if let Some(sig) = status.and_then(|s| std::os::unix::process::ExitStatusExt::signal(&s)) {
        // its own CPU limit, or the parent at the deadline
        if sig == libc::SIGXCPU || (sig == libc::SIGKILL && killed) {
            return timeout();
        }
        // a SIGKILL from elsewhere is the kernel's or a cgroup's memory limit
        return failed(format!(
            "the document could not be read: the converter stopped on it (signal {sig}), \
             which it also does when the document needs more than its memory limit of {} MiB",
            memory >> 20
        ));
    }
    let _ = (killed, memory);
    failed("the document could not be read: the converter stopped on it")
}

/// The limits of the child, as arguments that the child applies to itself before it
/// reads its input. Setting them in the child instead of between `fork` and `exec` lets
/// the server start it with `posix_spawn`, which does not copy the server's page tables.
fn limit_args(memory: u64, left: Duration, no_files: bool) -> Vec<String> {
    // a backstop for a parent that cannot kill the child: CPU time of every thread, so
    // that a converter using all cores is not stopped before its wall-clock deadline
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as u64;
    let cpu = (left.as_secs() + 1).saturating_mul(cores).saturating_add(1);
    vec![
        format!("memory={memory}"),
        format!("cpu={cpu}"),
        format!("files={}", u8::from(!no_files)),
        format!("parent={}", std::process::id()),
    ]
}

/// Apply the limits of [`limit_args`] to this process, or fail.
#[cfg(unix)]
fn apply_limits(memory: u64, cpu: u64, files: bool, parent: u32) -> std::io::Result<()> {
    let set = |res, soft: u64, hard: u64| {
        let l = libc::rlimit {
            rlim_cur: soft as libc::rlim_t,
            rlim_max: hard as libc::rlim_t,
        };
        // SAFETY: setrlimit only reads the struct
        if unsafe { libc::setrlimit(res, &l) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    };
    if memory == 0 || cpu == 0 {
        return Err(std::io::Error::other(
            "the child was started without limits",
        ));
    }
    set(libc::RLIMIT_AS, memory, memory)?;
    set(libc::RLIMIT_CPU, cpu, cpu + 1)?;
    set(libc::RLIMIT_CORE, 0, 0)?;
    if !files {
        set(libc::RLIMIT_FSIZE, 0, 0)?;
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: prctl with these arguments only sets the signal sent at the parent's death
        if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // the parent may have died before the call above
        // SAFETY: getppid has no preconditions
        if unsafe { libc::getppid() } as u32 != parent {
            return Err(std::io::Error::other("the server has ended"));
        }
    }
    let _ = parent;
    Ok(())
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
    // the limits come first, before any byte of the file is read
    #[cfg(unix)]
    if apply_limits(
        num("memory") as u64,
        num("cpu") as u64,
        num("files") == 1,
        num("parent") as u32,
    )
    .is_err()
    {
        return EXIT_LIMITS;
    }
    if num("protocol") != PROTOCOL as usize {
        return EXIT_PROTOCOL;
    }
    // the threads start under the limits, so that a probe finds a limit they do not fit
    if let Err(e) = start_threads() {
        let _ = writeln!(
            std::io::stderr(),
            "the conversion child could not start its threads: {e}"
        );
        return EXIT_THREADS;
    }
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
        return EXIT_INPUT;
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
    let mut reply = MAGIC.to_vec();
    reply.extend_from_slice(&PROTOCOL.to_le_bytes());
    reply.extend_from_slice(&encode(&r));
    let mut out = std::io::stdout().lock();
    if out.write_all(&reply).and_then(|_| out.flush()).is_err() {
        return EXIT_INPUT;
    }
    0
}

/// Bound the threads of the child before it starts any: glibc's malloc arenas, and the
/// global rayon pool, which would otherwise start at its first parallel loop with a thread
/// per core, or as many as `RAYON_NUM_THREADS` asks for, and panic when they do not fit.
fn start_threads() -> Result<(), rayon::ThreadPoolBuildError> {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: mallopt only sets a parameter of glibc's malloc, and no thread is running
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, CHILD_MALLOC_ARENAS);
    }
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    rayon::ThreadPoolBuilder::new()
        .num_threads(cores.min(CHILD_THREADS))
        .thread_name(|i| format!("convert-{i}"))
        .build_global()
}

fn convert_here(
    job: &str,
    format: Option<Format>,
    bytes: &[u8],
    o: &Options,
) -> Result<Reply, Refusal> {
    let _ = (bytes, o);
    match (job, format) {
        ("probe", _) => Ok(Reply::Text(Converted {
            text: "ready".into(),
            ..Default::default()
        })),
        #[cfg(feature = "office")]
        ("text", Some(f)) => super::office::convert_here(bytes, f, o).map(Reply::Text),
        #[cfg(feature = "office")]
        ("sheets", Some(f)) => super::office::sheets_here(bytes, f, o).map(Reply::Sheets),
        #[cfg(feature = "pdf")]
        ("pdf", _) => super::pdf::inspect(bytes, o.allow_partial, &o.pdf).map(Reply::Text),
        // the tests read the environment and the limits that the child runs under
        #[cfg(debug_assertions)]
        ("env", _) => Err(failed(
            std::env::vars_os()
                .map(|(k, _)| k.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" "),
        )),
        #[cfg(all(debug_assertions, unix))]
        ("limits", _) => {
            let get = |res| {
                let mut l = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                // SAFETY: getrlimit only writes the struct
                unsafe { libc::getrlimit(res, &mut l) };
                l.rlim_cur
            };
            Err(failed(format!(
                "as={} fsize={} core={}",
                get(libc::RLIMIT_AS),
                get(libc::RLIMIT_FSIZE),
                get(libc::RLIMIT_CORE)
            )))
        }
        // the tests start threads in the child to find what each one costs it
        #[cfg(debug_assertions)]
        ("threads", _) => {
            let n = o.max_text;
            let started: Vec<_> = (0..n)
                .map(|_| {
                    std::thread::Builder::new().spawn(|| {
                        std::thread::sleep(Duration::from_millis(200));
                    })
                })
                .collect();
            let ok = started
                .into_iter()
                .filter_map(Result::ok)
                .fold(0, |n, h| n + usize::from(h.join().is_ok()));
            Err(failed(format!(
                "started={ok} pool={}",
                rayon::current_num_threads()
            )))
        }
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

#[cfg(all(test, unix, feature = "office"))]
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
    #[cfg(feature = "office")]
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

    // A child whose shell forks the hung command, as dash does, is killed with the
    // command, so the grandchild cannot hold the pipes open past the deadline.
    #[cfg(feature = "office")]
    #[test]
    fn a_hung_grandchild_is_killed_at_the_deadline() {
        let o = options(Duration::from_millis(300), Default::default());
        let started = Instant::now();
        let r = run(&shell("sleep 30; true"), JOB, b"x", &o);
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
        // a SIGKILL that the parent did not send is a memory limit, not a timeout
        let r = run(&shell("kill -KILL $$"), JOB, b"x", &o);
        let e = r.unwrap_err().unwrap();
        assert_eq!(e.code, "conversion-failed");
        assert!(e.message.contains("memory limit"), "{}", e.message);
        let r = run(&shell("exit 101"), JOB, b"x", &o);
        assert_eq!(r.unwrap_err().unwrap().code, "conversion-failed");
        // the refusal quotes the end of the child's errors, without Rust's backtrace hint
        let script = "printf 'thread panicked at x.rs:1:2:\\nno pool\\n\
                      note: run with `RUST_BACKTRACE=1` to display a backtrace\\n' >&2; exit 101";
        let e = run(&shell(script), JOB, b"x", &o).unwrap_err().unwrap();
        assert!(
            e.message
                .ends_with("the child reported: thread panicked at x.rs:1:2: no pool"),
            "{}",
            e.message
        );
        // a panic after more than the log's 2 KiB of other errors is still quoted, and a
        // long message only by its end
        let long = format!(
            "printf '%s\\n' {} >&2; printf 'last words\\n' >&2; exit 101",
            "y".repeat(5000)
        );
        let e = run(&shell(&long), JOB, b"x", &o).unwrap_err().unwrap();
        let quote = format!("{} last words", "y".repeat(STDERR_QUOTE - 11));
        assert!(
            e.message.ends_with(&format!("reported: ...{quote}")),
            "{}",
            e.message
        );
        let r = run(&shell("exit 5"), JOB, b"x", &o).unwrap_err().unwrap();
        assert!(r.message.contains("--convert-memory"), "{}", r.message);
        // a child that could not set its limits, or speaks another protocol, is named
        let r = run(&shell("exit 3"), JOB, b"x", &o).unwrap_err().unwrap();
        assert_eq!(r.status, 500);
        assert!(r.message.contains("resource limits"), "{}", r.message);
        let r = run(&shell("exit 4"), JOB, b"x", &o).unwrap_err().unwrap();
        assert!(r.message.contains("another version"), "{}", r.message);
        let r = run(&shell("printf garbage"), JOB, b"x", &o)
            .unwrap_err()
            .unwrap();
        assert!(r.message.contains("another version"), "{}", r.message);
        let r = run(&shell("printf 'SPKC\\001\\000\\000\\000x'"), JOB, b"x", &o);
        assert!(r.unwrap_err().unwrap().message.contains("damaged"));
    }

    /// The `sparkles` binary next to this test binary. `cargo test` builds it for the
    /// integration tests, and a test that needs it fails without it rather than pass.
    fn binary() -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let b = exe.parent().unwrap().parent().unwrap().join("sparkles");
        assert!(
            b.is_file(),
            "{} is missing: build it with `cargo build -p sparkles-server` or run `cargo test -p sparkles-server`, which builds it",
            b.display()
        );
        b
    }

    /// The reply of a child of the real binary started with `args`, without its header.
    fn child_reply(args: &[String], env: Vec<(OsString, OsString)>) -> std::process::Output {
        child_reply_to(args, env, b"")
    }

    /// The reply of a child of the real binary started with `args` and `input`.
    fn child_reply_to(
        args: &[String],
        env: Vec<(OsString, OsString)>,
        input: &[u8],
    ) -> std::process::Output {
        let mut child = Command::new(binary())
            .arg(CHILD_ARG)
            .args(args)
            .env_clear()
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let input = input.to_vec();
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
        let out = child.wait_with_output().unwrap();
        writer.join().unwrap();
        out
    }

    fn refusal_of(out: &std::process::Output) -> Refusal {
        assert!(out.stdout.starts_with(MAGIC), "{out:?}");
        match decode(&out.stdout[8..]) {
            Some(Err(f)) => f,
            _ => panic!("{out:?}"),
        }
    }

    fn base_args() -> Vec<String> {
        let mut a = limit_args(300 << 20, Duration::from_secs(5), true);
        a.extend([
            "max_input=10".into(),
            "millis=5000".into(),
            format!("protocol={PROTOCOL}"),
        ]);
        a
    }

    // The child of the real binary limits its address space, file size and core files
    // before it reads its input.
    #[test]
    fn the_child_runs_under_its_limits() {
        let mut args = base_args();
        args.push("job=limits".into());
        let out = child_reply(&args, child_env([]));
        let f = refusal_of(&out);
        assert_eq!(f.message, format!("as={} fsize=0 core=0", 300u64 << 20));
        // a child without limits refuses to start
        let out = child_reply(
            &[
                "job=limits".into(),
                "max_input=10".into(),
                "millis=5000".into(),
            ],
            child_env([]),
        );
        assert_eq!(out.status.code(), Some(EXIT_LIMITS));
        assert!(out.stdout.is_empty());
        // and so does a child of another protocol
        let mut args = base_args();
        args.push("protocol=999".into());
        let out = child_reply(&args, child_env([]));
        assert_eq!(out.status.code(), Some(EXIT_PROTOCOL));
    }

    // The child keeps only the allowlisted variables of the server's environment.
    #[test]
    fn the_child_gets_no_secrets_from_the_environment() {
        let server: Vec<(OsString, OsString)> = vec![
            ("SPARKLES_TEST_SECRET".into(), "sk-marker".into()),
            ("ANTHROPIC_API_KEY".into(), "sk-marker".into()),
            ("TZ".into(), "UTC".into()),
        ];
        let env = child_env(server);
        let mut args = base_args();
        args.push("job=env".into());
        let out = child_reply(&args, env);
        let names = refusal_of(&out).message;
        let names: Vec<&str> = names.split(' ').collect();
        assert!(names.contains(&"TZ"), "{names:?}");
        assert!(names.contains(&"MIMALLOC_ARENA_RESERVE"), "{names:?}");
        assert!(!names.contains(&"SPARKLES_TEST_SECRET"), "{names:?}");
        assert!(!names.contains(&"ANTHROPIC_API_KEY"), "{names:?}");
    }

    // Threads cost the child their stacks and no malloc arena of 64 MiB each, so 64 of
    // them fit under 1 GiB, and its rayon pool stays small whatever the cores or
    // `RAYON_NUM_THREADS` ask for.
    #[test]
    fn the_child_bounds_what_its_threads_reserve() {
        let mut args = limit_args(1 << 30, Duration::from_secs(5), true);
        args.extend([
            "max_input=10".into(),
            "max_text=64".into(),
            "millis=5000".into(),
            format!("protocol={PROTOCOL}"),
            "job=threads".into(),
        ]);
        let mut env = child_env([]);
        env.push(("RAYON_NUM_THREADS".into(), "64".into()));
        let out = child_reply(&args, env);
        let f = refusal_of(&out);
        let pool: usize = f.message.rsplit('=').next().unwrap().parse().unwrap();
        assert!(f.message.starts_with("started=64 "), "{}", f.message);
        assert!((1..=CHILD_THREADS).contains(&pool), "{}", f.message);
    }

    // A PDF converts in a child under 1 GiB however many threads rayon would start on
    // its own. With a thread per core, each thread's malloc arena filled the limit on a
    // host with 20 cores, and the child panicked when the parser started the pool.
    #[cfg(feature = "pdf")]
    #[test]
    fn a_pdf_converts_in_a_child_on_a_host_with_many_cores() {
        let mut args = limit_args(1 << 30, Duration::from_secs(30), true);
        let pdf = super::super::fixtures::report();
        args.extend([
            format!("max_input={}", pdf.len()),
            "max_text=1048576".into(),
            "millis=30000".into(),
            format!("protocol={PROTOCOL}"),
            "job=pdf".into(),
            "partial=0".into(),
        ]);
        let mut env = child_env([]);
        env.push(("RAYON_NUM_THREADS".into(), "64".into()));
        let out = child_reply_to(&args, env, &pdf);
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        let here_rt = std::sync::Arc::new(super::super::PdfRuntime::new(1, None));
        let here = super::super::pdf::inspect(&pdf, false, &here_rt).map(Reply::Text);
        assert!(out.stdout.starts_with(MAGIC), "{out:?}");
        assert_eq!(decode(&out.stdout[8..]), Some(here));
    }

    // The startup probe passes for the real binary and names the reason for another.
    #[test]
    fn the_probe_checks_the_child() {
        let iso = Isolation {
            exe: binary(),
            memory: 512 << 20,
            prefix: Vec::new(),
        };
        probe(&iso).unwrap();
        let e = probe(&shell("exit 3")).unwrap_err();
        assert!(e.contains("resource limits"), "{e}");
        // a server whose probe failed refuses conversions with that reason
        let rt = super::super::PdfRuntime::new(1, None).with_broken(Some(e));
        let o = options(Duration::from_secs(20), rt);
        let docx = sparkles_office::fixtures::docx();
        let r = super::super::office::convert(&docx, Format::Docx, &o).unwrap_err();
        assert_eq!(r.status, 500);
        assert!(r.message.contains("--convert-in-process"), "{}", r.message);
    }

    // The same document converts to the same text in a child of the real binary and in
    // this process.
    #[test]
    fn the_binary_converts_in_a_child() {
        let docx = sparkles_office::fixtures::docx();
        let iso = Isolation {
            exe: binary(),
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
    #[cfg(feature = "pdf")]
    #[test]
    fn the_binary_converts_pdfs_and_sheets_in_a_child() {
        let iso = Isolation {
            exe: binary(),
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
            .unwrap_or_else(binary);
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
