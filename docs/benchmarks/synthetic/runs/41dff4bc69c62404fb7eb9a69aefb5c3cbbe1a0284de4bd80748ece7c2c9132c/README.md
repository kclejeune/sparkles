# Retained-memory and optional-feature source comparison

This manual record compares `765fc956` with `6e696f22` at 100,000 generated people,
about 1.05 million triples. It preserves the original Namespace screen and its longer
rerun separately from a dedicated Intel control. The compared builds contain retained
classification accounting, derived-table cache controls, partial-memory handling and
optional server-feature repairs. This comparison does not attribute a cost to one change.

Both executables were built on atlas with empty extra Cargo arguments and matching
Clang 21.1.8, GCC 16.2.0, rustc 1.99.1 and LLD 23.1.1 compiler lines. The baseline ELF
SHA256 is `38290f03cb726a8bb8d0dc5b14afbaa8be5b1767bd918d5b9e9a8ccac05d532d`.
The later ELF SHA256 is
`9d22cb20d0c96343153b9e32b17fc690dd91cef0b8f7e41e937d01ccb1846997`.
The interpreter and native libraries retain their verified Nix paths; neither executable
was rewritten or rebuilt on forge.

The Namespace screen retains 9,600 first-pass and 7,680 longer-rerun samples. Its six
initial execution flags are all below 0.3 ms. Five clear in the prescribed longer rerun;
`distinct-obj` remains 0.013296 to 0.0156415 ms in execution and 0.556 to 0.639 ms in HTTP
wall time. The execution ratio is 1.176406. This remains a failed screen observation.

The dedicated Intel Core i5-13500 comparison uses the stock remote query runner, server
CPU 2, client CPU 4, one query thread, disabled result caching, idle spinners and independent
stores. Eight interleaved A,B,B,A rounds run in each source order, followed by a separate
baseline/baseline control of eleven tiny queries. Each fresh server gives each query
three untimed and 40 timed requests. All 90,880 timed tuples are complete, finite and
retained. Ratios below always divide the later source by the baseline; the reversed
source order has been inverted. The same-baseline control is separate.

| Query | Intel AB execution | Intel reversed BA execution | Intel AB wall | Intel reversed BA wall | Same-baseline execution | Same-baseline wall |
|---|---:|---:|---:|---:|---:|---:|
| `class-predicate-list` | 1.000821 | 1.001577 | 0.728786 | 0.998749 | — | — |
| `class-predicates` | 1.000676 | 1.001667 | 0.735073 | 0.999643 | — | — |
| `contains` | 0.998138 | 0.999015 | 1.001645 | 1.000549 | — | — |
| `count-all` | 1.119036 | 1.014420 | 1.001022 | 0.982439 | 1.001270 | 0.982421 |
| `distinct-join` | 1.008724 | 1.016213 | 1.008618 | 1.012141 | — | — |
| `distinct-obj` | 1.092276 | 1.029931 | 1.000000 | 1.004878 | 0.983591 | 0.999488 |
| `employee-docs` | 1.011108 | 0.988621 | 1.000000 | 1.000000 | 1.001286 | 1.000000 |
| `exists-join` | 1.006689 | 0.996192 | 0.999644 | 1.000357 | — | — |
| `expr-agg-arg` | 1.001975 | 1.002212 | 1.007867 | 1.002040 | — | — |
| `expr-bind-group` | 0.999586 | 1.002071 | 1.000000 | 0.999495 | — | — |
| `expr-order-key` | 1.001311 | 1.000674 | 1.000000 | 1.000000 | — | — |
| `group-avg` | 1.000964 | 1.003099 | 0.997455 | 1.005236 | — | — |
| `knows-reach` | 0.994337 | 0.998087 | 0.994502 | 0.999256 | — | — |
| `lang-filter` | 0.991379 | 0.991609 | 1.025436 | 0.968955 | — | — |
| `minus` | 0.994030 | 0.987885 | 1.000000 | 1.000000 | 0.997504 | 1.000507 |
| `not-exists` | 0.996407 | 0.996389 | 0.999493 | 1.000000 | — | — |
| `optional-chain` | 1.001835 | 1.003683 | 1.000071 | 1.000071 | — | — |
| `optional-count` | 1.006907 | 1.013227 | 1.000000 | 1.000000 | — | — |
| `order-by-full` | 1.001396 | 1.000468 | 1.002351 | 1.001047 | — | — |
| `path-plus` | 1.023690 | 0.979553 | 1.039406 | 0.976340 | 1.042534 | 0.999487 |
| `predicate-counts` | 1.073520 | 1.007714 | 1.001022 | 1.012563 | 0.991289 | 0.996430 |
| `predicate-subjects` | 1.081604 | 1.001177 | 1.000513 | 0.998464 | 1.009013 | 1.001027 |
| `range-topk` | 1.004724 | 1.002071 | 1.000000 | 0.999493 | — | — |
| `regex-iri` | 0.997986 | 0.997433 | 0.997979 | 1.004015 | — | — |
| `star-join` | 0.981743 | 0.977940 | 1.003562 | 1.002725 | — | — |
| `star-lookup` | 0.987944 | 1.000330 | 1.000504 | 1.000000 | 1.007281 | 1.000000 |
| `subquery-agg` | 1.005046 | 1.005073 | 1.000000 | 1.000000 | 1.003758 | 1.000000 |
| `two-hop-count` | 1.023266 | 1.020468 | 1.014289 | 1.012889 | — | — |
| `types-grouped` | 1.074929 | 1.089074 | 1.050360 | 1.001973 | 0.972173 | 1.000000 |
| `values-star` | 0.996496 | 0.985211 | 1.000000 | 1.000000 | 1.020852 | 1.000000 |

