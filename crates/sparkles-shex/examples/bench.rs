//! Validation benchmark on a `scripts/gen-data.py` dataset, with `bench.shex`.
//!
//! ```sh
//! python3 scripts/gen-data.py 10000 > data.nt
//! cargo run --release -p sparkles-shex --example bench -- data.nt [schema.shex|-] [iterations]
//! ```

fn main() {
    eprintln!("the ShEx benchmark is not implemented yet");
    std::process::exit(2);
}
