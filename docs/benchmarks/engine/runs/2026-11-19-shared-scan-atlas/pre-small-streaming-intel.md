| query | pool off ms | on/off | 95% interval | added µs | pool-on-second ratio | pool-on-first ratio | same-build ratio | gate |
|---|--:|--:|---|--:|--:|--:|--:|---|
| count-all | 0.0055 | 1.0386 | 1.0282-1.0605 | 0.21 | 1.0604 | 0.9830 |  | pass |
| distinct-obj | 0.0049 | 1.0423 | 1.0197-1.0506 | 0.21 | 1.0713 | 0.9861 |  | pass |
| path-plus | 0.0105 | 1.0098 | 0.9854-1.0263 | 0.10 | 0.9729 | 0.9541 |  | pass |
| predicate-counts | 0.0062 | 1.0094 | 0.9977-1.0248 | 0.06 | 1.0307 | 1.0115 |  | pass |
| root-limit | 0.0036 | 1.0387 | 1.0039-1.0745 | 0.14 | 1.0617 | 0.9841 |  | pass |
| root-scan | 0.0824 | 1.0240 | 1.0208-1.0270 | 1.98 | 1.0248 | 0.9773 |  | FAIL |
| star-lookup | 0.0638 | 0.9881 | 0.9814-0.9982 | -0.76 | 0.9879 | 1.0118 |  | pass |
| subquery-agg | 0.1815 | 1.0063 | 1.0020-1.0089 | 1.14 | 1.0085 | 0.9959 |  | pass |
| values-star | 0.0190 | 1.0139 | 1.0012-1.0264 | 0.26 | 1.0328 | 1.0046 |  | pass |

FAIL: root-scan: on/off 1.0240 up to 1.0270, 1.98 µs added
the latency gate fails
