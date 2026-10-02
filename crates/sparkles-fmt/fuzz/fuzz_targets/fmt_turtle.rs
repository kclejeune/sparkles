#![no_main]

use libfuzzer_sys::fuzz_target;
use sparkles_fmt::Language;

// Turtle, or TriG when the header's variant bit is set
fuzz_target!(|data: &[u8]| sparkles_fmt_fuzz::run(data, Language::Turtle));
