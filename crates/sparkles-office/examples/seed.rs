//! Write the seed corpus of the fuzz target: one document of every format, each after the
//! byte that names its format. Usage: `cargo run --example seed --features fixtures DIR`.

use sparkles_office::Format;
use sparkles_office::fixtures::*;

fn main() -> std::io::Result<()> {
    let dir = std::env::args().nth(1).expect("usage: seed DIR");
    std::fs::create_dir_all(&dir)?;
    let docs = [
        (Format::Docx, docx()),
        (Format::Pptx, pptx()),
        (Format::Xlsx, xlsx()),
        (Format::Odt, odt()),
        (Format::Odp, odp()),
        (Format::Ods, ods()),
    ];
    for (i, (f, bytes)) in docs.into_iter().enumerate() {
        let mut out = vec![i as u8];
        out.extend_from_slice(&bytes);
        std::fs::write(format!("{dir}/{}", f.name()), out)?;
    }
    Ok(())
}
