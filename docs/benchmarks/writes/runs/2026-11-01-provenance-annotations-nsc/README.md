# Provenance annotation costs on Namespace

This record holds the measurements of roadmap item 415, the provenance annotations of
[F13 §5](../../../../specs/F13-incremental-views-and-provenance.md#provenance-annotations).
It is a manual record. The files are the raw output of the runs and are not edited.

## Incremental benchmark

`incr-bench.log` is the output of `remote.sh` on one ephemeral Namespace instance,
`linux/amd64:8x16`, an AMD EPYC with 8 vCPUs and 16 GiB of memory. The instance ran
nothing else, and each test ran pinned to CPUs 2–7. Two release builds of
`crates/sparkles-core/tests/incr_bench.rs` ran there. The baseline is commit `206d730c`,
and the candidate is commit `b994758f`. Both were built on atlas with `mbx test --release
-p sparkles-core --test incr_bench --no-run` and the same toolchain, Rust 1.99.1, and
`SHA256SUMS` identifies them. Each binary ran under the instance's own glibc loader.

The run alternates `pass_against_recompute` between the two builds for three rounds and
then runs `annotation_cost` on the candidate three times. Both tests use 200,000 people,
the default size. `pass_against_recompute` keeps each view as a table view and times 200
rounds after commits of one or two quads. `annotation_cost` attaches each view's query
as an internal circuit that keeps its rows, once without provenance and once with each
of `count`, `lineage` and `why` with 8 witnesses, and times 200 rounds after the same mix
of commits. `annotation_cost` asserts that each circuit reached the store's latest
commit, and `pass_against_recompute` checks no result.

## Query screen

`query-screen.md` is the summary of `mise run bench:nsc` with 8 rounds of A, B, B, A on
another instance of the same shape. A is the server of commit `206d730c`, and B is the
server of commit `5368bd97`, both release builds made on atlas. The commits after
`5368bd97` change only the incremental circuits, which the screen's queries do not run.
Both builds returned the same sorted answers to all 30 queries. An earlier 4-round run
of the same pair gave a geometric mean of 1.072, but in its third round B took about
twice as long as A on most queries under 0.1 ms while the other rounds were even. The
8-round rerun replaces it.
