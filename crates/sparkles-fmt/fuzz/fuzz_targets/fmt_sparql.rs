#![no_main]

use libfuzzer_sys::fuzz_target;
use sparkles_fmt::Language;

fuzz_target!(|data: &[u8]| sparkles_fmt_fuzz::run(data, Language::Sparql));
