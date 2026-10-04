//! The formatter and linter of SPARQL, Turtle, TriG, N-Triples, N-Quads and JSON-LD
//! (feature `fmt`), from the `sparkles-fmt` crate.

pub use sparkles_fmt::lint::{LintError, LintOptions, Linted};
pub use sparkles_fmt::{FormatError, Formatted, Language, Options, format};

/// Lint `text` as `lang` (SPARQL, Turtle or TriG).
pub fn lint(text: &str, lang: Language, opts: &LintOptions) -> Result<Linted, LintError> {
    sparkles_fmt::lint::lint(text, lang, opts)
}
