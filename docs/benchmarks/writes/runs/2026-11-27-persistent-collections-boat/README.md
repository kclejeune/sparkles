# Persistent collections on a Boat worker

This record holds the collection microbenchmarks of item 1152, which compare
`store::pmap::PMap` and `store::append_vec::AppendVec` with `imbl` 7.0.2, as the
[outcome of item 1152](../../../../specs/X16-storage-consolidation/SPEC.md#item-1152-outcome)
describes. It is a manual record. The TSV and log files are the raw output of the runs and
are not edited, and `SHA256SUMS` identifies them with the harness. The harness's manifest
and source carry a `.txt` suffix so that no tool takes them for a crate of the repository.

The runs used the Boat worker `bx_4xarzecx`, a sandbox with 8 vCPUs of an AMD Ryzen 9 9950X
and 16 GiB of memory. Each build used Rust 1.99.1 and mbx 1.29.1 with one compile job, and
the release profile in `harness/Cargo.toml.txt` with one codegen unit and thin LTO. Each run
pinned the binary to CPU 3 with `taskset`. The harness in `harness/main.rs.txt` includes the
crate's `pmap.rs` and `append_vec.rs` through `#[path]` attributes from the directory it ran
in, with copies of the two `keyset` helpers that `PMap` uses. The sources were 6176a2ab with
the change of item 1152 applied. In each of eight rounds the two arms run in one process,
`imbl` first in one round and `PMap` first in the next. Each line gives a case, its size,
each arm's median and range over the rounds in nanoseconds and the ratio of the medians,
`PMap` or `AppendVec` over `imbl`.

| File | Binary SHA-256 | What ran |
|---|---|---|
| `bench-v4.tsv` | `1564e07f…` in `s4-bench.log` | The change's sources at the start of its final validation, on a quiet worker with a load average of 0.4 to 0.6. This is the run of the X16 table. |
| `bench-v5a.tsv`, `bench-v5a2.tsv` | `35382831…` in `step6.log` | One binary of the final sources, run twice after a long test run, with load averages of 1.3 to 2.9. |
| `bench-v5b.tsv` | `ea3f596e…` in `step6.log` | The final sources with 16-entry nodes for maps of plain keys and values. The `sed` command in `step6.log` makes the edit and the `cp` after the run restores the file. |

The harness times collection operations alone. A commit case clones a map and changes one
entry, as a single-quad commit and a control frame do, and a tail case clones a vector and
appends rows. No case runs a whole-store commit, a WAL write or a spatial query. The
harness checks no answers. The correctness evidence is the unit tests of the same sources,
which compare `PMap` with `BTreeMap` under random writes and ranges and check snapshot
isolation and path copies. The same sources passed `mbx test -p sparkles-core` with 4,714
tests and, with `--features geo`, 183 spatial, store and control unit tests and 94 spatial
integration tests.

Boat timings are approximate and decide nothing. Item 1246 of the roadmap times the slower
cases on an Intel host.
