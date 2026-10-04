//! UniFFI's bindings generator, built with the library's own UniFFI version:
//! `cargo run --features bindgen --bin uniffi-bindgen -- generate --library <lib> --language kotlin`.

fn main() {
    uniffi::uniffi_bindgen_main()
}
