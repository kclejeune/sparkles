# executors gate: candidate against baseline

- baseline: host forge, commit cc94ac7f, started 2026-11-18T07:04:20Z
- candidate: host forge, commit 8baa5686, started 2026-11-18T09:27:30Z
- threshold: 5.0% slower, floor 5.0, 0.0% below 0.3 ms

| metric | baseline | candidate | ratio | flag |
|---|--:|--:|--:|---|
| 1.05m-1t/eager/distinct-join/wall | 5.032 | 5.2435 | 1.042 |  |
| 1.05m-1t/streaming/distinct-join/wall | 5.975 | 5.978 | 1.001 |  |
| 1.05m-1t/eager/export-500k/wall | 250.03 | 233.596 | 0.934 |  |
| 1.05m-1t/streaming/export-500k/wall | 338.984 | 356.007 | 1.050 | slower |
| 1.05m-1t/eager/group-avg/wall | 5.32 | 5.2 | 0.977 |  |
| 1.05m-1t/eager/knows-reach/wall | 12.8 | 12.76 | 0.997 |  |
| 1.05m-1t/streaming/knows-reach/wall | 12.6595 | 12.6365 | 0.998 |  |
| 1.05m-1t/eager/optional-chain/wall | 15.129 | 15.0345 | 0.994 |  |
| 1.05m-1t/streaming/optional-chain/wall | 21.0465 | 22.0365 | 1.047 |  |
| 1.05m-1t/eager/order-by-full/wall | 77.2985 | 76.2135 | 0.986 |  |
| 1.05m-1t/streaming/order-by-full/wall | 95.005 | 97.2165 | 1.023 |  |
| 1.05m-1t/eager/star-join/wall | 5.0695 | 5.044 | 0.995 |  |
| 1.05m-1t/streaming/star-join/wall | 6.111 | 7.007 | 1.147 | slower |
| 1.05m-12t/streaming/distinct-join/wall | 5.688 | 5.625 | 0.989 |  |
| 1.05m-12t/eager/export-500k/wall | 236.463 | 220.362 | 0.932 |  |
| 1.05m-12t/streaming/export-500k/wall | 270.646 | 287.297 | 1.062 | slower |
| 1.05m-12t/eager/group-avg/wall | 5.027 | 4.902 | 0.975 |  |
| 1.05m-12t/eager/knows-reach/wall | 12.7965 | 12.8205 | 1.002 |  |
| 1.05m-12t/streaming/knows-reach/wall | 12.763 | 12.7565 | 0.999 |  |
| 1.05m-12t/eager/optional-chain/wall | 14.561 | 14.2445 | 0.978 |  |
| 1.05m-12t/streaming/optional-chain/wall | 17.7105 | 18.6065 | 1.051 | slower |
| 1.05m-12t/eager/order-by-full/wall | 43.3055 | 41.501 | 0.958 |  |
| 1.05m-12t/streaming/order-by-full/wall | 48.442 | 51.578 | 1.065 | slower |
| 1.05m-12t/eager/star-join/wall | 5.02 | 4.9405 | 0.984 |  |
| 1.05m-12t/streaming/star-join/wall | 5.7385 | 5.9765 | 1.041 |  |
| 10.5m-1t/eager/class-predicate-list/wall | 11.252 | 11.451 | 1.018 |  |
| 10.5m-1t/streaming/class-predicate-list/wall | 11.6015 | 11.693 | 1.008 |  |
| 10.5m-1t/eager/class-predicates/wall | 11.2435 | 11.452 | 1.019 |  |
| 10.5m-1t/streaming/class-predicates/wall | 11.6575 | 11.7015 | 1.004 |  |
| 10.5m-1t/eager/contains/wall | 25.625 | 25.8865 | 1.010 |  |
| 10.5m-1t/streaming/contains/wall | 25.7005 | 25.951 | 1.010 |  |
| 10.5m-1t/eager/distinct-join/wall | 43.7165 | 44.4235 | 1.016 |  |
| 10.5m-1t/streaming/distinct-join/wall | 44.9425 | 45.9425 | 1.022 |  |
| 10.5m-1t/eager/exists-join/wall | 9.8705 | 9.9835 | 1.011 |  |
| 10.5m-1t/streaming/exists-join/wall | 9.8725 | 10.03 | 1.016 |  |
| 10.5m-1t/eager/export-500k/wall | 256.046 | 239.001 | 0.933 |  |
| 10.5m-1t/streaming/export-500k/wall | 336.033 | 356.016 | 1.059 | slower |
| 10.5m-1t/eager/expr-agg-arg/wall | 30.6045 | 29.938 | 0.978 |  |
| 10.5m-1t/streaming/expr-agg-arg/wall | 23.7425 | 24.521 | 1.033 |  |
| 10.5m-1t/eager/expr-bind-group/wall | 6.98 | 6.9785 | 1.000 |  |
| 10.5m-1t/eager/expr-order-key/wall | 12.907 | 13.0495 | 1.011 |  |
| 10.5m-1t/streaming/expr-order-key/wall | 12.301 | 12.2085 | 0.992 |  |
| 10.5m-1t/eager/group-avg/wall | 46.93 | 46.736 | 0.996 |  |
| 10.5m-1t/streaming/group-avg/wall | 38.327 | 38.8055 | 1.012 |  |
| 10.5m-1t/eager/knows-reach/wall | 140.929 | 141.146 | 1.002 |  |
| 10.5m-1t/streaming/knows-reach/wall | 138.226 | 138.632 | 1.003 |  |
| 10.5m-1t/eager/lang-filter/wall | 17.261 | 19.155 | 1.110 | slower |
| 10.5m-1t/streaming/lang-filter/wall | 17.263 | 19.2635 | 1.116 | slower |
| 10.5m-1t/eager/not-exists/wall | 10.807 | 10.759 | 0.996 |  |
| 10.5m-1t/streaming/not-exists/wall | 9.956 | 9.969 | 1.001 |  |
| 10.5m-1t/eager/optional-chain/wall | 142.571 | 137.047 | 0.961 |  |
| 10.5m-1t/streaming/optional-chain/wall | 207.165 | 220.019 | 1.062 | slower |
| 10.5m-1t/eager/optional-count/wall | 6.627 | 6.6505 | 1.004 |  |
| 10.5m-1t/streaming/optional-count/wall | 6.2765 | 6.5655 | 1.046 |  |
| 10.5m-1t/eager/order-by-full/wall | 865.074 | 858.015 | 0.992 |  |
| 10.5m-1t/streaming/order-by-full/wall | 1029.99 | 1061.58 | 1.031 |  |
| 10.5m-1t/eager/range-topk/wall | 6.423 | 6.501 | 1.012 |  |
| 10.5m-1t/streaming/range-topk/wall | 6.43 | 6.47 | 1.006 |  |
| 10.5m-1t/eager/regex-iri/wall | 35.7255 | 39.166 | 1.096 | slower |
| 10.5m-1t/streaming/regex-iri/wall | 35.728 | 39.28 | 1.099 | slower |
| 10.5m-1t/eager/star-join/wall | 37.048 | 36.7625 | 0.992 |  |
| 10.5m-1t/streaming/star-join/wall | 51.1085 | 52.107 | 1.020 |  |
| 10.5m-1t/eager/two-hop-count/wall | 29.109 | 28.929 | 0.994 |  |
| 10.5m-1t/streaming/two-hop-count/wall | 29.316 | 28.966 | 0.988 |  |
| 10.5m-12t/eager/class-predicate-list/wall | 11.285 | 11.551 | 1.024 |  |
| 10.5m-12t/streaming/class-predicate-list/wall | 11.634 | 11.724 | 1.008 |  |
| 10.5m-12t/eager/class-predicates/wall | 11.2615 | 11.4015 | 1.012 |  |
| 10.5m-12t/streaming/class-predicates/wall | 11.7405 | 11.899 | 1.014 |  |
| 10.5m-12t/eager/contains/wall | 5.011 | 5.0685 | 1.011 |  |
| 10.5m-12t/streaming/contains/wall | 5.1625 | 5.305 | 1.028 |  |
| 10.5m-12t/eager/distinct-join/wall | 44.702 | 44.7885 | 1.002 |  |
| 10.5m-12t/streaming/distinct-join/wall | 43.7235 | 45.1855 | 1.033 |  |
| 10.5m-12t/eager/exists-join/wall | 9.9305 | 9.917 | 0.999 |  |
| 10.5m-12t/streaming/exists-join/wall | 10.01 | 10.0695 | 1.006 |  |
| 10.5m-12t/eager/export-500k/wall | 242.538 | 225.286 | 0.929 |  |
| 10.5m-12t/streaming/export-500k/wall | 266.629 | 283.745 | 1.064 | slower |
| 10.5m-12t/eager/expr-agg-arg/wall | 29.963 | 28.8175 | 0.962 |  |
| 10.5m-12t/streaming/expr-agg-arg/wall | 24.4145 | 24.5445 | 1.005 |  |
| 10.5m-12t/eager/expr-bind-group/wall | 5.678 | 5.585 | 0.984 |  |
| 10.5m-12t/eager/expr-order-key/wall | 11.899 | 11.8655 | 0.997 |  |
| 10.5m-12t/streaming/expr-order-key/wall | 12.267 | 12.134 | 0.989 |  |
| 10.5m-12t/eager/group-avg/wall | 47.573 | 46.8625 | 0.985 |  |
| 10.5m-12t/streaming/group-avg/wall | 38.805 | 39.0515 | 1.006 |  |
| 10.5m-12t/eager/knows-reach/wall | 144.935 | 142.429 | 0.983 |  |
| 10.5m-12t/streaming/knows-reach/wall | 141.739 | 140.56 | 0.992 |  |
| 10.5m-12t/eager/not-exists/wall | 11.244 | 10.643 | 0.947 |  |
| 10.5m-12t/streaming/not-exists/wall | 10.097 | 9.965 | 0.987 |  |
| 10.5m-12t/eager/optional-chain/wall | 155.953 | 131.81 | 0.845 |  |
| 10.5m-12t/streaming/optional-chain/wall | 161.172 | 169.871 | 1.054 | slower |
| 10.5m-12t/eager/optional-count/wall | 6.3535 | 6.286 | 0.989 |  |
| 10.5m-12t/streaming/optional-count/wall | 6.2435 | 6.481 | 1.038 |  |
| 10.5m-12t/eager/order-by-full/wall | 528.413 | 434.529 | 0.822 |  |
| 10.5m-12t/streaming/order-by-full/wall | 555.395 | 501.717 | 0.903 |  |
| 10.5m-12t/eager/regex-iri/wall | 7.008 | 7.296 | 1.041 |  |
| 10.5m-12t/streaming/regex-iri/wall | 7.217 | 7.5325 | 1.044 |  |
| 10.5m-12t/eager/star-join/wall | 34.5345 | 33.717 | 0.976 |  |
| 10.5m-12t/streaming/star-join/wall | 42.5645 | 43.7645 | 1.028 |  |
| 10.5m-12t/eager/two-hop-count/wall | 12.497 | 12.532 | 1.003 |  |
| 10.5m-12t/streaming/two-hop-count/wall | 12.439 | 12.346 | 0.993 |  |

99 metrics compared, 13 flagged, 0 failed checks, 0 missing checks, 0 changed fingerprints.
