# Stage 4 against main on forge

A manual report. Each table combines the two orders of one A/B with the rule of
`scripts/x08-gates/combine.py`, the A/B harness's combiner. Runs q1 and q3 have eight rounds,
and their tables come from the same rule with its minimum of ten rounds lowered to eight.
The ratio is the candidate's execution time over main's, and the gate column applies
decision D4's rule for queries under 0.3 ms, which longer queries replace with the 5%
rule. Every run's two servers returned the same answer fingerprints, which each
`answers.tsv` lists.

| build | commit | sha256 |
|---|---|---|
| main | 58bbfca5 | 9ca4ecbb19e14bcbe6f5e41a4e018e73d618077a33dc170124c91dbec0bbbe64 |
| candidate of j1, k1 | fec1a9aa | 53d3459a2501b22b2f73cbb21e224a423d57184876ea1e7eb4ff64f147111f58 |
| candidate of q1, q2, q3 | 815822db | 00e54e68a364435e422c10d0385f36cbea236a71c7556787a2503629838b0059 |

## q1, eager: 10.5M triples, 12 threads, 8 rounds of 30, eager

| query | main ms | ratio | 95% interval | added µs | candidate-second ratio | candidate-first ratio | same-build ratio | gate |
|---|--:|--:|---|--:|--:|--:|--:|---|
| expr-agg-arg | 28.3662 | 0.9946 | 0.9914-0.9991 | -152.87 | 0.9945 | 1.0053 |  | pass |
| expr-bind-group | 4.6628 | 1.0232 | 1.0141-1.0271 | 108.15 | 1.0229 | 0.9770 |  | FAIL |
| group-avg | 47.8342 | 0.9983 | 0.9955-1.0011 | -79.40 | 0.9976 | 1.0009 |  | pass |
| lang-filter | 3.0179 | 1.0111 | 1.0060-1.0185 | 33.37 | 1.0099 | 0.9880 |  | pass |
| regex-iri | 6.2733 | 1.0204 | 1.0186-1.0227 | 127.67 | 1.0212 | 0.9809 |  | FAIL |
| subquery-agg | 0.6856 | 1.0292 | 1.0071-1.0607 | 20.04 | 1.0393 | 0.9811 |  | FAIL |

## q2, eager: 1.05M triples, 12 threads, 10 rounds of 30

| query | main ms | ratio | 95% interval | added µs | candidate-second ratio | candidate-first ratio | same-build ratio | gate |
|---|--:|--:|---|--:|--:|--:|--:|---|
| expr-agg-arg | 2.8881 | 0.9943 | 0.9895-0.9971 | -16.49 | 0.9939 | 1.0053 |  | pass |
| expr-bind-group | 0.7391 | 1.0194 | 1.0080-1.0573 | 14.36 | 1.0224 | 0.9838 |  | FAIL |
| group-avg | 4.2862 | 1.0003 | 0.9899-1.0071 | 1.15 | 0.9943 | 0.9937 |  | pass |
| lang-filter | 0.7314 | 0.9893 | 0.9658-1.0319 | -7.82 | 0.9871 | 1.0085 |  | pass |
| regex-iri | 0.9744 | 0.9978 | 0.9868-1.0192 | -2.16 | 0.9802 | 0.9846 |  | pass |
| star-join | 1.5511 | 1.0060 | 0.9911-1.0260 | 9.37 | 1.0010 | 0.9890 |  | FAIL |

## q2, streaming: 1.05M triples, 12 threads, 10 rounds of 30

| query | main ms | ratio | 95% interval | added µs | candidate-second ratio | candidate-first ratio | same-build ratio | gate |
|---|--:|--:|---|--:|--:|--:|--:|---|
| expr-agg-arg | 2.0217 | 1.0010 | 0.9894-1.0067 | 2.07 | 1.0037 | 1.0016 |  | pass |
| expr-bind-group | 0.3979 | 1.0005 | 0.9905-1.0105 | 0.21 | 0.9997 | 0.9987 |  | pass |
| group-avg | 3.6623 | 0.9776 | 0.9697-0.9818 | -81.92 | 0.9772 | 1.0225 |  | pass |
| lang-filter | 0.7359 | 0.9861 | 0.9603-1.0246 | -10.22 | 1.0106 | 1.0392 |  | pass |
| regex-iri | 0.9944 | 1.0057 | 0.9882-1.0347 | 5.65 | 0.9966 | 0.9853 |  | FAIL |
| star-join | 1.5986 | 1.0103 | 0.9932-1.0275 | 16.44 | 1.0065 | 0.9861 |  | FAIL |

## q3, streaming: 10.5M triples, 12 threads, 8 rounds of 30, streamed

| query | main ms | ratio | 95% interval | added µs | candidate-second ratio | candidate-first ratio | same-build ratio | gate |
|---|--:|--:|---|--:|--:|--:|--:|---|
| expr-agg-arg | 23.0673 | 1.0001 | 0.9955-1.0029 | 3.25 | 1.0034 | 1.0031 |  | pass |
| expr-bind-group | 3.4865 | 1.0043 | 0.9978-1.0112 | 15.10 | 1.0046 | 0.9959 |  | pass |
| group-avg | 40.0243 | 0.9884 | 0.9846-0.9910 | -465.24 | 0.9897 | 1.0131 |  | pass |
| lang-filter | 3.0505 | 0.9870 | 0.9803-0.9959 | -39.60 | 0.9813 | 1.0073 |  | pass |
| regex-iri | 6.4394 | 0.9978 | 0.9963-1.0046 | -13.89 | 0.9959 | 1.0003 |  | pass |
| subquery-agg | 0.7918 | 1.0112 | 0.9886-1.0323 | 8.87 | 1.0123 | 0.9900 |  | FAIL |

## j1, eager: 1.05M triples, one thread, 10 rounds of 30

| query | main ms | ratio | 95% interval | added µs | candidate-second ratio | candidate-first ratio | same-build ratio | gate |
|---|--:|--:|---|--:|--:|--:|--:|---|
| regex-iri | 3.7614 | 0.9904 | 0.9892-0.9912 | -36.20 | 0.9908 | 1.0102 |  | pass |

## j1, streaming: 1.05M triples, one thread, 10 rounds of 30

| query | main ms | ratio | 95% interval | added µs | candidate-second ratio | candidate-first ratio | same-build ratio | gate |
|---|--:|--:|---|--:|--:|--:|--:|---|
| regex-iri | 3.8024 | 0.9782 | 0.9765-0.9806 | -82.98 | 0.9786 | 1.0227 |  | pass |

## k1, eager: 1.05M triples, 12 threads, 12 rounds of 200, eager, fec1a9aa against main

| query | main ms | ratio | 95% interval | added µs | candidate-second ratio | candidate-first ratio | same-build ratio | gate |
|---|--:|--:|---|--:|--:|--:|--:|---|
| subquery-agg | 0.1949 | 0.9986 | 0.9661-1.0322 | -0.28 | 1.0100 | 1.0129 |  | pass |

## k2, eager: 1.05M triples, 12 threads, 12 rounds of 200, eager, main against itself

| query | main ms | ratio | 95% interval | added µs | candidate-second ratio | candidate-first ratio | same-build ratio | gate |
|---|--:|--:|---|--:|--:|--:|--:|---|
| subquery-agg | 0.1919 | 0.9808 | 0.9383-1.0306 | -3.69 | 0.9724 | 1.0109 |  | pass |

