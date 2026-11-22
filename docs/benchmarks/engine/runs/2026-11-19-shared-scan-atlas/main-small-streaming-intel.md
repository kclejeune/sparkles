| query | pool off ms | on/off | 95% interval | added µs | pool-on-second ratio | pool-on-first ratio | same-build ratio | gate |
|---|--:|--:|---|--:|--:|--:|--:|---|
| count-all | 0.0057 | 1.0138 | 0.9818-1.0430 | 0.08 | 1.0078 | 0.9805 |  | pass |
| distinct-obj | 0.0049 | 1.0616 | 1.0411-1.0928 | 0.30 | 1.0698 | 0.9492 |  | pass |
| path-plus | 0.0106 | 1.0203 | 0.9893-1.0453 | 0.21 | 1.0205 | 0.9803 |  | pass |
| predicate-counts | 0.0061 | 1.0504 | 1.0331-1.0821 | 0.31 | 1.0668 | 0.9670 |  | pass |
| root-limit | 0.0040 | 0.9648 | 0.9395-0.9879 | -0.14 | 0.9074 | 0.9748 |  | pass |
| root-scan | 0.0855 | 0.9818 | 0.9800-0.9855 | -1.55 | 0.9828 | 1.0195 |  | pass |
| star-lookup | 0.0640 | 0.9982 | 0.9833-1.0059 | -0.12 | 1.0116 | 1.0152 |  | pass |
| subquery-agg | 0.1828 | 1.0025 | 0.9983-1.0082 | 0.45 | 1.0011 | 0.9962 |  | pass |
| values-star | 0.0193 | 0.9999 | 0.9909-1.0054 | -0.00 | 1.0158 | 1.0161 |  | pass |

the latency gate passes
