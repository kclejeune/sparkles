# Persistent collections on a Boat worker

This record holds the collection microbenchmarks of item 1152, which compare
`store::pmap::PMap` and `store::append_vec::AppendVec` with `imbl` 7.0.2, as the
[outcome of item 1152](../../../../specs/X16-storage-consolidation/SPEC.md#item-1152-outcome)
describes. It is a manual record. The TSV and log files are the raw output of the runs and
are not edited, and `SHA256SUMS` identifies them with the harness. The harness's manifest
and source carry a `.txt` suffix so that no tool takes them for a crate of the repository.

The runs used the Boat worker `bx_4xarzecx`, a sandbox with 8 vCPUs of an AMD Ryzen 9 9950X
and 16 GiB of memory. Each build used Rust 1.99.1 and mbx 1.29.1 and the release profile in
`harness/Cargo.toml.txt` with one codegen unit and thin LTO. The traces in `s4-bench.log`
and `step6.log` show the builds of the earlier runs with one compile job and each of those
runs pinned to CPU 3 with `taskset`. The log of the last run, `bench-r1.out`, holds no trace
of its commands. The harness in `harness/main.rs.txt` includes the
crate's `pmap.rs` and `append_vec.rs` through `#[path]` attributes from the directory it ran
in, with copies of the two `keyset` helpers that `PMap` uses. In each of eight rounds the
two arms run in one process, `imbl` first in one round and `PMap` first in the next. Each
line gives a case, its size, each arm's median and range over the rounds in nanoseconds and
the ratio of the medians, `PMap` or `AppendVec` over `imbl`.

The worker's sources were `git archive 6176a2ab` with the working tree of item 1152 copied
over it. The runs before the commit of item 1152 did not hash those sources, so their
identity rests on the order of the logs. The run `bench-v4.tsv` used a working tree that the
formatting check of the same step found unformatted in `pmap.rs`, `control.rs` and
`store.rs`. That check's log is not kept here, since its output has trailing whitespace.
The formatting was applied before the next runs, and the record does not show whether
anything else changed between them. The runs `bench-v5a.tsv`, `bench-v5a2.tsv`
and `bench-v5b.tsv` followed the validation of the formatted tree, and three minutes later
that tree was committed as 0d1df80c, whose Rust is the same as that of d2c524f6. At
d2c524f6 `pmap.rs` is blob `ad8958f0` and `append_vec.rs` is blob `646578c0`.

The removal of d2c524f6 copies a neighbouring node only when a merge follows, after the leaf
has changed, and its tail append clones a shared tail at its own length before growing it to
a chunk. Commit 6e6405bf changes both paths, so these four runs measure the collections
before that repair. The run `bench-r1.tsv` measures the repaired code. Its checkout was tree
`5b09ff6ad2b9fab2a08a04e2e616d02537adfc0e`, whose `crates` directory is the same as that of
6e6405bf, with `pmap.rs` at blob `163a082e` and `append_vec.rs` at blob `9ea23465`.

| File | Binary SHA-256 | What ran |
|---|---|---|
| `bench-v4.tsv` | `1564e07f…` in `s4-bench.log` | The working tree before its formatting, on a quiet worker with a load average of 0.4 to 0.6. This is the run of the X16 table. |
| `bench-v5a.tsv`, `bench-v5a2.tsv` | `35382831…` in `step6.log` | One binary of the sources of d2c524f6, run twice after a long test run, with load averages of 1.3 to 2.9. |
| `bench-v5b.tsv` | `ea3f596e…` in `step6.log` | The sources of d2c524f6 with 16-entry nodes for maps of plain keys and values. The `sed` command in `step6.log` makes the edit and the `cp` after the run restores the file. Commit 66b57d65 makes the same edit on 6e6405bf, and commit 367348c7 reverts it. |
| `bench-r1.tsv` | `05f39530…` in `bench-r1.out` | The repaired code of 6e6405bf, with load averages of 1.0 to 3.7. |

The harness times collection operations alone. A commit case clones a map and changes one
entry, as a single-quad commit and a control frame do, and a tail case clones a vector and
appends rows. No case runs a whole-store commit, a WAL write or a spatial query. The
harness checks no answers. The correctness evidence is the unit tests of the same sources,
which compare `PMap` with `BTreeMap` under random writes and ranges and check snapshot
isolation and path copies. The formatted tree of the v5 runs passed `mbx test -p
sparkles-core` with 4,714 tests and, with `--features geo`, 183 spatial, store and control
unit tests, and commit 0d1df80c passed 94 spatial integration tests. For the repaired code, `check.out` records 4,671 core nextest
tests, 185 spatial, store and control unit tests and 94 spatial integration tests passing on
commit 481302bb, whose Rust differs from 6e6405bf only in the formatting of one test
assertion, and the new test of a panicking value clone failing on the mutators of
d2c524f6. `check2.out` records formatting, Clippy with and without `geo` and the 15 tests of
`PMap` and `AppendVec` passing on tree `5b09ff6a`.

Boat timings are approximate and decide nothing. Item 1246 of the roadmap times the slower
cases on an Intel host.
