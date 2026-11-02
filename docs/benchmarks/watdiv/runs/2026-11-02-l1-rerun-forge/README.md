# WatDiv L1, L3 and S7 rerun, forge run

This run measured again the WatDiv templates where Fluree was close to or ahead of
Sparkles in the published comparison: L1, whose first instance Fluree led by 11%, S7, a
tie, and L3 for reference. It ran on `forge` on 2026-11-02 between 11:51 and 12:27 UTC.

The host is `forge`, an Intel Core i5-13500 with 20 logical CPUs. The harness is
`scripts/bench-watdiv.sh` of commit `b62d3722`, with `ENGINES="sparkles fluree"`,
`PORT_BASE=4700`, the default two warm-up and ten timed requests per instance, and the
dataset and query instances of the published run (`scale=100 seed=1`, 10,973,381 triples),
copied from that run's work directory. Both engines loaded the data again in this run,
with the harness's load commands. Sparkles is the release build of commit `b62d3722` with
the SHA-256 `9b64590ed735903c9872875d5b2366dd25ff81c2df6023333b8850ea59cb7257`, and Fluree
is the flake's 4.2.2.

Each of the three rounds ran `QUERIES="L1 L3 S7"` with `SKIP_LOAD=1` under the host's
benchmark and heavy-job locks, so both servers started fresh, the answers were checked and
then every instance was timed, Sparkles first. Other benchmarks ran between the rounds.
`round-<n>` holds the hyperfine results of each instance and `answers.json`, and
`fwat-b62d3722-<n>.log` the harness output. Sparkles and Fluree returned the same value
fingerprints for all 15 instances in every round.

## Results

Means of the ten timed requests of each instance in ms, through `curl`, Sparkles against
Fluree.

| Instance | Round 1 | Round 2 | Round 3 |
|---|---|---|---|
| L1-1 | 3.568 / 3.573 | 3.713 / 3.795 | 3.632 / 3.778 |
| L1-2 | 3.593 / 3.877 | 3.641 / 3.582 | 3.682 / 3.499 |
| L1-3 | 3.639 / 3.711 | 3.730 / 3.869 | 3.677 / 3.832 |
| L1-4 | 3.657 / 3.742 | 3.592 / 3.722 | 3.579 / 3.722 |
| L1-5 | 3.661 / 3.766 | 3.563 / 3.655 | 3.718 / 3.543 |

| Template, geometric mean | Round 1 | Round 2 | Round 3 |
|---|---|---|---|
| L1 | 3.623 / 3.732 | 3.647 / 3.723 | 3.657 / 3.672 |
| L3 | 3.597 / 3.826 | 3.589 / 3.808 | 3.709 / 3.817 |
| S7 | 3.419 / 3.629 | 3.462 / 3.668 | 3.494 / 3.599 |

Sparkles has the lower mean on L1-1 in all three rounds and on the L1, L3 and S7 templates
in every round. An instance's means vary by about 0.2 ms between rounds for both engines.
The access log of a Sparkles server on `atlas`, a host of the same model, gives L1-1 a
median server time of 0.29 ms, of which execution is 0.05 ms, the lowest of the five L1
instances, so the remaining client time is the cost of a `curl` process and its request.
