//! Explicit, bounded local RDF reference replay. Supplied declarations are simulated.
#[path = "shared_entity_replay/mod.rs"]
mod replay;

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    match replay::run_cli(&arguments) {
        Ok(bytes) => {
            use std::io::Write;
            if std::io::stdout().lock().write_all(&bytes).is_err() {
                std::process::exit(1);
            }
        }
        Err(failure) => {
            eprintln!("{}", failure.code());
            std::process::exit(1);
        }
    }
}
