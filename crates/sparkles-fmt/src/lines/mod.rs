//! N-Triples 1.2 and N-Quads 1.2: one statement per line in the term spelling of
//! canonical N-Triples, each statement formatted and checked on its own (the oxttl quad
//! of the input statement equals the one of its formatted line), comments and blank
//! lines kept as written, source order kept unless `sort` (lines sorted by their bytes,
//! identical comment-free lines merged) or `canonicalize` (RDFC-1.0 labels, sorted, no
//! comments). [`format_stream`] reads line-aligned chunks and formats them on every core
//! (feature `parallel`); sorting past [`LinesConfig::sort_memory`] spills LZ4-framed
//! sorted runs and merges them.
//!
//! Not written yet: [`IMPLEMENTED`] is off, so neither entry point is reached.

use crate::{FormatError, Formatted, Language, LineStats, LinesConfig, Options};

/// Whether N-Triples and N-Quads format.
pub const IMPLEMENTED: bool = false;
/// Whether `sort` acts on the line formats.
pub const SORT_IMPLEMENTED: bool = false;
/// Whether `canonicalize` acts on the line formats.
pub const CANONICALIZE_IMPLEMENTED: bool = false;

/// Format a whole N-Triples or N-Quads document in memory (what [`crate::format`] and
/// the HTTP endpoint use).
pub fn format_str(text: &str, lang: Language, opts: &Options) -> Result<Formatted, FormatError> {
    let _ = (text, opts);
    Err(FormatError::unsupported_language(lang))
}

/// Format N-Triples or N-Quads from `r` to `w` in bounded memory (what
/// [`crate::format_lines`] and `sparkles fmt` use for files).
pub fn format_stream(
    r: impl std::io::BufRead,
    w: impl std::io::Write,
    lang: Language,
    opts: &Options,
    cfg: &LinesConfig,
) -> Result<LineStats, FormatError> {
    let _ = (r, w, opts, cfg);
    Err(FormatError::unsupported_language(lang))
}
