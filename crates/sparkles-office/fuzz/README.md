# Fuzzing the Office converter

A [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) target for `sparkles-office`. This
directory is a Cargo workspace of its own, with its own `Cargo.lock`, so that
`libfuzzer-sys` and the nightly toolchain stay out of the repository's workspace, its
lockfile, its third-party license notices and the Nix build. `mise run ci` never builds it.

The target `office_convert` takes a byte that picks one of the six formats and then the
file. It converts the file to Markdown, or to CSV for a spreadsheet, under small limits,
and sniffs its format. Any answer is fine. A panic, a hang or an out-of-memory input is a
finding. The converter also runs on a fixed set of mutations of the test fixtures in the
crate's normal tests (`damaged_files_never_panic`).

## Running

The target needs a nightly toolchain and cargo-fuzz, installed for the user:

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz --locked
```

Then, from the repository root:

```sh
mise run office:fuzz                    # 30 minutes, 2 jobs
mise run office:fuzz --time 600
crates/sparkles-office/fuzz/run.sh --help
```

The first run builds the seed corpus in `corpus/office_convert/` from the fixtures of
`src/fixtures.rs`. `corpus/`, `artifacts/` and `target/` are git-ignored. To replay a
finding, from this directory:

```sh
cargo +nightly fuzz run -a office_convert artifacts/office_convert/crash-...
```
