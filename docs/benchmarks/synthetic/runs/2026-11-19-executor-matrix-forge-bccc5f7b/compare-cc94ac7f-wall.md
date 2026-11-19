# executors gate: candidate against baseline

- baseline: host forge, commit cc94ac7f, started 2026-11-18T07:04:20Z
- candidate: host forge, commit bccc5f7b, started 2026-11-19T00:08:03Z
- threshold: 5.0% slower, floor 5.0, 0.0% below 0.3 ms

| metric | baseline | candidate | ratio | flag |
|---|--:|--:|--:|---|
| 1.05m-1t/eager/distinct-join/wall | 5.032 | 5.0555 | 1.005 |  |
| 1.05m-1t/streaming/distinct-join/wall | 5.975 | 5.9785 | 1.001 |  |
| 1.05m-1t/eager/export-500k/wall | 250.03 | 229.041 | 0.916 |  |
| 1.05m-1t/streaming/export-500k/wall | 338.984 | 363.045 | 1.071 | slower |
| 1.05m-1t/eager/group-avg/wall | 5.32 | 4.893 | 0.920 |  |
| 1.05m-1t/eager/knows-reach/wall | 12.8 | 12.5685 | 0.982 |  |
| 1.05m-1t/streaming/knows-reach/wall | 12.6595 | 12.691 | 1.002 |  |
| 1.05m-1t/eager/optional-chain/wall | 15.129 | 15.0495 | 0.995 |  |
| 1.05m-1t/streaming/optional-chain/wall | 21.0465 | 22.042 | 1.047 |  |
| 1.05m-1t/eager/order-by-full/wall | 77.2985 | 77.044 | 0.997 |  |
| 1.05m-1t/streaming/order-by-full/wall | 95.005 | 98.041 | 1.032 |  |
| 1.05m-1t/eager/star-join/wall | 5.0695 | 4.944 | 0.975 |  |
| 1.05m-1t/streaming/star-join/wall | 6.111 | 7.0055 | 1.146 | slower |
| 1.05m-12t/streaming/distinct-join/wall | 5.688 | 5.5425 | 0.974 |  |
| 1.05m-12t/eager/export-500k/wall | 236.463 | 216.165 | 0.914 |  |
| 1.05m-12t/streaming/export-500k/wall | 270.646 | 292.122 | 1.079 | slower |
| 1.05m-12t/eager/group-avg/wall | 5.027 | 4.737 | 0.942 |  |
| 1.05m-12t/eager/knows-reach/wall | 12.7965 | 12.71 | 0.993 |  |
| 1.05m-12t/streaming/knows-reach/wall | 12.763 | 12.824 | 1.005 |  |
| 1.05m-12t/eager/optional-chain/wall | 14.561 | 14.1455 | 0.971 |  |
| 1.05m-12t/streaming/optional-chain/wall | 17.7105 | 18.6625 | 1.054 | slower |
| 1.05m-12t/eager/order-by-full/wall | 43.3055 | 42.076 | 0.972 |  |
| 1.05m-12t/streaming/order-by-full/wall | 48.442 | 52.1165 | 1.076 | slower |
| 1.05m-12t/eager/star-join/wall | 5.02 | 4.876 | 0.971 |  |
| 1.05m-12t/streaming/star-join/wall | 5.7385 | 5.943 | 1.036 |  |
| 10.5m-1t/eager/class-predicate-list/wall | 11.252 | 11.379 | 1.011 |  |
| 10.5m-1t/streaming/class-predicate-list/wall | 11.6015 | 11.7705 | 1.015 |  |
| 10.5m-1t/eager/class-predicates/wall | 11.2435 | 11.4505 | 1.018 |  |
| 10.5m-1t/streaming/class-predicates/wall | 11.6575 | 11.8155 | 1.014 |  |
| 10.5m-1t/eager/contains/wall | 25.625 | 25.8015 | 1.007 |  |
| 10.5m-1t/streaming/contains/wall | 25.7005 | 25.846 | 1.006 |  |
| 10.5m-1t/eager/distinct-join/wall | 43.7165 | 43.587 | 0.997 |  |
| 10.5m-1t/streaming/distinct-join/wall | 44.9425 | 45.75 | 1.018 |  |
| 10.5m-1t/eager/exists-join/wall | 9.8705 | 9.9885 | 1.012 |  |
| 10.5m-1t/streaming/exists-join/wall | 9.8725 | 9.9625 | 1.009 |  |
| 10.5m-1t/eager/export-500k/wall | 256.046 | 234.96 | 0.918 |  |
| 10.5m-1t/streaming/export-500k/wall | 336.033 | 362.973 | 1.080 | slower |
| 10.5m-1t/eager/expr-agg-arg/wall | 30.6045 | 26.712 | 0.873 |  |
| 10.5m-1t/streaming/expr-agg-arg/wall | 23.7425 | 23.871 | 1.005 |  |
| 10.5m-1t/eager/expr-bind-group/wall | 6.98 | 6.034 | 0.864 |  |
| 10.5m-1t/eager/expr-order-key/wall | 12.907 | 13.0315 | 1.010 |  |
| 10.5m-1t/streaming/expr-order-key/wall | 12.301 | 12.3895 | 1.007 |  |
| 10.5m-1t/eager/group-avg/wall | 46.93 | 41.08 | 0.875 |  |
| 10.5m-1t/streaming/group-avg/wall | 38.327 | 39.546 | 1.032 |  |
| 10.5m-1t/eager/knows-reach/wall | 140.929 | 138.23 | 0.981 |  |
| 10.5m-1t/streaming/knows-reach/wall | 138.226 | 138.493 | 1.002 |  |
| 10.5m-1t/eager/lang-filter/wall | 17.261 | 18.8015 | 1.089 | slower |
| 10.5m-1t/streaming/lang-filter/wall | 17.263 | 18.8695 | 1.093 | slower |
| 10.5m-1t/eager/not-exists/wall | 10.807 | 10.241 | 0.948 |  |
| 10.5m-1t/streaming/not-exists/wall | 9.956 | 9.9465 | 0.999 |  |
| 10.5m-1t/eager/optional-chain/wall | 142.571 | 136.004 | 0.954 |  |
| 10.5m-1t/streaming/optional-chain/wall | 207.165 | 222.053 | 1.072 | slower |
| 10.5m-1t/eager/optional-count/wall | 6.627 | 5.825 | 0.879 |  |
| 10.5m-1t/streaming/optional-count/wall | 6.2765 | 6.583 | 1.049 |  |
| 10.5m-1t/eager/order-by-full/wall | 865.074 | 858.048 | 0.992 |  |
| 10.5m-1t/streaming/order-by-full/wall | 1029.99 | 1066.69 | 1.036 |  |
| 10.5m-1t/eager/range-topk/wall | 6.423 | 6.45 | 1.004 |  |
| 10.5m-1t/streaming/range-topk/wall | 6.43 | 6.4475 | 1.003 |  |
| 10.5m-1t/eager/regex-iri/wall | 35.7255 | 39.011 | 1.092 | slower |
| 10.5m-1t/streaming/regex-iri/wall | 35.728 | 38.985 | 1.091 | slower |
| 10.5m-1t/eager/star-join/wall | 37.048 | 36.058 | 0.973 |  |
| 10.5m-1t/streaming/star-join/wall | 51.1085 | 52.274 | 1.023 |  |
| 10.5m-1t/eager/two-hop-count/wall | 29.109 | 29.1715 | 1.002 |  |
| 10.5m-1t/streaming/two-hop-count/wall | 29.316 | 29.348 | 1.001 |  |
| 10.5m-12t/eager/class-predicate-list/wall | 11.285 | 11.535 | 1.022 |  |
| 10.5m-12t/streaming/class-predicate-list/wall | 11.634 | 11.824 | 1.016 |  |
| 10.5m-12t/eager/class-predicates/wall | 11.2615 | 11.4625 | 1.018 |  |
| 10.5m-12t/streaming/class-predicates/wall | 11.7405 | 11.7825 | 1.004 |  |
| 10.5m-12t/eager/contains/wall | 5.011 | 5.089 | 1.016 |  |
| 10.5m-12t/streaming/contains/wall | 5.1625 | 5.2175 | 1.011 |  |
| 10.5m-12t/eager/distinct-join/wall | 44.702 | 44.292 | 0.991 |  |
| 10.5m-12t/streaming/distinct-join/wall | 43.7235 | 43.956 | 1.005 |  |
| 10.5m-12t/eager/exists-join/wall | 9.9305 | 10.096 | 1.017 |  |
| 10.5m-12t/streaming/exists-join/wall | 10.01 | 9.9095 | 0.990 |  |
| 10.5m-12t/eager/export-500k/wall | 242.538 | 228.272 | 0.941 |  |
| 10.5m-12t/streaming/export-500k/wall | 266.629 | 289.663 | 1.086 | slower |
| 10.5m-12t/eager/expr-agg-arg/wall | 29.963 | 25.147 | 0.839 |  |
| 10.5m-12t/streaming/expr-agg-arg/wall | 24.4145 | 24.552 | 1.006 |  |
| 10.5m-12t/eager/expr-bind-group/wall | 5.678 | 4.4955 | 0.792 |  |
| 10.5m-12t/eager/expr-order-key/wall | 11.899 | 11.87 | 0.998 |  |
| 10.5m-12t/streaming/expr-order-key/wall | 12.267 | 12.401 | 1.011 |  |
| 10.5m-12t/eager/group-avg/wall | 47.573 | 41.8035 | 0.879 |  |
| 10.5m-12t/streaming/group-avg/wall | 38.805 | 40.4835 | 1.043 |  |
| 10.5m-12t/eager/knows-reach/wall | 144.935 | 143.335 | 0.989 |  |
| 10.5m-12t/streaming/knows-reach/wall | 141.739 | 141.608 | 0.999 |  |
| 10.5m-12t/eager/not-exists/wall | 11.244 | 10.1305 | 0.901 |  |
| 10.5m-12t/streaming/not-exists/wall | 10.097 | 10.1615 | 1.006 |  |
| 10.5m-12t/eager/optional-chain/wall | 155.953 | 133.347 | 0.855 |  |
| 10.5m-12t/streaming/optional-chain/wall | 161.172 | 173.888 | 1.079 | slower |
| 10.5m-12t/eager/optional-count/wall | 6.3535 | 5.5085 | 0.867 |  |
| 10.5m-12t/streaming/optional-count/wall | 6.2435 | 6.565 | 1.051 | slower |
| 10.5m-12t/eager/order-by-full/wall | 528.413 | 469.392 | 0.888 |  |
| 10.5m-12t/streaming/order-by-full/wall | 555.395 | 566.968 | 1.021 |  |
| 10.5m-12t/eager/regex-iri/wall | 7.008 | 7.135 | 1.018 |  |
| 10.5m-12t/streaming/regex-iri/wall | 7.217 | 7.341 | 1.017 |  |
| 10.5m-12t/eager/star-join/wall | 34.5345 | 33.808 | 0.979 |  |
| 10.5m-12t/streaming/star-join/wall | 42.5645 | 43.3965 | 1.020 |  |
| 10.5m-12t/eager/two-hop-count/wall | 12.497 | 12.5705 | 1.006 |  |
| 10.5m-12t/streaming/two-hop-count/wall | 12.439 | 12.5385 | 1.008 |  |

99 metrics compared, 14 flagged, 0 failed checks, 0 missing checks, 0 changed fingerprints.