`distinct-obj` is slower in execution in all eight forward rounds and six of eight
reversed rounds. Forward round ratios span 1.0633 to 1.1136, while reversed ratios span
0.9405 to 1.1369. Its median execution increase is 0.3405 µs forward and 0.111 µs reversed.
`count-all` exceeds 10% only in the forward aggregate, from 4.129 to 4.6205 µs. The reversed
aggregate is 4.265 to 4.3265 µs and does not reproduce that threshold. No query reaches a
10% aggregate execution increase in both orders, and no aggregate HTTP-wall ratio
reaches 1.10 in either order. These facts do not establish that every positive is noise.

Fresh-process spreads are substantial even on Intel. `distinct-obj` execution spreads
are 8.30%/10.86% for baseline/later source forward and 18.42%/14.88% in reverse. Its wall
spreads approach 100%. The baseline/baseline controls reach 20.72% execution and 107.79%
wall process spread across their eleven queries. Process spreads and complete per-round
ratios for every metric are retained in [analysis.json](analysis.json); they are not
confidence intervals. The same-baseline leg does not replace either source ordering.

The sorted TSV reference fingerprints agree across variants for 30 queries in each
source order and 11 queries in the same-baseline control. These are reference-answer
checks, not captured native answers for every timed request. No independent mathematical
oracle or result-format equivalence is inferred.

The named Intel job exited 0 under 12 GiB memory, 2 GiB swap and 2670+30 seconds. Actual
manager peak memory was 5,464,104,960 bytes and swap peak 0. All thirteen recorded leaf
memory-event boundaries remain zero; available ancestor counters have zero deltas,
while the root event file is unavailable. Historical ancestor totals are not zeroed or
reported as new events. The exact completed service was stopped after collection,
its cgroup and recorded processes are absent and both locks are released. The collector
created a complete verified archive, but its own operating-system exit status is
unknown after its terminal holder failed to return a durable result. Benchmark success
and archive completeness do not reconstruct that collector status.

[record.json](record.json) binds the immutable raw archive and source identities.
[followup-selection.json](followup-selection.json) preserves every positive execution
and wall lead from both hosts and all eleven tiny controls. Their union covers all 30
queries. Item 759 retains these leads for attribution and repair of confirmed costs,
separately from the earlier source pairs in items 752 and 756. This record does not
convert the surviving Namespace flag into a passing screen or supply publication approval.
