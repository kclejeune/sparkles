| query | pool off ms | on/off | 95% interval | added µs | pool-on-second ratio | pool-on-first ratio | same-build ratio | gate |
|---|--:|--:|---|--:|--:|--:|--:|---|
| count-all | 0.0046 | 0.9781 | 0.9605-0.9900 | -0.10 | 0.9307 | 0.9729 |  | pass |
| distinct-obj | 0.0040 | 0.9763 | 0.9542-0.9923 | -0.09 | 0.9599 | 1.0072 |  | pass |
| path-plus | 0.0110 | 0.9936 | 0.9764-1.0053 | -0.07 | 1.0140 | 1.0272 |  | pass |
| predicate-counts | 0.0048 | 0.9864 | 0.9719-0.9960 | -0.07 | 0.9382 | 0.9643 |  | pass |
| root-limit | 0.0114 | 0.9702 | 0.8933-0.9936 | -0.34 | 0.9701 | 1.0306 |  | pass |
| root-scan | 0.0871 | 0.9724 | 0.9700-0.9764 | -2.41 | 0.9737 | 1.0299 |  | pass |
| star-lookup | 0.0609 | 0.9929 | 0.9839-0.9991 | -0.43 | 1.0055 | 1.0200 |  | pass |
| subquery-agg | 0.1845 | 1.0032 | 0.9992-1.0073 | 0.59 | 1.0040 | 0.9976 |  | pass |
| values-star | 0.0154 | 0.9679 | 0.9602-0.9753 | -0.49 | 0.9765 | 1.0424 |  | pass |

the latency gate passes
