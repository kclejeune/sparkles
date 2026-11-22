# executors gate: candidate against baseline

- baseline: host forge, commit eda22fce, started 2026-11-19T02:31:44Z
- candidate: host forge, commit bccc5f7b, started 2026-11-19T00:08:03Z
- threshold: 5.0% slower, floor 5.0, 0.0% below 0.3 ms

| metric | baseline | candidate | ratio | flag |
|---|--:|--:|--:|---|
| 1.05m-1t/eager/distinct-join/wall | 5.311 | 5.0555 | 0.952 |  |
| 1.05m-1t/streaming/distinct-join/wall | 5.978 | 5.9785 | 1.000 |  |
| 1.05m-1t/eager/export-500k/wall | 232.748 | 229.041 | 0.984 |  |
| 1.05m-1t/streaming/export-500k/wall | 346.026 | 363.045 | 1.049 |  |
| 1.05m-1t/eager/group-avg/wall | 5.144 | 4.893 | 0.951 |  |
| 1.05m-1t/eager/knows-reach/wall | 12.745 | 12.5685 | 0.986 |  |
| 1.05m-1t/streaming/knows-reach/wall | 12.641 | 12.691 | 1.004 |  |
| 1.05m-1t/eager/optional-chain/wall | 15.046 | 15.0495 | 1.000 |  |
| 1.05m-1t/streaming/optional-chain/wall | 21.032 | 22.042 | 1.048 |  |
| 1.05m-1t/eager/order-by-full/wall | 76.071 | 77.044 | 1.013 |  |
| 1.05m-1t/streaming/order-by-full/wall | 95.2165 | 98.041 | 1.030 |  |
| 1.05m-1t/eager/star-join/wall | 5.047 | 4.944 | 0.980 |  |
| 1.05m-1t/streaming/star-join/wall | 6.977 | 7.0055 | 1.004 |  |
| 1.05m-12t/eager/distinct-join/wall | 5.1115 | 4.949 | 0.968 |  |
| 1.05m-12t/streaming/distinct-join/wall | 5.57 | 5.5425 | 0.995 |  |
| 1.05m-12t/eager/export-500k/wall | 218.201 | 216.165 | 0.991 |  |
| 1.05m-12t/streaming/export-500k/wall | 274.777 | 292.122 | 1.063 | slower |
| 1.05m-12t/eager/knows-reach/wall | 12.8475 | 12.71 | 0.989 |  |
| 1.05m-12t/streaming/knows-reach/wall | 12.558 | 12.824 | 1.021 |  |
| 1.05m-12t/eager/optional-chain/wall | 14.018 | 14.1455 | 1.009 |  |
| 1.05m-12t/streaming/optional-chain/wall | 17.8885 | 18.6625 | 1.043 |  |
| 1.05m-12t/eager/order-by-full/wall | 41.102 | 42.076 | 1.024 |  |
| 1.05m-12t/streaming/order-by-full/wall | 48.714 | 52.1165 | 1.070 | slower |
| 1.05m-12t/eager/star-join/wall | 5.08 | 4.876 | 0.960 |  |
| 1.05m-12t/streaming/star-join/wall | 5.8565 | 5.943 | 1.015 |  |
| 10.5m-1t/eager/class-predicate-list/wall | 11.4605 | 11.379 | 0.993 |  |
| 10.5m-1t/streaming/class-predicate-list/wall | 11.6835 | 11.7705 | 1.007 |  |
| 10.5m-1t/eager/class-predicates/wall | 11.3165 | 11.4505 | 1.012 |  |
| 10.5m-1t/streaming/class-predicates/wall | 11.755 | 11.8155 | 1.005 |  |
| 10.5m-1t/eager/contains/wall | 25.94 | 25.8015 | 0.995 |  |
| 10.5m-1t/streaming/contains/wall | 25.932 | 25.846 | 0.997 |  |
| 10.5m-1t/eager/distinct-join/wall | 44.279 | 43.587 | 0.984 |  |
| 10.5m-1t/streaming/distinct-join/wall | 45.943 | 45.75 | 0.996 |  |
| 10.5m-1t/eager/exists-join/wall | 9.927 | 9.9885 | 1.006 |  |
| 10.5m-1t/streaming/exists-join/wall | 9.9515 | 9.9625 | 1.001 |  |
| 10.5m-1t/eager/export-500k/wall | 236.022 | 234.96 | 0.996 |  |
| 10.5m-1t/streaming/export-500k/wall | 342.02 | 362.973 | 1.061 | slower |
| 10.5m-1t/eager/expr-agg-arg/wall | 29.4805 | 26.712 | 0.906 |  |
| 10.5m-1t/streaming/expr-agg-arg/wall | 24.4215 | 23.871 | 0.977 |  |
| 10.5m-1t/eager/expr-bind-group/wall | 6.866 | 6.034 | 0.879 |  |
| 10.5m-1t/eager/expr-order-key/wall | 13.058 | 13.0315 | 0.998 |  |
| 10.5m-1t/streaming/expr-order-key/wall | 12.2385 | 12.3895 | 1.012 |  |
| 10.5m-1t/eager/group-avg/wall | 46.2595 | 41.08 | 0.888 |  |
| 10.5m-1t/streaming/group-avg/wall | 38.514 | 39.546 | 1.027 |  |
| 10.5m-1t/eager/knows-reach/wall | 140.419 | 138.23 | 0.984 |  |
| 10.5m-1t/streaming/knows-reach/wall | 137.868 | 138.493 | 1.005 |  |
| 10.5m-1t/eager/lang-filter/wall | 19.4715 | 18.8015 | 0.966 |  |
| 10.5m-1t/streaming/lang-filter/wall | 19.6705 | 18.8695 | 0.959 |  |
| 10.5m-1t/eager/minus/wall | 5.479 | 5.039 | 0.920 |  |
| 10.5m-1t/eager/not-exists/wall | 10.7515 | 10.241 | 0.953 |  |
| 10.5m-1t/streaming/not-exists/wall | 9.962 | 9.9465 | 0.998 |  |
| 10.5m-1t/eager/optional-chain/wall | 135.031 | 136.004 | 1.007 |  |
| 10.5m-1t/streaming/optional-chain/wall | 213.014 | 222.053 | 1.042 |  |
| 10.5m-1t/eager/optional-count/wall | 6.5665 | 5.825 | 0.887 |  |
| 10.5m-1t/streaming/optional-count/wall | 6.591 | 6.583 | 0.999 |  |
| 10.5m-1t/eager/order-by-full/wall | 850.098 | 858.048 | 1.009 |  |
| 10.5m-1t/streaming/order-by-full/wall | 1039.98 | 1066.69 | 1.026 |  |
| 10.5m-1t/eager/range-topk/wall | 6.4965 | 6.45 | 0.993 |  |
| 10.5m-1t/streaming/range-topk/wall | 6.4685 | 6.4475 | 0.997 |  |
| 10.5m-1t/eager/regex-iri/wall | 38.9225 | 39.011 | 1.002 |  |
| 10.5m-1t/streaming/regex-iri/wall | 38.896 | 38.985 | 1.002 |  |
| 10.5m-1t/eager/star-join/wall | 36.0715 | 36.058 | 1.000 |  |
| 10.5m-1t/streaming/star-join/wall | 51.153 | 52.274 | 1.022 |  |
| 10.5m-1t/eager/two-hop-count/wall | 29 | 29.1715 | 1.006 |  |
| 10.5m-1t/streaming/two-hop-count/wall | 29.034 | 29.348 | 1.011 |  |
| 10.5m-12t/eager/class-predicate-list/wall | 11.3145 | 11.535 | 1.019 |  |
| 10.5m-12t/streaming/class-predicate-list/wall | 11.828 | 11.824 | 1.000 |  |
| 10.5m-12t/eager/class-predicates/wall | 11.319 | 11.4625 | 1.013 |  |
| 10.5m-12t/streaming/class-predicates/wall | 11.8305 | 11.7825 | 0.996 |  |
| 10.5m-12t/eager/contains/wall | 5.089 | 5.089 | 1.000 |  |
| 10.5m-12t/streaming/contains/wall | 5.2495 | 5.2175 | 0.994 |  |
| 10.5m-12t/eager/distinct-join/wall | 44.83 | 44.292 | 0.988 |  |
| 10.5m-12t/streaming/distinct-join/wall | 45.3265 | 43.956 | 0.970 |  |
| 10.5m-12t/eager/exists-join/wall | 10.024 | 10.096 | 1.007 |  |
| 10.5m-12t/streaming/exists-join/wall | 10.1045 | 9.9095 | 0.981 |  |
| 10.5m-12t/eager/export-500k/wall | 223.232 | 228.272 | 1.023 |  |
| 10.5m-12t/streaming/export-500k/wall | 267.032 | 289.663 | 1.085 | slower |
| 10.5m-12t/eager/expr-agg-arg/wall | 28.6455 | 25.147 | 0.878 |  |
| 10.5m-12t/streaming/expr-agg-arg/wall | 24.643 | 24.552 | 0.996 |  |
| 10.5m-12t/eager/expr-bind-group/wall | 5.6455 | 4.4955 | 0.796 |  |
| 10.5m-12t/eager/expr-order-key/wall | 11.888 | 11.87 | 0.998 |  |
| 10.5m-12t/streaming/expr-order-key/wall | 12.237 | 12.401 | 1.013 |  |
| 10.5m-12t/eager/group-avg/wall | 47.074 | 41.8035 | 0.888 |  |
| 10.5m-12t/streaming/group-avg/wall | 39.129 | 40.4835 | 1.035 |  |
| 10.5m-12t/eager/knows-reach/wall | 142.483 | 143.335 | 1.006 |  |
| 10.5m-12t/streaming/knows-reach/wall | 140.913 | 141.608 | 1.005 |  |
| 10.5m-12t/eager/minus/wall | 5.18 | 4.7395 | 0.915 |  |
| 10.5m-12t/eager/not-exists/wall | 10.7415 | 10.1305 | 0.943 |  |
| 10.5m-12t/streaming/not-exists/wall | 9.9715 | 10.1615 | 1.019 |  |
| 10.5m-12t/eager/optional-chain/wall | 133.476 | 133.347 | 0.999 |  |
| 10.5m-12t/streaming/optional-chain/wall | 162.232 | 173.888 | 1.072 | slower |
| 10.5m-12t/eager/optional-count/wall | 6.271 | 5.5085 | 0.878 |  |
| 10.5m-12t/streaming/optional-count/wall | 6.456 | 6.565 | 1.017 |  |
| 10.5m-12t/eager/order-by-full/wall | 441.635 | 469.392 | 1.063 | slower |
| 10.5m-12t/streaming/order-by-full/wall | 492.708 | 566.968 | 1.151 | slower |
| 10.5m-12t/eager/regex-iri/wall | 7.1325 | 7.135 | 1.000 |  |
| 10.5m-12t/streaming/regex-iri/wall | 7.343 | 7.341 | 1.000 |  |
| 10.5m-12t/eager/star-join/wall | 33.4955 | 33.808 | 1.009 |  |
| 10.5m-12t/streaming/star-join/wall | 42.39 | 43.3965 | 1.024 |  |
| 10.5m-12t/eager/two-hop-count/wall | 12.529 | 12.5705 | 1.003 |  |
| 10.5m-12t/streaming/two-hop-count/wall | 12.3105 | 12.5385 | 1.019 |  |

101 metrics compared, 7 flagged, 0 failed checks, 0 missing checks, 0 changed fingerprints.
