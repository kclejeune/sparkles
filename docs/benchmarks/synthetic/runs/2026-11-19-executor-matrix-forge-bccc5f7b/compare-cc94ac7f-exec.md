# executors gate: candidate against baseline

- baseline: host forge, commit cc94ac7f, started 2026-11-18T07:04:20Z
- candidate: host forge, commit bccc5f7b, started 2026-11-19T00:08:03Z
- threshold: 5.0% slower, 0.0% below 0.3 ms

| metric | baseline | candidate | ratio | flag |
|---|--:|--:|--:|---|
| 1.05m-1t/eager/class-predicate-list/exec | 0.9277 | 0.9425 | 1.016 |  |
| 1.05m-1t/streaming/class-predicate-list/exec | 0.9164 | 0.9335 | 1.019 |  |
| 1.05m-1t/eager/class-predicates/exec | 0.9344 | 0.9542 | 1.021 |  |
| 1.05m-1t/streaming/class-predicates/exec | 0.9288 | 0.945 | 1.017 |  |
| 1.05m-1t/eager/contains/exec | 2.5103 | 2.5341 | 1.009 |  |
| 1.05m-1t/streaming/contains/exec | 2.5137 | 2.5357 | 1.009 |  |
| 1.05m-1t/eager/count-all/exec | 0.0049 | 0.0051 | 1.041 | slower, short |
| 1.05m-1t/streaming/count-all/exec | 0.0055 | 0.0062 | 1.127 | slower, short |
| 1.05m-1t/eager/distinct-join/exec | 3.4591 | 3.556 | 1.028 |  |
| 1.05m-1t/streaming/distinct-join/exec | 3.7005 | 3.7412 | 1.011 |  |
| 1.05m-1t/eager/distinct-obj/exec | 0.0043 | 0.0042 | 0.977 |  |
| 1.05m-1t/streaming/distinct-obj/exec | 0.0048 | 0.0052 | 1.083 | slower, short |
| 1.05m-1t/eager/employee-docs/exec | 0.0378 | 0.0379 | 1.003 | slower, short |
| 1.05m-1t/streaming/employee-docs/exec | 0.038 | 0.0397 | 1.045 | slower, short |
| 1.05m-1t/eager/exists-join/exec | 1.0268 | 1.0598 | 1.032 |  |
| 1.05m-1t/streaming/exists-join/exec | 1.032 | 1.038 | 1.006 |  |
| 1.05m-1t/eager/export-500k/exec | 2.1529 | 2.1283 | 0.989 |  |
| 1.05m-1t/streaming/export-500k/exec | 0.4137 | 0.4029 | 0.974 |  |
| 1.05m-1t/eager/expr-agg-arg/exec | 2.5984 | 2.2869 | 0.880 |  |
| 1.05m-1t/streaming/expr-agg-arg/exec | 1.8951 | 1.9303 | 1.019 |  |
| 1.05m-1t/eager/expr-bind-group/exec | 0.6045 | 0.5071 | 0.839 |  |
| 1.05m-1t/streaming/expr-bind-group/exec | 0.3711 | 0.3637 | 0.980 |  |
| 1.05m-1t/eager/expr-order-key/exec | 1.1463 | 1.1427 | 0.997 |  |
| 1.05m-1t/streaming/expr-order-key/exec | 1.1418 | 1.1415 | 1.000 |  |
| 1.05m-1t/eager/group-avg/exec | 3.9521 | 3.5797 | 0.906 |  |
| 1.05m-1t/streaming/group-avg/exec | 3.1703 | 3.1865 | 1.005 |  |
| 1.05m-1t/eager/knows-reach/exec | 11.514 | 11.339 | 0.985 |  |
| 1.05m-1t/streaming/knows-reach/exec | 11.3391 | 11.3938 | 1.005 |  |
| 1.05m-1t/eager/lang-filter/exec | 1.6404 | 1.7853 | 1.088 | slower |
| 1.05m-1t/streaming/lang-filter/exec | 1.6413 | 1.7853 | 1.088 | slower |
| 1.05m-1t/eager/minus/exec | 0.3123 | 0.3652 | 1.169 | slower |
| 1.05m-1t/streaming/minus/exec | 0.2571 | 0.263 | 1.023 | slower, short |
| 1.05m-1t/eager/not-exists/exec | 0.7998 | 0.7642 | 0.955 |  |
| 1.05m-1t/streaming/not-exists/exec | 0.74 | 0.7418 | 1.002 |  |
| 1.05m-1t/eager/optional-chain/exec | 2.0023 | 2.0081 | 1.003 |  |
| 1.05m-1t/streaming/optional-chain/exec | 1.951 | 2.0606 | 1.056 | slower |
| 1.05m-1t/eager/optional-count/exec | 0.4877 | 0.4164 | 0.854 |  |
| 1.05m-1t/streaming/optional-count/exec | 0.5162 | 0.5412 | 1.048 |  |
| 1.05m-1t/eager/order-by-full/exec | 41.1682 | 41.7813 | 1.015 |  |
| 1.05m-1t/streaming/order-by-full/exec | 41.5046 | 41.5443 | 1.001 |  |
| 1.05m-1t/eager/path-plus/exec | 0.012 | 0.0128 | 1.067 | slower, short |
| 1.05m-1t/streaming/path-plus/exec | 0.0123 | 0.0128 | 1.041 | slower, short |
| 1.05m-1t/eager/predicate-counts/exec | 0.0052 | 0.0052 | 1.000 |  |
| 1.05m-1t/streaming/predicate-counts/exec | 0.0064 | 0.0069 | 1.078 | slower, short |
| 1.05m-1t/eager/predicate-subjects/exec | 0.0072 | 0.0071 | 0.986 |  |
| 1.05m-1t/streaming/predicate-subjects/exec | 0.0088 | 0.0089 | 1.011 | slower, short |
| 1.05m-1t/eager/range-topk/exec | 0.5352 | 0.5417 | 1.012 |  |
| 1.05m-1t/streaming/range-topk/exec | 0.5288 | 0.535 | 1.012 |  |
| 1.05m-1t/eager/regex-iri/exec | 3.4805 | 3.7539 | 1.079 | slower |
| 1.05m-1t/streaming/regex-iri/exec | 3.4798 | 3.7667 | 1.082 | slower |
| 1.05m-1t/eager/star-join/exec | 1.4114 | 1.3985 | 0.991 |  |
| 1.05m-1t/streaming/star-join/exec | 1.4248 | 1.4166 | 0.994 |  |
| 1.05m-1t/eager/star-lookup/exec | 0.0633 | 0.0617 | 0.975 |  |
| 1.05m-1t/streaming/star-lookup/exec | 0.0641 | 0.0626 | 0.977 |  |
| 1.05m-1t/eager/subquery-agg/exec | 0.1866 | 0.1851 | 0.992 |  |
| 1.05m-1t/streaming/subquery-agg/exec | 0.1859 | 0.1855 | 0.998 |  |
| 1.05m-1t/eager/two-hop-count/exec | 2.6461 | 2.6354 | 0.996 |  |
| 1.05m-1t/streaming/two-hop-count/exec | 2.6282 | 2.6254 | 0.999 |  |
| 1.05m-1t/eager/types-grouped/exec | 0.0085 | 0.008 | 0.941 |  |
| 1.05m-1t/streaming/types-grouped/exec | 0.0128 | 0.013 | 1.016 | slower, short |
| 1.05m-1t/eager/values-star/exec | 0.0153 | 0.0161 | 1.052 | slower, short |
| 1.05m-1t/streaming/values-star/exec | 0.0198 | 0.0195 | 0.985 |  |
| 1.05m-12t/eager/class-predicate-list/exec | 0.9796 | 0.9889 | 1.009 |  |
| 1.05m-12t/streaming/class-predicate-list/exec | 0.9774 | 1.0285 | 1.052 | slower |
| 1.05m-12t/eager/class-predicates/exec | 1.0028 | 0.9972 | 0.994 |  |
| 1.05m-12t/streaming/class-predicates/exec | 0.985 | 1.0521 | 1.068 | slower |
| 1.05m-12t/eager/contains/exec | 0.7304 | 0.7315 | 1.002 |  |
| 1.05m-12t/streaming/contains/exec | 0.7594 | 0.7625 | 1.004 |  |
| 1.05m-12t/eager/count-all/exec | 0.0072 | 0.0066 | 0.917 |  |
| 1.05m-12t/streaming/count-all/exec | 0.0092 | 0.01 | 1.087 | slower, short |
| 1.05m-12t/eager/distinct-join/exec | 3.579 | 3.6951 | 1.032 |  |
| 1.05m-12t/streaming/distinct-join/exec | 3.7664 | 3.9186 | 1.040 |  |
| 1.05m-12t/eager/distinct-obj/exec | 0.0066 | 0.0062 | 0.939 |  |
| 1.05m-12t/streaming/distinct-obj/exec | 0.0076 | 0.0087 | 1.145 | slower, short |
| 1.05m-12t/eager/employee-docs/exec | 0.0494 | 0.0505 | 1.022 | slower, short |
| 1.05m-12t/streaming/employee-docs/exec | 0.0532 | 0.0561 | 1.055 | slower, short |
| 1.05m-12t/eager/exists-join/exec | 1.104 | 1.1449 | 1.037 |  |
| 1.05m-12t/streaming/exists-join/exec | 1.1012 | 1.1417 | 1.037 |  |
| 1.05m-12t/eager/export-500k/exec | 2.33 | 2.3765 | 1.020 |  |
| 1.05m-12t/streaming/export-500k/exec | 0.4109 | 0.3764 | 0.916 |  |
| 1.05m-12t/eager/expr-agg-arg/exec | 2.9364 | 2.6789 | 0.912 |  |
| 1.05m-12t/streaming/expr-agg-arg/exec | 2.0075 | 2.0678 | 1.030 |  |
| 1.05m-12t/eager/expr-bind-group/exec | 0.8229 | 0.6675 | 0.811 |  |
| 1.05m-12t/streaming/expr-bind-group/exec | 0.3984 | 0.3951 | 0.992 |  |
| 1.05m-12t/eager/expr-order-key/exec | 1.2338 | 1.2528 | 1.015 |  |
| 1.05m-12t/streaming/expr-order-key/exec | 1.2804 | 1.2707 | 0.992 |  |
| 1.05m-12t/eager/group-avg/exec | 4.1406 | 3.8866 | 0.939 |  |
| 1.05m-12t/streaming/group-avg/exec | 3.4011 | 3.4177 | 1.005 |  |
| 1.05m-12t/eager/knows-reach/exec | 11.7683 | 11.6403 | 0.989 |  |
| 1.05m-12t/streaming/knows-reach/exec | 11.6548 | 11.7048 | 1.004 |  |
| 1.05m-12t/eager/lang-filter/exec | 0.7539 | 0.7557 | 1.002 |  |
| 1.05m-12t/streaming/lang-filter/exec | 0.7287 | 0.8162 | 1.120 | slower |
| 1.05m-12t/eager/minus/exec | 0.3401 | 0.4354 | 1.280 | slower |
| 1.05m-12t/streaming/minus/exec | 0.2777 | 0.2865 | 1.032 | slower, short |
| 1.05m-12t/eager/not-exists/exec | 0.8565 | 0.8329 | 0.972 |  |
| 1.05m-12t/streaming/not-exists/exec | 0.7975 | 0.8133 | 1.020 |  |
| 1.05m-12t/eager/optional-chain/exec | 2.1289 | 2.1405 | 1.005 |  |
| 1.05m-12t/streaming/optional-chain/exec | 2.0969 | 2.1728 | 1.036 |  |
| 1.05m-12t/eager/optional-count/exec | 0.5365 | 0.4689 | 0.874 |  |
| 1.05m-12t/streaming/optional-count/exec | 0.558 | 0.6016 | 1.078 | slower |
| 1.05m-12t/eager/order-by-full/exec | 8.9195 | 9.4152 | 1.056 | slower |
| 1.05m-12t/streaming/order-by-full/exec | 9.2472 | 9.6813 | 1.047 |  |
| 1.05m-12t/eager/path-plus/exec | 0.0185 | 0.0198 | 1.070 | slower, short |
| 1.05m-12t/streaming/path-plus/exec | 0.02 | 0.0189 | 0.945 |  |
| 1.05m-12t/eager/predicate-counts/exec | 0.0069 | 0.0072 | 1.043 | slower, short |
| 1.05m-12t/streaming/predicate-counts/exec | 0.0103 | 0.0111 | 1.078 | slower, short |
| 1.05m-12t/eager/predicate-subjects/exec | 0.009 | 0.0102 | 1.133 | slower, short |
| 1.05m-12t/streaming/predicate-subjects/exec | 0.0152 | 0.0161 | 1.059 | slower, short |
| 1.05m-12t/eager/range-topk/exec | 0.5715 | 0.5704 | 0.998 |  |
| 1.05m-12t/streaming/range-topk/exec | 0.5668 | 0.5791 | 1.022 |  |
| 1.05m-12t/eager/regex-iri/exec | 0.9856 | 0.9741 | 0.988 |  |
| 1.05m-12t/streaming/regex-iri/exec | 0.9581 | 1.025 | 1.070 | slower |
| 1.05m-12t/eager/star-join/exec | 1.6144 | 1.5399 | 0.954 |  |
| 1.05m-12t/streaming/star-join/exec | 1.6428 | 1.6374 | 0.997 |  |
| 1.05m-12t/eager/star-lookup/exec | 0.0842 | 0.0869 | 1.032 | slower, short |
| 1.05m-12t/streaming/star-lookup/exec | 0.094 | 0.0918 | 0.977 |  |
| 1.05m-12t/eager/subquery-agg/exec | 0.2334 | 0.2412 | 1.033 | slower, short |
| 1.05m-12t/streaming/subquery-agg/exec | 0.2253 | 0.231 | 1.025 | slower, short |
| 1.05m-12t/eager/two-hop-count/exec | 1.2139 | 1.242 | 1.023 |  |
| 1.05m-12t/streaming/two-hop-count/exec | 1.2527 | 1.2518 | 0.999 |  |
| 1.05m-12t/eager/types-grouped/exec | 0.0118 | 0.0113 | 0.958 |  |
| 1.05m-12t/streaming/types-grouped/exec | 0.0185 | 0.0208 | 1.124 | slower, short |
| 1.05m-12t/eager/values-star/exec | 0.0192 | 0.0204 | 1.063 | slower, short |
| 1.05m-12t/streaming/values-star/exec | 0.0277 | 0.029 | 1.047 | slower, short |
| 10.5m-1t/eager/class-predicate-list/exec | 9.8738 | 10.0343 | 1.016 |  |
| 10.5m-1t/streaming/class-predicate-list/exec | 10.2686 | 10.436 | 1.016 |  |
| 10.5m-1t/eager/class-predicates/exec | 9.8502 | 10.1323 | 1.029 |  |
| 10.5m-1t/streaming/class-predicates/exec | 10.2741 | 10.4014 | 1.012 |  |
| 10.5m-1t/eager/contains/exec | 24.4183 | 24.5739 | 1.006 |  |
| 10.5m-1t/streaming/contains/exec | 24.4599 | 24.593 | 1.005 |  |
| 10.5m-1t/eager/count-all/exec | 0.0061 | 0.0059 | 0.967 |  |
| 10.5m-1t/streaming/count-all/exec | 0.0071 | 0.0074 | 1.042 | slower, short |
| 10.5m-1t/eager/distinct-join/exec | 38.4891 | 38.4555 | 0.999 |  |
| 10.5m-1t/streaming/distinct-join/exec | 37.4851 | 37.5512 | 1.002 |  |
| 10.5m-1t/eager/distinct-obj/exec | 0.0047 | 0.0042 | 0.894 |  |
| 10.5m-1t/streaming/distinct-obj/exec | 0.0053 | 0.0055 | 1.038 | slower, short |
| 10.5m-1t/eager/employee-docs/exec | 0.0493 | 0.0494 | 1.002 | slower, short |
| 10.5m-1t/streaming/employee-docs/exec | 0.0552 | 0.0539 | 0.976 |  |
| 10.5m-1t/eager/exists-join/exec | 8.4838 | 8.5598 | 1.009 |  |
| 10.5m-1t/streaming/exists-join/exec | 8.4718 | 8.576 | 1.012 |  |
| 10.5m-1t/eager/export-500k/exec | 2.2054 | 2.3891 | 1.083 | slower |
| 10.5m-1t/streaming/export-500k/exec | 0.4301 | 0.4176 | 0.971 |  |
| 10.5m-1t/eager/expr-agg-arg/exec | 29.2226 | 25.2713 | 0.865 |  |
| 10.5m-1t/streaming/expr-agg-arg/exec | 22.7784 | 22.7009 | 0.997 |  |
| 10.5m-1t/eager/expr-bind-group/exec | 5.7527 | 4.7988 | 0.834 |  |
| 10.5m-1t/streaming/expr-bind-group/exec | 3.298 | 3.2089 | 0.973 |  |
| 10.5m-1t/eager/expr-order-key/exec | 10.9126 | 11.0278 | 1.011 |  |
| 10.5m-1t/streaming/expr-order-key/exec | 10.7785 | 10.9184 | 1.013 |  |
| 10.5m-1t/eager/group-avg/exec | 45.314 | 39.4715 | 0.871 |  |
| 10.5m-1t/streaming/group-avg/exec | 36.706 | 37.9805 | 1.035 |  |
| 10.5m-1t/eager/knows-reach/exec | 139.616 | 136.81 | 0.980 |  |
| 10.5m-1t/streaming/knows-reach/exec | 136.913 | 137.127 | 1.002 |  |
| 10.5m-1t/eager/lang-filter/exec | 16.0053 | 17.5737 | 1.098 | slower |
| 10.5m-1t/streaming/lang-filter/exec | 15.9799 | 17.6311 | 1.103 | slower |
| 10.5m-1t/eager/minus/exec | 3.2511 | 3.7737 | 1.161 | slower |
| 10.5m-1t/streaming/minus/exec | 2.4409 | 2.4634 | 1.009 |  |
| 10.5m-1t/eager/not-exists/exec | 9.4401 | 8.9848 | 0.952 |  |
| 10.5m-1t/streaming/not-exists/exec | 8.6438 | 8.6244 | 0.998 |  |
| 10.5m-1t/eager/optional-chain/exec | 27.6928 | 26.2702 | 0.949 |  |
| 10.5m-1t/streaming/optional-chain/exec | 22.5187 | 25.1218 | 1.116 | slower |
| 10.5m-1t/eager/optional-count/exec | 5.3185 | 4.5102 | 0.848 |  |
| 10.5m-1t/streaming/optional-count/exec | 4.9994 | 5.3028 | 1.061 | slower |
| 10.5m-1t/eager/order-by-full/exec | 514.724 | 518.709 | 1.008 |  |
| 10.5m-1t/streaming/order-by-full/exec | 517.212 | 520.744 | 1.007 |  |
| 10.5m-1t/eager/path-plus/exec | 0.0095 | 0.0102 | 1.074 | slower, short |
| 10.5m-1t/streaming/path-plus/exec | 0.0094 | 0.0098 | 1.043 | slower, short |
| 10.5m-1t/eager/predicate-counts/exec | 0.0054 | 0.0048 | 0.889 |  |
| 10.5m-1t/streaming/predicate-counts/exec | 0.0066 | 0.0072 | 1.091 | slower, short |
| 10.5m-1t/eager/predicate-subjects/exec | 0.0075 | 0.007 | 0.933 |  |
| 10.5m-1t/streaming/predicate-subjects/exec | 0.0087 | 0.0094 | 1.080 | slower, short |
| 10.5m-1t/eager/range-topk/exec | 5.0799 | 5.1241 | 1.009 |  |
| 10.5m-1t/streaming/range-topk/exec | 5.0418 | 5.0589 | 1.003 |  |
| 10.5m-1t/eager/regex-iri/exec | 34.4965 | 37.7425 | 1.094 | slower |
| 10.5m-1t/streaming/regex-iri/exec | 34.5189 | 37.7833 | 1.095 | slower |
| 10.5m-1t/eager/star-join/exec | 14.067 | 13.9379 | 0.991 |  |
| 10.5m-1t/streaming/star-join/exec | 14.8004 | 14.6403 | 0.989 |  |
| 10.5m-1t/eager/star-lookup/exec | 0.0574 | 0.0582 | 1.014 | slower, short |
| 10.5m-1t/streaming/star-lookup/exec | 0.0611 | 0.0609 | 0.997 |  |
| 10.5m-1t/eager/subquery-agg/exec | 1.6985 | 1.6993 | 1.000 |  |
| 10.5m-1t/streaming/subquery-agg/exec | 1.6515 | 1.6437 | 0.995 |  |
| 10.5m-1t/eager/two-hop-count/exec | 27.7387 | 27.8177 | 1.003 |  |
| 10.5m-1t/streaming/two-hop-count/exec | 27.8409 | 27.8699 | 1.001 |  |
| 10.5m-1t/eager/types-grouped/exec | 0.0089 | 0.0082 | 0.921 |  |
| 10.5m-1t/streaming/types-grouped/exec | 0.0127 | 0.0131 | 1.031 | slower, short |
| 10.5m-1t/eager/values-star/exec | 0.0161 | 0.0169 | 1.050 | slower, short |
| 10.5m-1t/streaming/values-star/exec | 0.0207 | 0.0214 | 1.034 | slower, short |
| 10.5m-12t/eager/class-predicate-list/exec | 10.213 | 10.4325 | 1.021 |  |
| 10.5m-12t/streaming/class-predicate-list/exec | 10.5404 | 10.6903 | 1.014 |  |
| 10.5m-12t/eager/class-predicates/exec | 10.1471 | 10.3289 | 1.018 |  |
| 10.5m-12t/streaming/class-predicates/exec | 10.5623 | 10.678 | 1.011 |  |
| 10.5m-12t/eager/contains/exec | 4.2087 | 4.2908 | 1.020 |  |
| 10.5m-12t/streaming/contains/exec | 4.1981 | 4.263 | 1.015 |  |
| 10.5m-12t/eager/count-all/exec | 0.0091 | 0.0089 | 0.978 |  |
| 10.5m-12t/streaming/count-all/exec | 0.0107 | 0.0114 | 1.065 | slower, short |
| 10.5m-12t/eager/distinct-join/exec | 39.015 | 38.8905 | 0.997 |  |
| 10.5m-12t/streaming/distinct-join/exec | 37.7993 | 38.0947 | 1.008 |  |
| 10.5m-12t/eager/distinct-obj/exec | 0.0063 | 0.0069 | 1.095 | slower, short |
| 10.5m-12t/streaming/distinct-obj/exec | 0.0087 | 0.009 | 1.034 | slower, short |
| 10.5m-12t/eager/employee-docs/exec | 0.0674 | 0.0686 | 1.018 | slower, short |
| 10.5m-12t/streaming/employee-docs/exec | 0.0804 | 0.0793 | 0.986 |  |
| 10.5m-12t/eager/exists-join/exec | 8.9689 | 9.0845 | 1.013 |  |
| 10.5m-12t/streaming/exists-join/exec | 8.8335 | 8.7666 | 0.992 |  |
| 10.5m-12t/eager/export-500k/exec | 2.5833 | 2.3064 | 0.893 |  |
| 10.5m-12t/streaming/export-500k/exec | 0.4231 | 0.3829 | 0.905 |  |
| 10.5m-12t/eager/expr-agg-arg/exec | 28.7974 | 23.9869 | 0.833 |  |
| 10.5m-12t/streaming/expr-agg-arg/exec | 23.193 | 23.3457 | 1.007 |  |
| 10.5m-12t/eager/expr-bind-group/exec | 4.8261 | 3.6836 | 0.763 |  |
| 10.5m-12t/streaming/expr-bind-group/exec | 3.4278 | 3.379 | 0.986 |  |
| 10.5m-12t/eager/expr-order-key/exec | 10.1057 | 10.0788 | 0.997 |  |
| 10.5m-12t/streaming/expr-order-key/exec | 11.1112 | 11.2747 | 1.015 |  |
| 10.5m-12t/eager/group-avg/exec | 46.1542 | 40.3942 | 0.875 |  |
| 10.5m-12t/streaming/group-avg/exec | 37.5777 | 39.0808 | 1.040 |  |
| 10.5m-12t/eager/knows-reach/exec | 143.563 | 141.859 | 0.988 |  |
| 10.5m-12t/streaming/knows-reach/exec | 140.487 | 140.383 | 0.999 |  |
| 10.5m-12t/eager/lang-filter/exec | 2.9065 | 3.047 | 1.048 |  |
| 10.5m-12t/streaming/lang-filter/exec | 2.9165 | 3.0595 | 1.049 |  |
| 10.5m-12t/eager/minus/exec | 3.3023 | 3.9065 | 1.183 | slower |
| 10.5m-12t/streaming/minus/exec | 2.571 | 2.6218 | 1.020 |  |
| 10.5m-12t/eager/not-exists/exec | 10.1632 | 9.183 | 0.904 |  |
| 10.5m-12t/streaming/not-exists/exec | 9.0994 | 9.1442 | 1.005 |  |
| 10.5m-12t/eager/optional-chain/exec | 45.2343 | 27.5346 | 0.609 |  |
| 10.5m-12t/streaming/optional-chain/exec | 25.4781 | 27.6282 | 1.084 | slower |
| 10.5m-12t/eager/optional-count/exec | 5.4636 | 4.6455 | 0.850 |  |
| 10.5m-12t/streaming/optional-count/exec | 5.1799 | 5.5205 | 1.066 | slower |
| 10.5m-12t/eager/order-by-full/exec | 187.553 | 146.426 | 0.781 |  |
| 10.5m-12t/streaming/order-by-full/exec | 191.771 | 166.012 | 0.866 |  |
| 10.5m-12t/eager/path-plus/exec | 0.0153 | 0.0164 | 1.072 | slower, short |
| 10.5m-12t/streaming/path-plus/exec | 0.0149 | 0.0146 | 0.980 |  |
| 10.5m-12t/eager/predicate-counts/exec | 0.0076 | 0.008 | 1.053 | slower, short |
| 10.5m-12t/streaming/predicate-counts/exec | 0.0111 | 0.0111 | 1.000 |  |
| 10.5m-12t/eager/predicate-subjects/exec | 0.0091 | 0.0109 | 1.198 | slower, short |
| 10.5m-12t/streaming/predicate-subjects/exec | 0.0145 | 0.0154 | 1.062 | slower, short |
| 10.5m-12t/eager/range-topk/exec | 2.1753 | 2.0935 | 0.962 |  |
| 10.5m-12t/streaming/range-topk/exec | 2.2206 | 2.277 | 1.025 |  |
| 10.5m-12t/eager/regex-iri/exec | 6.2021 | 6.3188 | 1.019 |  |
| 10.5m-12t/streaming/regex-iri/exec | 6.2201 | 6.3301 | 1.018 |  |
| 10.5m-12t/eager/star-join/exec | 13.7188 | 13.2456 | 0.966 |  |
| 10.5m-12t/streaming/star-join/exec | 14.0274 | 13.7189 | 0.978 |  |
| 10.5m-12t/eager/star-lookup/exec | 0.0733 | 0.0723 | 0.986 |  |
| 10.5m-12t/streaming/star-lookup/exec | 0.0806 | 0.0811 | 1.006 | slower, short |
| 10.5m-12t/eager/subquery-agg/exec | 0.8063 | 0.783 | 0.971 |  |
| 10.5m-12t/streaming/subquery-agg/exec | 0.8356 | 0.7995 | 0.957 |  |
| 10.5m-12t/eager/two-hop-count/exec | 11.3232 | 11.3824 | 1.005 |  |
| 10.5m-12t/streaming/two-hop-count/exec | 11.2561 | 11.3629 | 1.009 |  |
| 10.5m-12t/eager/types-grouped/exec | 0.0115 | 0.0126 | 1.096 | slower, short |
| 10.5m-12t/streaming/types-grouped/exec | 0.0196 | 0.0203 | 1.036 | slower, short |
| 10.5m-12t/eager/values-star/exec | 0.0206 | 0.0208 | 1.010 | slower, short |
| 10.5m-12t/streaming/values-star/exec | 0.0325 | 0.0276 | 0.849 |  |

248 metrics compared, 75 flagged, 0 failed checks, 0 missing checks, 0 changed fingerprints.
