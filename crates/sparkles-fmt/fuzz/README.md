# Fuzzing the formatter

[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets for `sparkles-fmt`. This
directory is a Cargo workspace of its own (with its own `Cargo.lock`): `libfuzzer-sys`
and the nightly toolchain stay out of the repository's workspace, its lockfile, its
third-party license notices and the Nix build, and `mise run ci` never builds it.

| Target | Languages | Also checks |
|---|---|---|
| `fmt_sparql` | SPARQL queries and updates | |
| `fmt_turtle` | Turtle, TriG (header variant bit) | |
| `fmt_lines` | N-Triples, N-Quads (header variant bit) | streamed output (any bytes, any chunk size, spilled sorted runs) equals the in-memory output |
| `fmt_jsonld` | JSON-LD | |

Each input is a five-byte options header followed by the text; an all-zero header is the
default options (see `decode` in `src/invariants.rs` for the bits). Every target checks
the formatter's own promises:

- it never panics;
- it either refuses with a syntax error positioned inside the input, or prints output
  that means what the input means (by the reference parse of each: spargebra, oxttl,
  strict JSON), keeps every comment, and formats to itself under the same options;
- a cursor maps to a character boundary of the output.

Two refusals are deliberate and pass: nesting deeper than the formatter takes (256
levels of brackets, so that nothing overflows the stack), and SPARQL that only
spargebra's leniency accepts: tokens it reads as two where the grammar's longest match
makes one (`PREFIXex:` as `PREFIX ex:`, `CONSTRUCTWHERE`, `?o .2 ?s` as `?o . 2 ?s`;
see `lenient_reading`). Any other refusal (`unsupported-syntax`, `unsafe-format`,
`unstable-format`) is a finding.

## Running

Needs a nightly toolchain and cargo-fuzz, installed for the user (not by `mise`):

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz --locked
```

Then, from the repository root:

```sh
mise run fmt:fuzz                                # every target, 30 minutes each, 2 jobs
mise run fmt:fuzz --time 600 fmt_lines           # one target, 10 minutes
crates/sparkles-fmt/fuzz/run.sh --help
```

`run.sh` builds with debug assertions, runs at low priority in libFuzzer's fork mode
(a crash does not stop the run) and lists what it found under `artifacts/<target>/`.
The first run builds the seed corpora (`seed.sh`) in `corpus/<target>/` from the golden
inputs and, when the variables are set, the suites the tests use: `SPARKLES_W3C_DIR`
(SPARQL, the RDF suite next to it, Jena's RIOT JSON-LD files and the JSON results),
`SPARKLES_RDF_TESTS_DIR`, `SPARKLES_SHACL_TESTS` and `SPARKLES_JSONLD_TESTS`. The suites
are read where they are checked out; no third-party file is copied into the repository.
`corpus/`, `artifacts/` and `target/` are git-ignored.

To replay or shrink a finding (from this directory):

```sh
cargo +nightly fuzz run -a fmt_turtle artifacts/fmt_turtle/crash-…
SPARKLES_FUZZ_MATCH=unstable-format cargo +nightly fuzz tmin -a fmt_turtle artifacts/fmt_turtle/crash-…
```

With `SPARKLES_FUZZ_MATCH` set, a broken promise whose message does not contain it does
not count, so shrinking keeps the kind of finding it started from (without it, `tmin`
settles on whatever smaller input fails first).

A fixed finding becomes a test in `../tests/fuzz_regressions.rs`, which runs the same
checks (it includes `src/invariants.rs`) in the normal test suite.
