| query | base ms | tip/base | 95% interval | added µs | tip-second ratio | tip-first ratio | same-build ratio | gate |
|---|--:|--:|---|--:|--:|--:|--:|---|
| contains | 2.5334 | 1.0062 | 1.0055-1.0070 | 15.79 | 1.0063 | 0.9938 |  | pass |
| count-all | 0.0061 | 1.0101 | 0.9876-1.0496 | 0.06 | 0.9826 | 0.9631 |  | pass |
| distinct-join | 3.7458 | 1.0351 | 1.0345-1.0358 | 131.34 | 1.0356 | 0.9666 |  | FAIL |
| distinct-obj | 0.0053 | 1.0214 | 0.9984-1.0489 | 0.11 | 1.0171 | 0.9750 |  | pass |
| employee-docs | 0.0408 | 1.0388 | 1.0299-1.0532 | 1.59 | 1.0437 | 0.9671 |  | FAIL |
| expr-bind-group | 0.3585 | 1.0130 | 1.0093-1.0151 | 4.66 | 1.0140 | 0.9881 |  | pass |
| lang-filter | 1.7901 | 1.0278 | 1.0271-1.0285 | 49.69 | 1.0276 | 0.9729 |  | FAIL |
| minus | 0.2575 | 0.9987 | 0.9967-1.0012 | -0.33 | 1.0009 | 1.0035 |  | pass |
| path-plus | 0.0116 | 0.9914 | 0.9791-1.0080 | -0.10 | 0.9798 | 0.9967 |  | pass |
| predicate-counts | 0.0066 | 1.0141 | 1.0003-1.0369 | 0.09 | 0.9938 | 0.9664 |  | pass |
| predicate-subjects | 0.0086 | 1.0214 | 1.0090-1.0378 | 0.18 | 1.0015 | 0.9599 |  | pass |
| regex-iri | 3.7227 | 1.0097 | 1.0084-1.0104 | 36.11 | 1.0094 | 0.9901 |  | pass |
| star-join | 1.4092 | 1.0127 | 1.0101-1.0155 | 17.88 | 1.0122 | 0.9870 |  | pass |
| star-lookup | 0.0630 | 1.0320 | 1.0266-1.0365 | 2.02 | 1.0291 | 0.9662 |  | FAIL |
| subquery-agg | 0.1819 | 0.9957 | 0.9916-0.9975 | -0.78 | 0.9940 | 1.0025 |  | pass |
| types-grouped | 0.0123 | 1.0137 | 1.0029-1.0267 | 0.17 | 1.0192 | 0.9920 |  | pass |
| values-star | 0.0198 | 1.0515 | 1.0414-1.0627 | 1.02 | 1.0549 | 0.9540 |  | FAIL |

FAIL: distinct-join: tip/base 1.0351 up to 1.0358, 131.34 µs added
FAIL: employee-docs: tip/base 1.0388 up to 1.0532, 1.59 µs added
FAIL: lang-filter: tip/base 1.0278 up to 1.0285, 49.69 µs added
FAIL: star-lookup: tip/base 1.0320 up to 1.0365, 2.02 µs added
FAIL: values-star: tip/base 1.0515 up to 1.0627, 1.02 µs added
the latency gate fails
