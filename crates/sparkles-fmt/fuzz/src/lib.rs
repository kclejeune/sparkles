//! The fuzz targets' shared part: the options header and the formatter's promises.

pub mod invariants;

use invariants::{Extra, check, check_stream, decode};
use sparkles_fmt::Language;
use std::sync::OnceLock;

/// Run one fuzz input through `lang` (the variant bit picks TriG over Turtle and N-Quads
/// over N-Triples), panicking with the broken promise.
pub fn run(data: &[u8], lang: Language) {
    let (opts, extra, bytes) = decode(data);
    let lang = variant(lang, extra);
    if lang.is_line_format()
        && let Err(e) = check_stream(bytes, lang, &opts, extra)
    {
        fail(format!("{lang:?} under {opts:?}, {extra:?}: {e}"));
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    if let Err(e) = check(text, lang, &opts) {
        fail(format!("{lang:?} under {opts:?}: {e}"));
    }
}

/// Panic with `message`, unless `SPARKLES_FUZZ_MATCH` is set and `message` does not
/// contain it: shrinking a finding (`cargo fuzz tmin`) then keeps its kind.
fn fail(message: String) {
    static MATCH: OnceLock<Option<String>> = OnceLock::new();
    match MATCH.get_or_init(|| std::env::var("SPARKLES_FUZZ_MATCH").ok()) {
        Some(m) if !message.contains(m.as_str()) => {}
        _ => panic!("{message}"),
    }
}

fn variant(lang: Language, extra: Extra) -> Language {
    match (lang, extra.variant) {
        (Language::Turtle, true) => Language::TriG,
        (Language::NTriples, true) => Language::NQuads,
        (lang, _) => lang,
    }
}
