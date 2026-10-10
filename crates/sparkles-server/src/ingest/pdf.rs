//! PDF conversion with pdf-inspector (spec C18 §7.1.1).
//!
//! The rendition of a PDF is the Markdown that pdf-inspector writes with a
//! `<!-- Page N -->` marker before each page. A PDF is converted without OCR only when no
//! page needs it and no font's codes are broken. pdf-inspector reports the pages that
//! need OCR twice: in its classification of the whole document, and in its extraction of
//! each page. A document with a few text pages and one scanned page can classify as
//! text-based while that page still comes out empty, so the pages of both reports are
//! refused together. Every other option is fixed here, so the same PDF always gives the
//! same Markdown until the pinned version changes.
//!
//! Parsing runs on a thread of its own, at most `--pdf-workers` at a time, with a panic
//! caught as `conversion-failed`. A conversion that overruns the task's deadline cannot
//! be stopped inside the library: the task reports `conversion-timeout`, and the thread
//! keeps its worker slot until it ends.

use super::PdfRuntime;
use super::convert::{Converted, Options, PageReason, Refusal};
use crate::mcp::memory::ingest::normalize;
use pdf_inspector::{PdfError, PdfOptions, ProcessMode};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc;

/// The reason of a page that comes out empty without a reason of its own.
const SCANNED: &str = "scanned";

/// Convert a PDF under `o`'s deadline on a worker thread.
pub fn convert(bytes: &[u8], o: &Options) -> Result<Converted, Refusal> {
    let Some(slot) = PdfRuntime::acquire(&o.pdf, o.deadline) else {
        return Err(Refusal::new(
            "conversion-timeout",
            504,
            "every PDF worker stayed busy until the deadline",
        ));
    };
    let bytes = bytes.to_vec();
    let allow_partial = o.allow_partial;
    let rt = o.pdf.clone();
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("pdf-convert".into())
        .spawn(move || {
            let _slot = slot;
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                inspect(&bytes, allow_partial, &rt)
            }));
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
        Ok(Ok(r)) => r,
        Ok(Err(_)) => Err(Refusal::new(
            "conversion-failed",
            422,
            "the PDF could not be read: the converter stopped on it",
        )),
        Err(_) => Err(Refusal::new(
            "conversion-timeout",
            504,
            "the PDF's conversion did not end before the deadline",
        )),
    }
}

/// The fixed options of a conversion, limited to `pages` when given.
fn options(pages: Option<&[u32]>) -> PdfOptions {
    let mut o = PdfOptions::new().mode(ProcessMode::Full);
    o.markdown.include_page_numbers = true;
    if let Some(p) = pages {
        o = o.pages(p.iter().copied());
    }
    o
}

fn failed(e: PdfError) -> Refusal {
    match e {
        PdfError::Encrypted => Refusal::new(
            "conversion-failed",
            422,
            "the PDF is encrypted with a password",
        ),
        PdfError::NotAPdf(_) => Refusal::new("conversion-failed", 422, "the file is not a PDF"),
        e => Refusal::new(
            "conversion-failed",
            422,
            format!("the PDF could not be read: {e}"),
        ),
    }
}

/// The pages that need OCR, with their reasons, from both of pdf-inspector's reports.
fn needing(
    r: &pdf_inspector::PdfProcessResult,
    pages: &pdf_inspector::PagesExtractionResult,
) -> BTreeMap<u32, Vec<String>> {
    let mut out: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for p in &r.pages_needing_ocr {
        out.entry(*p).or_default();
    }
    for p in &r.ocr_reasons_by_page {
        out.entry(p.page)
            .or_default()
            .extend(p.reasons.iter().cloned());
    }
    for p in &pages.pages_needing_ocr {
        out.entry(*p).or_default();
    }
    for p in &pages.ocr_reasons_by_page {
        out.entry(p.page)
            .or_default()
            .extend(p.reasons.iter().cloned());
    }
    for p in &pages.pages {
        if p.needs_ocr {
            let e = out.entry(p.page + 1).or_default();
            if let Some(r) = &p.ocr_reason {
                e.push(r.clone());
            }
        }
    }
    for (page, reasons) in out.iter_mut() {
        if reasons.is_empty() {
            let empty = pages
                .pages
                .iter()
                .find(|p| p.page + 1 == *page)
                .is_none_or(|p| p.markdown.trim().is_empty());
            reasons.push(if empty { SCANNED } else { "unreliable-text" }.into());
        }
        reasons.sort();
        reasons.dedup();
    }
    out
}

