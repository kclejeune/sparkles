# Property path views on Namespace

This record holds the measurements of roadmap item 417, the fixed-point operator of
[F13 §4.3](../../../../specs/F13-incremental-views-and-provenance.md#fixed-point-operator).
It is a manual record. The files are the raw output of the runs and are not edited.

`incr-bench.log` is the output of `remote.sh` and then `remote-2.sh` on one ephemeral
Namespace instance, `linux/amd64:8x16`, an AMD EPYC with 8 vCPUs and 16 GiB of memory.
The instance ran nothing else, and each process ran pinned to CPUs 2–7 with 6 Rayon
threads. Three release builds of `crates/sparkles-core/tests/incr_bench.rs` ran there,
and `SHA256SUMS` identifies them. The baseline is commit `0dc27215` with the benchmark
file of commit `d7182de3`, so that it times the path views while they are `recompute`
nodes. The candidate is commit `d7182de3`, whose engine code is that of the lane's tip.
The third build is commit `35cbbd25`, the overdeletion experiment. All three were built
on atlas with `mbx test --release -p sparkles-core --test incr_bench --no-run` and Rust
1.99.1, and each binary ran under the instance's own glibc loader.

`remote.sh` runs the whole benchmark on the baseline and the candidate in three rounds of
alternating order, and then alternates `path_pass_against_recompute` between the
experiment and the candidate for three rounds. `remote-2.sh` adds two more rounds of the
whole benchmark, so that each of the two builds ran in five processes as the gate does.
Every process used 200,000 people, the default size, and every check of every process
passed, including each view's rows against its query evaluated again.

`compare.md` is the output of `scripts/bench-gate.py compare` over the gate records that
`scripts/bench-gate.py metrics` made from the five processes of each build. The records
lack the compilers and the build configuration, which these commands do not collect, so
the comparison ran with `--allow-different`.
