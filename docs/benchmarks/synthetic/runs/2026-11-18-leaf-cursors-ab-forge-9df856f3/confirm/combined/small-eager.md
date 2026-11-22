| query | base ms | tip/base | 95% interval | added µs | tip-second ratio | tip-first ratio | same-build ratio | gate |
|---|--:|--:|---|--:|--:|--:|--:|---|
| contains | 2.5323 | 1.0062 | 1.0056-1.0066 | 15.75 | 1.0061 | 0.9937 |  | pass |
| count-all | 0.0045 | 1.0309 | 1.0058-1.0627 | 0.14 | 1.0262 | 0.9656 |  | pass |
| distinct-join | 3.5658 | 1.0219 | 1.0208-1.0231 | 78.16 | 1.0210 | 0.9777 |  | FAIL |
| distinct-obj | 0.0039 | 1.0204 | 0.9937-1.0297 | 0.08 | 1.0103 | 0.9703 |  | pass |
| employee-docs | 0.0362 | 1.0129 | 1.0049-1.0230 | 0.47 | 1.0096 | 0.9840 |  | pass |
| expr-bind-group | 0.5049 | 1.0000 | 0.9973-1.0029 | 0.02 | 0.9986 | 0.9985 |  | pass |
| lang-filter | 1.7914 | 1.0271 | 1.0263-1.0276 | 48.46 | 1.0269 | 0.9735 |  | FAIL |
| minus | 0.3613 | 1.0075 | 1.0033-1.0099 | 2.69 | 1.0084 | 0.9935 |  | pass |
| path-plus | 0.0111 | 1.0348 | 1.0199-1.0488 | 0.39 | 1.0804 | 1.0090 |  | pass |
| predicate-counts | 0.0047 | 0.9889 | 0.9811-1.0170 | -0.05 | 0.9528 | 0.9743 |  | pass |
| predicate-subjects | 0.0065 | 1.0324 | 1.0181-1.0490 | 0.21 | 1.0074 | 0.9453 |  | pass |
| regex-iri | 3.7161 | 1.0105 | 1.0093-1.0114 | 39.16 | 1.0105 | 0.9895 |  | pass |
| star-join | 1.3886 | 1.0192 | 1.0163-1.0222 | 26.60 | 1.0178 | 0.9799 |  | FAIL |
| star-lookup | 0.0591 | 1.0175 | 1.0124-1.0261 | 1.03 | 1.0178 | 0.9831 |  | FAIL |
| subquery-agg | 0.1849 | 0.9882 | 0.9835-0.9907 | -2.19 | 0.9880 | 1.0118 |  | pass |
| types-grouped | 0.0076 | 0.9969 | 0.9804-1.0065 | -0.02 | 0.9768 | 0.9829 |  | pass |
| values-star | 0.0156 | 1.0226 | 1.0136-1.0267 | 0.35 | 1.0254 | 0.9805 |  | pass |

FAIL: distinct-join: tip/base 1.0219 up to 1.0231, 78.16 µs added
FAIL: lang-filter: tip/base 1.0271 up to 1.0276, 48.46 µs added
FAIL: star-join: tip/base 1.0192 up to 1.0222, 26.60 µs added
FAIL: star-lookup: tip/base 1.0175 up to 1.0261, 1.03 µs added
the latency gate fails
