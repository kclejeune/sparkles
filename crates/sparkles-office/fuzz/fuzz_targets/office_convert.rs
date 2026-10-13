#![no_main]

use libfuzzer_sys::fuzz_target;
use sparkles_office::{Format, Limits, sheets, sniff, to_markdown};

// The first byte picks the format, the rest is the file. Any answer is fine, a panic, a
// hang or a large allocation is not: the limits are small so that the fuzzer finds a
// missing bound quickly.
fuzz_target!(|data: &[u8]| {
    let Some((first, file)) = data.split_first() else {
        return;
    };
    let format = Format::ALL[*first as usize % Format::ALL.len()];
    let limits = Limits {
        max_entries: 200,
        max_part: 1 << 20,
        max_total: 4 << 20,
        max_nodes: 50_000,
        max_text: 1 << 20,
        max_csv: 1 << 20,
        max_cells: 50_000,
        max_images: 4,
        max_image_bytes: 1 << 20,
        ..Limits::default()
    };
    let _ = sniff(file);
    if format.is_spreadsheet() {
        let _ = sheets(file, format, &limits);
    } else {
        let _ = to_markdown(file, format, &limits);
    }
});
