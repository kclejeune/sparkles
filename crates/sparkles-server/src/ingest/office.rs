//! Office documents (spec C18 §7.1): DOCX, PPTX and the OpenDocument text and
//! presentation formats become Markdown, and a spreadsheet becomes one CSV text per
//! sheet, which the mapping draft of §7.8 takes one at a time. The reading is
//! `sparkles-office`, which bounds every size of the file.
//!
//! Like a PDF, the file comes from outside, so it is read under the task's deadline in a
//! child process of the server ([`super::isolate`]), or on a thread of its own when there
//! is none, with a panic caught as `conversion-failed`. At most `--pdf-workers`
//! conversions of PDF and Office files run at once.

use super::PdfRuntime;
use super::convert::{Converted, EmbeddedImage, Format, Options, Refusal};
use super::isolate::{self, Job, Reply};
use crate::mcp::memory::ingest::normalize;
use sparkles_office::{Error, Limits};
use std::sync::mpsc;

fn format_of(f: Format) -> sparkles_office::Format {
    match f {
        Format::Docx => sparkles_office::Format::Docx,
        Format::Pptx => sparkles_office::Format::Pptx,
        Format::Xlsx => sparkles_office::Format::Xlsx,
        Format::Odt => sparkles_office::Format::Odt,
        Format::Odp => sparkles_office::Format::Odp,
        _ => sparkles_office::Format::Ods,
    }
}

fn limits(o: &Options) -> Limits {
    Limits {
        max_text: o.max_text,
        max_images: o.max_images,
        // the conversion stops at the task's deadline, so that the thread the task has
        // given up on ends soon after and frees its worker slot
        deadline: Some(o.deadline),
        ..Limits::default()
    }
}

fn refusal(e: Error) -> Refusal {
    match e {
        Error::Corrupt(m) => Refusal::new(
            "conversion-failed",
            422,
            format!("the document could not be read: {m}"),
        ),
        Error::Encrypted => Refusal::new(
            "conversion-failed",
            422,
            "the document is encrypted with a password",
        ),
        Error::TooLarge(m) => Refusal::new("too-large", 413, m),
        Error::Unsupported(m) => Refusal::new("unsupported-format", 415, m),
        Error::Timeout => Refusal::new(
            "conversion-timeout",
            504,
            "the document's conversion did not end before the deadline",
        ),
    }
}

/// Run `job` in a child process, or `f` on a worker thread when the server has no child
/// process to use, under `o`'s deadline.
fn run<T: Send + 'static>(
    bytes: &[u8],
    o: &Options,
    job: Job,
    from_child: impl FnOnce(Reply) -> Option<T>,
    f: impl FnOnce(&[u8]) -> Result<T, Error> + Send + 'static,
) -> Result<T, Refusal> {
    let Some(slot) = PdfRuntime::acquire(&o.pdf, o.deadline) else {
        return Err(Refusal::new(
            "conversion-timeout",
            504,
            "every conversion worker stayed busy until the deadline",
        ));
    };
    if let Some(r) = o.pdf.child_refusal() {
        return Err(r);
    }
    if let Some(iso) = o.pdf.isolation() {
        match isolate::run(iso, job, bytes, o) {
            Ok(r) => {
                return from_child(r).ok_or_else(|| {
                    Refusal::new(
                        "conversion-failed",
                        422,
                        "the conversion child answered with the wrong kind of result",
                    )
                });
            }
            Err(Some(r)) => return Err(r),
            // no child could be started: convert in process
            Err(None) => {}
        }
    }
    let bytes = bytes.to_vec();
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("office-convert".into())
        // the trees of nested XML are walked recursively, to the depth the limits allow
        .stack_size(16 << 20)
        .spawn(move || {
            let _slot = slot;
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&bytes)));
            let _ = tx.send(r);
        });
    if spawned.is_err() {
        return Err(Refusal::new(
            "conversion-failed",
            500,
            "no thread could be started for the conversion",
        ));
    }
    let left = o
        .deadline
        .saturating_duration_since(std::time::Instant::now());
    match rx.recv_timeout(left) {
        Ok(Ok(r)) => r.map_err(refusal),
        Ok(Err(_)) => Err(Refusal::new(
            "conversion-failed",
            422,
            "the document could not be read: the converter stopped on it",
        )),
        Err(_) => Err(Refusal::new(
            "conversion-timeout",
            504,
            "the document's conversion did not end before the deadline",
        )),
    }
}

/// A text document or a presentation as the text of a rendition.
pub fn convert(bytes: &[u8], format: Format, o: &Options) -> Result<Converted, Refusal> {
    let f = format_of(format);
    let l = limits(o);
    run(
        bytes,
        o,
        Job::Text(format),
        |r| match r {
            Reply::Text(c) => Some(c),
            Reply::Sheets(_) => None,
        },
        move |b| sparkles_office::to_markdown(b, f, &l).map(|d| converted(d, format)),
    )
}

/// The conversion in this process, as the child runs it.
pub fn convert_here(bytes: &[u8], format: Format, o: &Options) -> Result<Converted, Refusal> {
    let d = sparkles_office::to_markdown(bytes, format_of(format), &limits(o)).map_err(refusal)?;
    Ok(converted(d, format))
}

/// The sheets in this process, as the child reads them.
pub fn sheets_here(
    bytes: &[u8],
    format: Format,
    o: &Options,
) -> Result<Vec<sparkles_office::Sheet>, Refusal> {
    sparkles_office::sheets(bytes, format_of(format), &limits(o)).map_err(refusal)
}

fn converted(d: sparkles_office::Document, format: Format) -> Converted {
    Converted {
        text: normalize(&d.markdown),
        media_type: format.media_type().into(),
        title: d.title,
        images: d
            .images
            .into_iter()
            .map(|i| EmbeddedImage {
                media_type: i.media_type,
                bytes: i.bytes,
                alt: i.alt,
            })
            .collect(),
        notes: d.notes,
        ..Default::default()
    }
}

/// The visible sheets of a workbook as CSV.
pub fn sheets(
    bytes: &[u8],
    format: Format,
    o: &Options,
) -> Result<Vec<sparkles_office::Sheet>, Refusal> {
    let f = format_of(format);
    let l = limits(o);
    run(
        bytes,
        o,
        Job::Sheets(format),
        |r| match r {
            Reply::Sheets(s) => Some(s),
            Reply::Text(_) => None,
        },
        move |b| sparkles_office::sheets(b, f, &l),
    )
}
