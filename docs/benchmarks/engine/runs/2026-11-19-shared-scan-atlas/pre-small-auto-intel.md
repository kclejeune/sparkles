| query | pool off ms | on/off | 95% interval | added µs | pool-on-second ratio | pool-on-first ratio | same-build ratio | gate |
|---|--:|--:|---|--:|--:|--:|--:|---|
| count-all | 0.0047 | 1.0269 | 0.9966-1.0504 | 0.13 | 0.9846 | 0.9337 |  | pass |
| distinct-obj | 0.0042 | 1.0146 | 0.9908-1.0376 | 0.06 | 0.9627 | 0.9351 |  | pass |
| path-plus | 0.0099 | 1.1450 | 1.1196-1.1581 | 1.43 | 1.1125 | 0.8487 |  | FAIL |
| predicate-counts | 0.0049 | 1.0226 | 0.9997-1.0476 | 0.11 | 0.9845 | 0.9415 |  | pass |
| root-limit | 0.0115 | 1.0397 | 1.0109-1.0875 | 0.46 | 1.0262 | 0.9493 |  | pass |
| root-scan | 0.0839 | 1.0106 | 1.0067-1.0141 | 0.89 | 1.0118 | 0.9907 |  | pass |
| star-lookup | 0.0618 | 0.9993 | 0.9905-1.0092 | -0.05 | 0.9903 | 0.9918 |  | pass |
| subquery-agg | 0.1832 | 1.0132 | 1.0098-1.0187 | 2.41 | 1.0129 | 0.9867 |  | pass |
| values-star | 0.0159 | 0.9845 | 0.9736-0.9959 | -0.25 | 0.9561 | 0.9865 |  | pass |

FAIL: path-plus: on/off 1.1450 up to 1.1581, 1.43 µs added
the latency gate fails
