| query | pool off ms | on/off | 95% interval | added µs | pool-on-second ratio | pool-on-first ratio | same-build ratio | gate |
|---|--:|--:|---|--:|--:|--:|--:|---|
| count-all | 0.0054 | 1.0386 | 1.0292-1.0607 | 0.21 | 1.0604 | 0.9830 |  | pass |
| distinct-obj | 0.0048 | 1.0423 | 1.0219-1.0532 | 0.20 | 1.0713 | 0.9861 |  | pass |
| path-plus | 0.0105 | 1.0098 | 0.9849-1.0257 | 0.10 | 0.9729 | 0.9541 |  | pass |
| predicate-counts | 0.0061 | 1.0079 | 0.9953-1.0225 | 0.05 | 1.0276 | 1.0115 |  | pass |
| root-limit | 0.0036 | 1.0314 | 1.0026-1.0674 | 0.11 | 1.0468 | 0.9841 |  | pass |
| root-scan | 0.0823 | 1.0240 | 1.0208-1.0276 | 1.97 | 1.0248 | 0.9773 |  | FAIL |
| star-lookup | 0.0636 | 0.9872 | 0.9800-0.9975 | -0.81 | 0.9860 | 1.0118 |  | pass |
| subquery-agg | 0.1814 | 1.0049 | 1.0011-1.0084 | 0.89 | 1.0057 | 0.9959 |  | pass |
| values-star | 0.0189 | 1.0139 | 1.0025-1.0261 | 0.26 | 1.0328 | 1.0046 |  | pass |

FAIL: root-scan: on/off 1.0240 up to 1.0276, 1.97 µs added
the latency gate fails