fn inspect(bytes: &[u8], allow_partial: bool, rt: &Arc<PdfRuntime>) -> Result<Converted, Refusal> {
    let r = pdf_inspector::process_pdf_mem_with_options(bytes, options(None)).map_err(failed)?;
    let per_page = pdf_inspector::extract_pages_markdown_mem(bytes, None).map_err(failed)?;
    let need = needing(&r, &per_page);
    let fonts: Vec<String> = r
        .cmap_gaps
        .iter()
        .filter(|g| g.unmapped > 0)
        .map(|g| g.font.clone())
        .collect();
    let broken = r.has_encoding_issues;
    let title = r.title.clone().filter(|t| !t.trim().is_empty());
    let mut out = Converted {
        media_type: "application/pdf".into(),
        title,
        ..Default::default()
    };
    if need.is_empty() && !broken {
        out.text = normalize(r.markdown.as_deref().unwrap_or(""));
        return Ok(out);
    }
    if let Some(ocr) = rt.ocr() {
        let mut pages: Vec<u32> = need.keys().copied().collect();
        if broken && pages.is_empty() {
            // the broken fonts' pages are not known: read every page
            pages = (1..=r.page_count).collect();
        }
        return ocr_convert(bytes, r.page_count, &pages, &per_page, ocr, out);
    }
    let refusal = || {
        let mut f = Refusal::new(
            "needs-ocr",
            422,
            if need.is_empty() {
                "the PDF's fonts map some characters to nothing readable: OCR is needed, and this server has none".to_string()
            } else {
                format!(
                    "{} of the PDF's {} pages {} no usable text: OCR is needed, and this server has none",
                    need.len(),
                    r.page_count,
                    if need.len() == 1 { "has" } else { "have" }
                )
            },
        );
        f.pages = need
            .iter()
            .map(|(page, reasons)| PageReason {
                page: *page,
                reasons: reasons.clone(),
            })
            .collect();
        if broken {
            f.fonts = fonts.clone();
        }
        f
    };
    if !allow_partial {
        return Err(refusal());
    }
    let keep: Vec<u32> = (1..=r.page_count)
        .filter(|p| !need.contains_key(p))
        .collect();
    if keep.is_empty() {
        return Err(refusal());
    }
    let text = if need.is_empty() {
        r.markdown.clone().unwrap_or_default()
    } else {
        pdf_inspector::process_pdf_mem_with_options(bytes, options(Some(&keep)))
            .map_err(failed)?
            .markdown
            .unwrap_or_default()
    };
    out.text = normalize(&text);
    out.omitted_pages = need.keys().copied().collect();
    if broken {
        out.notes.push(format!(
            "some characters could not be read and are U+FFFD in the text{}",
            if fonts.is_empty() {
                String::new()
            } else {
                format!(" (fonts {})", fonts.join(", "))
            }
        ));
    }
    Ok(out)
}

#[cfg(not(feature = "pdf-ocr"))]
fn ocr_convert(
    _bytes: &[u8],
    _page_count: u32,
    _pages: &[u32],
    _per_page: &pdf_inspector::PagesExtractionResult,
    ocr: &super::OcrConfig,
    _out: Converted,
) -> Result<Converted, Refusal> {
    match *ocr {}
}

/// Read `pages` by OCR and the others from their text layer, each page after its marker.
#[cfg(feature = "pdf-ocr")]
fn ocr_convert(
    bytes: &[u8],
    page_count: u32,
    pages: &[u32],
    per_page: &pdf_inspector::PagesExtractionResult,
    ocr: &super::OcrConfig,
    mut out: Converted,
) -> Result<Converted, Refusal> {
    use pdf_inspector::vision::{
        ModelDownloadPolicy, ModelStore, OarOcrEngine, OcrFusionOptions, OcrMode, OcrOptions,
        PP_OCR_V6_SMALL, PdfiumRenderer, RenderOptions, fuse_ocr_pages, run_ocr_pages,
    };
    let fail = |m: String| Refusal::new("ocr-failed", 500, m);
    let options = OcrOptions::new()
        .mode(OcrMode::Force)
        .model_directory(&ocr.models)
        .model_downloads(ModelDownloadPolicy::Offline);
    let models = ModelStore::from_options(&options)
        .and_then(|s| s.resolve(&PP_OCR_V6_SMALL))
        .map_err(|e| fail(format!("the OCR models cannot be used: {e}")))?;
    let engine = OarOcrEngine::from_models(&models)
        .map_err(|e| fail(format!("ONNX Runtime cannot run the OCR models: {e}")))?;
    let renderer = match &ocr.pdfium {
        Some(p) => PdfiumRenderer::load_from_path(p),
        None => PdfiumRenderer::load(),
    }
    .map_err(|e| fail(format!("PDFium cannot be loaded: {e}")))?;
    let run = run_ocr_pages(
        &renderer,
        &engine,
        bytes,
        pages,
        None,
        &RenderOptions::new(),
        &options,
    )
    .map_err(|e| fail(format!("OCR failed: {e}")))?;
    let fused = fuse_ocr_pages(&per_page.pages, &run, page_count, &OcrFusionOptions::new())
        .map_err(|e| fail(format!("OCR failed: {e}")))?;
    let mut md = String::new();
    for n in 1..=page_count {
        let text = if pages.contains(&n) {
            fused
                .pages
                .iter()
                .find(|p| p.page_number == n)
                .map(|p| p.markdown.clone())
                .unwrap_or_default()
        } else {
            per_page
                .pages
                .iter()
                .find(|p| p.page + 1 == n)
                .map(|p| p.markdown.clone())
                .unwrap_or_default()
        };
        md.push_str(&format!("<!-- Page {n} -->\n\n{}\n\n", text.trim_end()));
    }
    out.text = normalize(md.trim_end()) + "\n";
    out.ocr_pages = pages.to_vec();
    Ok(out)
}
