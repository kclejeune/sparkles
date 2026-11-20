# DISTINCT on charged buffers, forge, 2026-11-20

These are the records behind the comparison of DISTINCT before and after its move onto
`Charged` buffers in the [memory results](../../RESULTS.md#charged-distinct-buffers) and the
[X19 outcome](../../../../specs/X19-charged-buffers/SPEC.md#outcome). They are manual
records, not bench-record files, so the [run catalog](../../RUNS.json) does not list them.
Their files keep the bytes the run wrote. The `SHA256SUMS` file lists the hash of each
original file, and each `.zst` file compresses the file of the same name that it lists.
The scripts and queries are in `scripts.tar.zst`.

Both builds are release builds made on `wally` with rustc 1.99.1 and mbx 1.30.0. A is
74f229b9, the base before the move, with sha256
`e20608f94666d7208d0273efd7e6452f553ad81f5111e9b6eea058865e56527e`. B is 62f72e53, the
last commit of the change that touches the server, with sha256
`a17438cdae3c88c86321195ec62fb51037a41d819db1fe548c875931a79d1f1c`. The data is
`scripts/gen-data.py 100000`, which loads as 1,052,801 quads, and its compressed file has
sha256 `b6ebcf554bcd529c4459935e75db570490507317a612e1e1b4c0e16cd3aa563d`. Each build
loaded its own store, and every server ran with one Rayon thread and
`--result-cache-mb 0`. `checksums.log` records that the binaries, the data and the scripts
matched these hashes before the run.

The run took place on `forge`, a 13th-generation Intel Core i5-13500 with 16 GiB of
memory, under the host's benchmark and build locks with no other heavy process.
`run6b.sh` ran the batch screen's harness, `nsc-bench-remote.sh` and `summary.py`, once in
each execution mode. It pinned the server to CPU 2 and the client to CPU 4, two separate
performance cores, with idle-priority spinners on both, and started a fresh server for
each process. Each mode ran 8 rounds of A, B, B, A with 3 warm-up and 20 timed requests
of each query per process. A query at a ratio of 1.10 or more would have been rerun with
16 rounds of 40 requests, and none was.

| Directory or file | Contents |
|---|---|
| `streaming`, `auto` | One pass per execution mode. `samples.tsv` holds the `execMs` and client wall time of each timed request, `answers.tsv` the hashes of the sorted TSV answers, `summary.md` the harness's report and validity check, and `rows.tsv` the columns that `rows.py` derives for the decision rule. |
| `status` | The progress log of `run6b.sh`, which ends with `DONE exit=0`. |
| `host.txt`, `*/env.txt` | The host, its cores, governor and load, and the harness's environment. |
| `freq.tsv` | `sampler.sh`'s readings every 5 s of the server and client cores' frequency and throttle counts, the host's free memory and page-fault and compaction counters, and the server's process and minor faults. |
| `affinity.txt` | The allowed and last CPU of every thread of each server process. |

The decision rule was written before the run. Each query and mode is noisy when the
same-binary spread of either build exceeds 0.10, and the run is invalid when more than
three of the twelve rows of the six queries of at least 0.3 ms are noisy or when either
pass fails the harness's own validity check. A difference is noise unless the ratio is
above 1.00 in every round and the median added time exceeds 0.5 µs. A real cost under
10% goes into a roadmap item with a recovery, and one of 10% or more blocks the change.
`distinct-obj` and `predicate-subjects` take 4 to 9 µs and are decided only by the X08
harness of the Intel rule.

Both passes are valid. The same-binary spread is 1.9% in streaming and 2.8% in automatic
execution, every server process ran within 1.7% of the median speed, no row of at least
0.3 ms is noisy, and every query returned the same answers from both builds. The server's
core ran at 3.9 to 4.0 GHz in 98% of the readings, its throttle count did not change
during the run, and every server thread was allowed only CPU 2.
