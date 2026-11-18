# Per-request instruction counts on stanley

These runs set the noise floor of `scripts/request-insn.py` and test it against two
known regressions. Each run served 38 queries under streaming and automatic execution
from one store of 1.05M triples, `scripts/gen-data.py 100000`, on `stanley`, an Intel Core
Ultra X7 358H with Linux 7.3.3 and `kernel.perf_event_paranoid` at 2. The server ran on
performance core 2 or 0 and the client on core 3 or 1, with one Rayon thread, idle
spinners, eight rounds of A,B,B,A, five warm-up requests and up to 50 counted requests per
query and process. The host carried other work at a load of 13 to 18. The queries are
those of `scripts/bench.sh` with `root-scan`, `root-limit` and the five string queries of
item 678, in `queries/`.

| Run | A | B | Purpose |
|---|---|---|---|
| `aa-same` | 9acd6d74 built on stanley, sha256 4dfe940f | the same binary | Noise floor of one binary |
| `aa-build` | 9acd6d74 built on wally, sha256 f0090fa0 | 9acd6d74 built on stanley | Noise floor of one source built on two hosts |
| `bt-1039` | 9acd6d74 built on stanley | 60eeac80, sha256 afdfec33 | Item 1039's per-block charge |
| `bt-678` | 7e272f4d, sha256 f0090fa0 | a97afd76, sha256 872dc8f1 | Item 678's string floors |
| `r1063` | 9acd6d74 built on stanley | 9e3d7954, sha256 603a9bf3 | Row 1063's remaining cost |

Every binary was built with clang 21.1.8, GCC 16.2.0, LLD 23.1.1 and rustc 1.99.1. The
build of 7e272f4d is byte for byte the build of 9acd6d74 on wally. Each directory holds
`samples.tsv`, with one line per query window and process, `manifest.json` and
`summary.md`. `aa-same` and `bt-1039` were recorded before the script kept the counters'
run fraction, and their windows were marked as not fully counted whenever the running
and enabled times differed by rounding, so their summaries are made with
`--ignore-coverage`. In the later runs the smallest run fraction of a query window is
0.999578 in `aa-build`, 0.999765 in `bt-678` and 0.999813 in `r1063`, above the 0.999
that the script requires. Every pair returned the same answers to every query.

`analyze.py.txt` computes the per-query interval deviations and the per-round ratios of
execution and wall time that the results page quotes. `profiles/` holds the call-graph
profiles of row 1063, made by `prof.sh.txt` with `perf record --call-graph lbr
--user-callchains` over 100,000 or 300,000 requests of `root-limit`, `cyc4` with the timed call `Operator::next_shared` as its own region, and classified by
`classify.py.txt` into the samples under `QueryCursor::next_batch_at_most`, the timed
part of a shared-batch read, and the rest, and the `strace -c -f` counts of 50 and 1,050
requests of each build.

| Profile files | Event | Period | Requests | Regions |
|---|---|--:|--:|---|
| `A-rl-ins2.txt`, `B-rl-ins2.txt` | `cpu_core/instructions/u` | 1,000,003 | 100,000 | `next_batch_at_most` |
| `A-rl-cyc.txt`, `B-rl-cyc.txt` | `cpu_core/cycles/u` | 300,007 | 100,000 | `next_batch_at_most` |
| `A-rl-cyc3.txt`, `B-rl-cyc3.txt` | `cpu_core/cycles/u` | 300,007 | 300,000 | `next_batch_at_most` |
| `A-rl-cyc4.txt`, `B-rl-cyc4.txt` | `cpu_core/cycles/u` | 300,007 | 300,000 | `Operator::next_shared`, then `next_batch_at_most` |

Each profile followed 200 untimed requests in one server process on core 0, with the
client on core 1. A sample stands for one period of its event, so a count of samples
times the period over the requests gives the event per request.
