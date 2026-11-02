# Write rows and changed-store queries, forge run

These runs refreshed the write comparisons with Oxigraph after the write-through fences and
annotation records of items 501 and 502, and measured the changed-store queries before and
after the parallel reads of changed index blocks. They ran on `forge` on 2026-11-02 between
11:50 and 12:30 UTC.

The host is `forge`, an Intel Core i5-13500 with 20 logical CPUs and the SK hynix
HFS512GEJ9X102N drive under LUKS, LVM and ext4. Each stage waited for the host's benchmark
and heavy-job locks and held both until it finished, so no other benchmark or build ran
during a stage. The stages of other work ran between some of these stages.

The harness is `scripts/bench.sh` of the commit of each binary, with `SKIP_PROBE=1` and
`PORT_BASE=4600`. The `load` stage is the default mode limited to `star-join` with
`QUERIES=star-join`, so it loads the stores and times `star-join`, the single-triple update
latency and the throughput of 16 clients. The `updates` stage is the `updates` mode with
`QUERIES="two-hop-count contains regex-iri star-join"`, which applies the 5,000 commits of
`churn-5000.ru` and then times those queries on the changed stores. The `mixed` stage is the
`mixed` mode with its defaults. Oxigraph is 0.5.11 from the flake. At 10.5M only Sparkles
ran, and the Oxigraph cells at that size keep their earlier measurements.

The binaries are release builds of `mbx build --release -p sparkles-server` in the Nix
development shell on `atlas`, copied to `forge`.

| Binary | Source | SHA-256 |
|---|---|---|
| `b62d3722` | main before the parallel reads of changed blocks | `9b64590ed735903c9872875d5b2366dd25ff81c2df6023333b8850ea59cb7257` |
| `98f1728d` | the parallel reads of changed blocks | `744102123ab0b47e0b898fb174f9c7f6b2067983d0ad4045cacdfcff46b1622d` |

The `98f1728d` binary was built from the tree of that commit before its documentation was
finished. Commit `62e793ce` later bounds the delta that a parallel read merges at 65,536
keys, which is above the 5,000 commits of these runs, so it reads these stores the same way.

Each directory holds the results directory of one stage: `<binary>-<size>-<stage>`. The
`mixed` directories also hold the files of the `load` stage before them, because both modes
write to `results`. `b62d3722-1.05m-load-and-mixed` holds both stages of that binary at
1.05M. `logs` holds the harness output of each stage.

## Correctness

Every Sparkles and Oxigraph answer of the changed stores has the same value fingerprint in
`answers.json`, at 1.05M across both engines and both binaries, and at 10.5M across both
binaries. No commit of the churn or mixed writers failed, and no read failed.

## Results

Times are medians of the timed requests in ms, through `curl` as the harness times them.

| Stage | Measurement | b62d3722 | 98f1728d | Oxigraph |
|---|---|---:|---:|---:|
| 1.05M updates | `two-hop-count` | 8.16 | 6.21 | 5,993.6 and 6,007.4 |
| 1.05M updates | `contains` | 5.07 | 4.31 | 149.1 and 155.1 |
| 1.05M updates | `regex-iri` | 5.19 | 4.45 | 298.0 and 375.3 |
| 1.05M updates | `star-join` | 7.97 | 8.11 | 2,874.1 and 2,878.0 |
| 10.5M updates | `two-hop-count` | 35.83 | 21.63 | — |
| 10.5M updates | `contains` | 14.44 | 8.06 | — |
| 10.5M updates | `regex-iri` | 17.12 | 9.19 | — |
| 10.5M updates | `star-join` | 42.07 | 42.92 | — |
| 1.05M churn | commits/s | 777.5 | 934.9 | 809.4 and 743.7 |
| 1.05M churn | p50 / p99 | 1.290 / 1.788 | 1.154 / 1.762 | 1.277 / 1.809 and 1.348 / 2.092 |
| 10.5M churn | commits/s | 681.4 | 684.2 | — |
| 10.5M churn | p50 / p99 | 1.325 / 3.299 | 1.364 / 3.191 | — |
| 1.05M load | update latency | 5.14 | 3.66 | 3.65 and 3.52 |
| 1.05M updates | update latency | 3.64 | 3.88 | 3.47 and 3.52 |
| 1.05M mixed | writes/s | 245.7 | 252.0 | 1,264.1 and 1,276.9 |
| 1.05M mixed | reads/s | 2,101.3 | 2,197.9 | 11.9 and 15.7 |
| 10.5M mixed | writes/s | — | 210.5 | — |
| 10.5M mixed | reads/s | — | 197.1 | — |

The two Oxigraph values of a row come from the processes that ran beside each binary at
1.05M. The update latency of `b62d3722` on the loaded store was 5.0 to 5.3 ms in every
sample, and `98f1728d` took 3.6 to 3.7 ms one minute later on the same kind of store.
Sparkles synchronizes each commit and Oxigraph does not, and a difference of this size is
consistent with the drive's slow state that the
[fence probes](../2026-11-01-fence-probe-forge/README.md) describe. These runs did not
sample the drive, so they do not establish its state.
