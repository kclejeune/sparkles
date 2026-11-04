# Warm regex instruction counts on the classification batch

This is a manually maintained diagnostic for item 733. It compares the original
server binaries before and after the classification batch, and the cached binary
partway through it. It does not change the engine or replace a generated peer
comparison. The compressed JSON lines preserve every accepted timed sample and
its server counters. The record identifies the inputs, binaries and analysis.

| Variant | Source revision | Build host | Role |
|---|---|---|---|
| A | `ae8a6cd58299039e575e7802fc73347c7167b95c` | atlas | The base before the batch. |
| M | `4ab93ecbc605d50a0edde8872d2abb37f435fce2` | wally | The batch after the EL changes and the UI merge. |
| B | `5b0acdf99c32f47510e5b5b9075fae841ff91389` | atlas | The end of the batch. |
| C | The same executable and store as A. | atlas | The same-binary control. |

The hashes of A and B match the original pinned check. All three executables
record GCC 16.2.0, Clang 21.1.8, rustc 1.99.1 and LLD 23.1.1 in their compiler
comments. No executable was built on forge. The retained environment records the
Intel i5-13500, kernel, inputs and complete binary hashes.

The input is the cached output of `scripts/gen-data.py 100000`, with 1,052,801
quads. Each executable loaded its own persistent store. One Rayon thread runs
with the server pinned to CPU 2 and the Python HTTP client to CPU 4, with
SCHED_IDLE spinners on those CPUs and the result cache disabled. The server uses
the default execution mode and returns its unrounded `execMs` in the native JSON
format. The client opens a new HTTP connection for each request. This differs
from the original curl client, so comparisons here use only these runs.

Each fresh server receives three warm-up requests per query, then 100 measured
requests. Eight rounds run A,M,B,B,M,A and eight run B,M,A,A,M,B, which puts both
ends of each comparison in both positions. The control runs eight rounds of
A,C,C,A. Every job holds forge's benchmark lock in a named systemd user unit
with a 12 GiB memory cap and 2 GiB swap cap. No other heavy work runs on the host.

The hardware counters cover user instructions, cycles and generic cache misses
of every existing server thread. Every thread's allowed CPU was checked to be 2,
which is served by the core PMU. A new thread during an interval causes that
whole interval to be rejected and repeated with the new thread included. An
exited thread's open descriptor retains its final count. The rejected interval
metadata is retained separately. None of the accepted intervals multiplexes its
counters.

The timing estimate is the geometric mean of paired round ratios, using the
geometric mean of the two process medians within each round. The 95% intervals
bootstrap whole rounds 10,000 times with seed 733. Counter values divide the
whole request batch by its request count and use the same pairing. These count
the HTTP wrapper and other server work, so they do not isolate an operator's
instructions, especially for the microsecond queries.

Every TSV answer and every timed native answer agrees across variants. Query
plans agree after removing `timeMs` and consistently renaming the parser's
random internal aggregate variables. The query text, operators, estimates and
observed rows stay in that comparison. `regex-iri` tests 102,000 vocabulary keys
and matches 1,000 in every variant.

The initial stores were loaded without a preceding filesystem trim. Load
latencies are not compared, and every reported query is warm. The later
bisection check records its preparation separately.

| Query | A server ms | B/A server time | 95% interval | B/A instructions | B/A cycles |
|---|---:|---:|---|---:|---:|
| `regex-iri` | 3.46750 | 1.06197 | 1.06066 to 1.06326 | 0.999991 | 1.05918 |
| `exists-join` | 0.91849 | 1.02484 | 1.02340 to 1.02706 | 0.999942 | 1.01949 |
| `star-join` | 1.30523 | 0.92528 | 0.92251 to 0.92822 | 0.999866 | 0.94910 |
| `lang-filter` | 1.64401 | 1.00363 | 1.00135 to 1.00613 | 0.999975 | 1.00242 |
| `types-grouped` | 0.00574 | 0.98097 | 0.97062 to 0.99089 | 0.997953 | 0.97202 |
| `count-all` | 0.00337 | 0.97124 | 0.95351 to 0.98949 | 0.997237 | 0.96842 |

The `regex-iri` control reads 1.00061, with an interval of 0.99934 to 1.00217.
The base runs 75.20247 million instructions per request and the candidate
75.20178 million. Cycles rise from 14.25826 million to 15.10200 million. Generic
cache misses read 0.859 of the base, with an interval of 0.720 to 1.019, so the
run does not establish an increase in those misses. This event does not measure
instruction-cache or branch-predictor behavior separately.

The middle build's `regex-iri` time reads 0.99881 of A, with an interval of
0.99803 to 0.99949, and its instructions read 1.000004. This puts the timing
change after the UI merge. The only production changes in the rest of the
interval are the classifier changes and ontology-limit initialization of
`e951e2c3`. The remaining changes through B affect tests, workflows and docs.

The retained disassembly compares four regex and key-filter functions in A, M
and B. Their instructions and operands agree after RIP-relative relocation
displacements and relocation comments are removed. Their linked addresses move.
`RegexState::search`, for example, starts at `0x653d4d0` in A, `0x653d7d0` in M
and `0x65db280` in B, with the same 936-byte body.

The repeated time and cycle increase, unchanged request instruction count,
unchanged hot instructions, identical plans and answers, and the absence of a
query-path source change support a layout explanation without added query work. This is an
inference from the combined evidence. The measurements do not identify a
particular instruction-cache conflict or branch alias. The previously reported
small `types-grouped` loss does not reproduce under this diagnostic, whose
wrapper counters cannot resolve such a small engine-only cost.

The [rebuilt midpoint checks](bisection/README.md) keep the same instructions
but do not repeat the full six-percent contrast, even after restoring the
six-query mix. The base itself takes longer in those checks. They leave a
stable latency attribution to the classifier merge and the responsible cache
or process state unconfirmed. The original measured loss remains valid for its
recorded runs, and its recovery needs reproduction against the then-current
engine. This investigation makes no engine repair.

To recompute a summary, decompress the retained JSON lines and run
`analyze.py.txt` with the forward and reverse paths, followed by `--out` and a
new output path. Run it with the control path alone for the noise check.
`measure.py.txt` is the exact counter driver. `static.py.txt` reproduces the
selected disassembly comparison from the identified binaries.
