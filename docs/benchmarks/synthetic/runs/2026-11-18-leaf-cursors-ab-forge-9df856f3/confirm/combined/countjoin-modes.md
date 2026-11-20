| query | eager ms | streaming/eager | 95% interval | added µs | streaming-second ratio | streaming-first ratio | same-build ratio | gate |
|---|--:|--:|---|--:|--:|--:|--:|---|
| cj-filter | 55.3029 | 0.1515 | 0.1513-0.1517 | -46924.47 | 0.1514 | 6.5977 |  | pass |
| cj-two-var | 5.2825 | 1.8128 | 1.8074-1.8149 | 4293.67 | 1.8134 | 0.5518 |  | FAIL |
| two-hop-count | 2.6412 | 0.9913 | 0.9886-0.9942 | -22.92 | 0.9895 | 1.0069 |  | pass |

FAIL: cj-two-var: streaming/eager 1.8128 up to 1.8149, 4293.67 µs added
the latency gate fails
