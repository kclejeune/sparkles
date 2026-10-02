#![no_main]

use libfuzzer_sys::fuzz_target;
use sparkles_fmt::Language;

// N-Triples, or N-Quads when the header's variant bit is set; in memory and streamed
fuzz_target!(|data: &[u8]| sparkles_fmt_fuzz::run(data, Language::NTriples));
