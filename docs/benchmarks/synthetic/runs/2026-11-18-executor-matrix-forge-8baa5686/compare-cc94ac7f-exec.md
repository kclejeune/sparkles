# executors gate: candidate against baseline

- baseline: host forge, commit cc94ac7f, started 2026-11-18T07:04:20Z
- candidate: host forge, commit 8baa5686, started 2026-11-18T09:27:30Z
- threshold: 5.0% slower, 0.0% below 0.3 ms

| metric | baseline | candidate | ratio | flag |
|---|--:|--:|--:|---|
| 1.05m-1t/eager/class-predicate-list/exec | 0.9277 | 0.9286 | 1.001 |  |
| 1.05m-1t/streaming/class-predicate-list/exec | 0.9164 | 0.9251 | 1.009 |  |
| 1.05m-1t/eager/class-predicates/exec | 0.9344 | 0.9376 | 1.003 |  |
| 1.05m-1t/streaming/class-predicates/exec | 0.9288 | 0.9378 | 1.010 |  |
| 1.05m-1t/eager/contains/exec | 2.5103 | 2.5478 | 1.015 |  |
| 1.05m-1t/streaming/contains/exec | 2.5137 | 2.5484 | 1.014 |  |
| 1.05m-1t/eager/count-all/exec | 0.0049 | 0.0047 | 0.959 |  |
| 1.05m-1t/streaming/count-all/exec | 0.0055 | 0.0065 | 1.182 | slower, short |
| 1.05m-1t/eager/distinct-join/exec | 3.4591 | 3.5928 | 1.039 |  |
| 1.05m-1t/streaming/distinct-join/exec | 3.7005 | 3.8374 | 1.037 |  |
| 1.05m-1t/eager/distinct-obj/exec | 0.0043 | 0.0039 | 0.907 |  |
| 1.05m-1t/streaming/distinct-obj/exec | 0.0048 | 0.0052 | 1.083 | slower, short |
| 1.05m-1t/eager/employee-docs/exec | 0.0378 | 0.0367 | 0.971 |  |
| 1.05m-1t/streaming/employee-docs/exec | 0.038 | 0.0397 | 1.045 | slower, short |
| 1.05m-1t/eager/exists-join/exec | 1.0268 | 1.0493 | 1.022 |  |
| 1.05m-1t/streaming/exists-join/exec | 1.032 | 1.0432 | 1.011 |  |
| 1.05m-1t/eager/export-500k/exec | 2.1529 | 2.0962 | 0.974 |  |
| 1.05m-1t/streaming/export-500k/exec | 0.4137 | 0.3945 | 0.954 |  |
| 1.05m-1t/eager/expr-agg-arg/exec | 2.5984 | 2.5452 | 0.980 |  |
| 1.05m-1t/streaming/expr-agg-arg/exec | 1.8951 | 1.9221 | 1.014 |  |
| 1.05m-1t/eager/expr-bind-group/exec | 0.6045 | 0.601 | 0.994 |  |
| 1.05m-1t/streaming/expr-bind-group/exec | 0.3711 | 0.3705 | 0.998 |  |
| 1.05m-1t/eager/expr-order-key/exec | 1.1463 | 1.1594 | 1.011 |  |
| 1.05m-1t/streaming/expr-order-key/exec | 1.1418 | 1.1466 | 1.004 |  |
| 1.05m-1t/eager/group-avg/exec | 3.9521 | 3.8903 | 0.984 |  |
| 1.05m-1t/streaming/group-avg/exec | 3.1703 | 3.1995 | 1.009 |  |
| 1.05m-1t/eager/knows-reach/exec | 11.514 | 11.4978 | 0.999 |  |
| 1.05m-1t/streaming/knows-reach/exec | 11.3391 | 11.3287 | 0.999 |  |
| 1.05m-1t/eager/lang-filter/exec | 1.6404 | 1.8326 | 1.117 | slower |
| 1.05m-1t/streaming/lang-filter/exec | 1.6413 | 1.8391 | 1.121 | slower |
| 1.05m-1t/eager/minus/exec | 0.3123 | 0.4119 | 1.319 | slower |
| 1.05m-1t/streaming/minus/exec | 0.2571 | 0.2617 | 1.018 | slower, short |
| 1.05m-1t/eager/not-exists/exec | 0.7998 | 0.806 | 1.008 |  |
| 1.05m-1t/streaming/not-exists/exec | 0.74 | 0.7415 | 1.002 |  |
| 1.05m-1t/eager/optional-chain/exec | 2.0023 | 2.0171 | 1.007 |  |
| 1.05m-1t/streaming/optional-chain/exec | 1.951 | 2.0606 | 1.056 | slower |
| 1.05m-1t/eager/optional-count/exec | 0.4877 | 0.4841 | 0.993 |  |
| 1.05m-1t/streaming/optional-count/exec | 0.5162 | 0.5438 | 1.053 | slower |
| 1.05m-1t/eager/order-by-full/exec | 41.1682 | 41.0957 | 0.998 |  |
| 1.05m-1t/streaming/order-by-full/exec | 41.5046 | 41.5359 | 1.001 |  |
| 1.05m-1t/eager/path-plus/exec | 0.012 | 0.0112 | 0.933 |  |
| 1.05m-1t/streaming/path-plus/exec | 0.0123 | 0.0133 | 1.081 | slower, short |
| 1.05m-1t/eager/predicate-counts/exec | 0.0052 | 0.0045 | 0.865 |  |
| 1.05m-1t/streaming/predicate-counts/exec | 0.0064 | 0.0069 | 1.078 | slower, short |
| 1.05m-1t/eager/predicate-subjects/exec | 0.0072 | 0.0068 | 0.944 |  |
| 1.05m-1t/streaming/predicate-subjects/exec | 0.0088 | 0.0093 | 1.057 | slower, short |
| 1.05m-1t/eager/range-topk/exec | 0.5352 | 0.5427 | 1.014 |  |
| 1.05m-1t/streaming/range-topk/exec | 0.5288 | 0.5369 | 1.015 |  |
| 1.05m-1t/eager/regex-iri/exec | 3.4805 | 3.8049 | 1.093 | slower |
| 1.05m-1t/streaming/regex-iri/exec | 3.4798 | 3.7996 | 1.092 | slower |
| 1.05m-1t/eager/star-join/exec | 1.4114 | 1.4137 | 1.002 |  |
| 1.05m-1t/streaming/star-join/exec | 1.4248 | 1.4395 | 1.010 |  |
| 1.05m-1t/eager/star-lookup/exec | 0.0633 | 0.0621 | 0.981 |  |
| 1.05m-1t/streaming/star-lookup/exec | 0.0641 | 0.0645 | 1.006 | slower, short |
| 1.05m-1t/eager/subquery-agg/exec | 0.1866 | 0.1862 | 0.998 |  |
| 1.05m-1t/streaming/subquery-agg/exec | 0.1859 | 0.1836 | 0.988 |  |
| 1.05m-1t/eager/two-hop-count/exec | 2.6461 | 2.6222 | 0.991 |  |
| 1.05m-1t/streaming/two-hop-count/exec | 2.6282 | 2.6159 | 0.995 |  |
| 1.05m-1t/eager/types-grouped/exec | 0.0085 | 0.0081 | 0.953 |  |
| 1.05m-1t/streaming/types-grouped/exec | 0.0128 | 0.0128 | 1.000 |  |
| 1.05m-1t/eager/values-star/exec | 0.0153 | 0.0152 | 0.993 |  |
| 1.05m-1t/streaming/values-star/exec | 0.0198 | 0.02 | 1.010 | slower, short |
| 1.05m-12t/eager/class-predicate-list/exec | 0.9796 | 0.9861 | 1.007 |  |
| 1.05m-12t/streaming/class-predicate-list/exec | 0.9774 | 1.0122 | 1.036 |  |
| 1.05m-12t/eager/class-predicates/exec | 1.0028 | 0.9958 | 0.993 |  |
| 1.05m-12t/streaming/class-predicates/exec | 0.985 | 1.0197 | 1.035 |  |
| 1.05m-12t/eager/contains/exec | 0.7304 | 0.7585 | 1.038 |  |
| 1.05m-12t/streaming/contains/exec | 0.7594 | 0.7729 | 1.018 |  |
| 1.05m-12t/eager/count-all/exec | 0.0072 | 0.008 | 1.111 | slower, short |
| 1.05m-12t/streaming/count-all/exec | 0.0092 | 0.0103 | 1.120 | slower, short |
| 1.05m-12t/eager/distinct-join/exec | 3.579 | 3.7598 | 1.051 | slower |
| 1.05m-12t/streaming/distinct-join/exec | 3.7664 | 4.015 | 1.066 | slower |
| 1.05m-12t/eager/distinct-obj/exec | 0.0066 | 0.0068 | 1.030 | slower, short |
| 1.05m-12t/streaming/distinct-obj/exec | 0.0076 | 0.0087 | 1.145 | slower, short |
| 1.05m-12t/eager/employee-docs/exec | 0.0494 | 0.0505 | 1.022 | slower, short |
| 1.05m-12t/streaming/employee-docs/exec | 0.0532 | 0.0542 | 1.019 | slower, short |
| 1.05m-12t/eager/exists-join/exec | 1.104 | 1.1391 | 1.032 |  |
| 1.05m-12t/streaming/exists-join/exec | 1.1012 | 1.1496 | 1.044 |  |
| 1.05m-12t/eager/export-500k/exec | 2.33 | 2.3889 | 1.025 |  |
| 1.05m-12t/streaming/export-500k/exec | 0.4109 | 0.3735 | 0.909 |  |
| 1.05m-12t/eager/expr-agg-arg/exec | 2.9364 | 2.9012 | 0.988 |  |
| 1.05m-12t/streaming/expr-agg-arg/exec | 2.0075 | 2.0462 | 1.019 |  |
| 1.05m-12t/eager/expr-bind-group/exec | 0.8229 | 0.7623 | 0.926 |  |
| 1.05m-12t/streaming/expr-bind-group/exec | 0.3984 | 0.3979 | 0.999 |  |
| 1.05m-12t/eager/expr-order-key/exec | 1.2338 | 1.2422 | 1.007 |  |
| 1.05m-12t/streaming/expr-order-key/exec | 1.2804 | 1.2247 | 0.956 |  |
| 1.05m-12t/eager/group-avg/exec | 4.1406 | 4.0346 | 0.974 |  |
| 1.05m-12t/streaming/group-avg/exec | 3.4011 | 3.3616 | 0.988 |  |
| 1.05m-12t/eager/knows-reach/exec | 11.7683 | 11.8109 | 1.004 |  |
| 1.05m-12t/streaming/knows-reach/exec | 11.6548 | 11.6777 | 1.002 |  |
| 1.05m-12t/eager/lang-filter/exec | 0.7539 | 0.7779 | 1.032 |  |
| 1.05m-12t/streaming/lang-filter/exec | 0.7287 | 0.8097 | 1.111 | slower |
| 1.05m-12t/eager/minus/exec | 0.3401 | 0.4813 | 1.415 | slower |
| 1.05m-12t/streaming/minus/exec | 0.2777 | 0.2805 | 1.010 | slower, short |
| 1.05m-12t/eager/not-exists/exec | 0.8565 | 0.8673 | 1.013 |  |
| 1.05m-12t/streaming/not-exists/exec | 0.7975 | 0.7928 | 0.994 |  |
| 1.05m-12t/eager/optional-chain/exec | 2.1289 | 2.1128 | 0.992 |  |
| 1.05m-12t/streaming/optional-chain/exec | 2.0969 | 2.2341 | 1.065 | slower |
| 1.05m-12t/eager/optional-count/exec | 0.5365 | 0.5478 | 1.021 |  |
| 1.05m-12t/streaming/optional-count/exec | 0.558 | 0.5751 | 1.031 |  |
| 1.05m-12t/eager/order-by-full/exec | 8.9195 | 8.7682 | 0.983 |  |
| 1.05m-12t/streaming/order-by-full/exec | 9.2472 | 9.0326 | 0.977 |  |
| 1.05m-12t/eager/path-plus/exec | 0.0185 | 0.0175 | 0.946 |  |
| 1.05m-12t/streaming/path-plus/exec | 0.02 | 0.0201 | 1.005 | slower, short |
| 1.05m-12t/eager/predicate-counts/exec | 0.0069 | 0.0074 | 1.072 | slower, short |
| 1.05m-12t/streaming/predicate-counts/exec | 0.0103 | 0.0108 | 1.049 | slower, short |
| 1.05m-12t/eager/predicate-subjects/exec | 0.009 | 0.0083 | 0.922 |  |
| 1.05m-12t/streaming/predicate-subjects/exec | 0.0152 | 0.0148 | 0.974 |  |
| 1.05m-12t/eager/range-topk/exec | 0.5715 | 0.582 | 1.018 |  |
| 1.05m-12t/streaming/range-topk/exec | 0.5668 | 0.5746 | 1.014 |  |
| 1.05m-12t/eager/regex-iri/exec | 0.9856 | 1.0236 | 1.039 |  |
| 1.05m-12t/streaming/regex-iri/exec | 0.9581 | 1.017 | 1.061 | slower |
| 1.05m-12t/eager/star-join/exec | 1.6144 | 1.5913 | 0.986 |  |
| 1.05m-12t/streaming/star-join/exec | 1.6428 | 1.6511 | 1.005 |  |
| 1.05m-12t/eager/star-lookup/exec | 0.0842 | 0.0831 | 0.987 |  |
| 1.05m-12t/streaming/star-lookup/exec | 0.094 | 0.0933 | 0.993 |  |
| 1.05m-12t/eager/subquery-agg/exec | 0.2334 | 0.2184 | 0.936 |  |
| 1.05m-12t/streaming/subquery-agg/exec | 0.2253 | 0.2094 | 0.929 |  |
| 1.05m-12t/eager/two-hop-count/exec | 1.2139 | 1.2316 | 1.015 |  |
| 1.05m-12t/streaming/two-hop-count/exec | 1.2527 | 1.2287 | 0.981 |  |
| 1.05m-12t/eager/types-grouped/exec | 0.0118 | 0.0122 | 1.034 | slower, short |
| 1.05m-12t/streaming/types-grouped/exec | 0.0185 | 0.0209 | 1.130 | slower, short |
| 1.05m-12t/eager/values-star/exec | 0.0192 | 0.021 | 1.094 | slower, short |
| 1.05m-12t/streaming/values-star/exec | 0.0277 | 0.0313 | 1.130 | slower, short |
| 10.5m-1t/eager/class-predicate-list/exec | 9.8738 | 10.1028 | 1.023 |  |
| 10.5m-1t/streaming/class-predicate-list/exec | 10.2686 | 10.3444 | 1.007 |  |
| 10.5m-1t/eager/class-predicates/exec | 9.8502 | 10.0737 | 1.023 |  |
| 10.5m-1t/streaming/class-predicates/exec | 10.2741 | 10.335 | 1.006 |  |
| 10.5m-1t/eager/contains/exec | 24.4183 | 24.6489 | 1.009 |  |
| 10.5m-1t/streaming/contains/exec | 24.4599 | 24.6929 | 1.010 |  |
| 10.5m-1t/eager/count-all/exec | 0.0061 | 0.0063 | 1.033 | slower, short |
| 10.5m-1t/streaming/count-all/exec | 0.0071 | 0.0076 | 1.070 | slower, short |
| 10.5m-1t/eager/distinct-join/exec | 38.4891 | 39.1584 | 1.017 |  |
| 10.5m-1t/streaming/distinct-join/exec | 37.4851 | 38.2143 | 1.019 |  |
| 10.5m-1t/eager/distinct-obj/exec | 0.0047 | 0.0043 | 0.915 |  |
| 10.5m-1t/streaming/distinct-obj/exec | 0.0053 | 0.0054 | 1.019 | slower, short |
| 10.5m-1t/eager/employee-docs/exec | 0.0493 | 0.05 | 1.014 | slower, short |
| 10.5m-1t/streaming/employee-docs/exec | 0.0552 | 0.0538 | 0.975 |  |
| 10.5m-1t/eager/exists-join/exec | 8.4838 | 8.6015 | 1.014 |  |
| 10.5m-1t/streaming/exists-join/exec | 8.4718 | 8.6288 | 1.019 |  |
| 10.5m-1t/eager/export-500k/exec | 2.2054 | 2.3207 | 1.052 | slower |
| 10.5m-1t/streaming/export-500k/exec | 0.4301 | 0.3997 | 0.929 |  |
| 10.5m-1t/eager/expr-agg-arg/exec | 29.2226 | 28.5302 | 0.976 |  |
| 10.5m-1t/streaming/expr-agg-arg/exec | 22.7784 | 23.1268 | 1.015 |  |
| 10.5m-1t/eager/expr-bind-group/exec | 5.7527 | 5.7654 | 1.002 |  |
| 10.5m-1t/streaming/expr-bind-group/exec | 3.298 | 3.2952 | 0.999 |  |
| 10.5m-1t/eager/expr-order-key/exec | 10.9126 | 11.0564 | 1.013 |  |
| 10.5m-1t/streaming/expr-order-key/exec | 10.7785 | 10.6506 | 0.988 |  |
| 10.5m-1t/eager/group-avg/exec | 45.314 | 45.2064 | 0.998 |  |
| 10.5m-1t/streaming/group-avg/exec | 36.706 | 37.2281 | 1.014 |  |
| 10.5m-1t/eager/knows-reach/exec | 139.616 | 140.011 | 1.003 |  |
| 10.5m-1t/streaming/knows-reach/exec | 136.913 | 137.307 | 1.003 |  |
| 10.5m-1t/eager/lang-filter/exec | 16.0053 | 18.0412 | 1.127 | slower |
| 10.5m-1t/streaming/lang-filter/exec | 15.9799 | 18.0744 | 1.131 | slower |
| 10.5m-1t/eager/minus/exec | 3.2511 | 4.3002 | 1.323 | slower |
| 10.5m-1t/streaming/minus/exec | 2.4409 | 2.4674 | 1.011 |  |
| 10.5m-1t/eager/not-exists/exec | 9.4401 | 9.477 | 1.004 |  |
| 10.5m-1t/streaming/not-exists/exec | 8.6438 | 8.737 | 1.011 |  |
| 10.5m-1t/eager/optional-chain/exec | 27.6928 | 26.9698 | 0.974 |  |
| 10.5m-1t/streaming/optional-chain/exec | 22.5187 | 25.3125 | 1.124 | slower |
| 10.5m-1t/eager/optional-count/exec | 5.3185 | 5.3051 | 0.997 |  |
| 10.5m-1t/streaming/optional-count/exec | 4.9994 | 5.2789 | 1.056 | slower |
| 10.5m-1t/eager/order-by-full/exec | 514.724 | 517.2 | 1.005 |  |
| 10.5m-1t/streaming/order-by-full/exec | 517.212 | 519.819 | 1.005 |  |
| 10.5m-1t/eager/path-plus/exec | 0.0095 | 0.0089 | 0.937 |  |
| 10.5m-1t/streaming/path-plus/exec | 0.0094 | 0.0094 | 1.000 |  |
| 10.5m-1t/eager/predicate-counts/exec | 0.0054 | 0.0046 | 0.852 |  |
| 10.5m-1t/streaming/predicate-counts/exec | 0.0066 | 0.0069 | 1.045 | slower, short |
| 10.5m-1t/eager/predicate-subjects/exec | 0.0075 | 0.0069 | 0.920 |  |
| 10.5m-1t/streaming/predicate-subjects/exec | 0.0087 | 0.009 | 1.034 | slower, short |
| 10.5m-1t/eager/range-topk/exec | 5.0799 | 5.1719 | 1.018 |  |
| 10.5m-1t/streaming/range-topk/exec | 5.0418 | 5.0924 | 1.010 |  |
| 10.5m-1t/eager/regex-iri/exec | 34.4965 | 38.1641 | 1.106 | slower |
| 10.5m-1t/streaming/regex-iri/exec | 34.5189 | 38.1572 | 1.105 | slower |
| 10.5m-1t/eager/star-join/exec | 14.067 | 14.1484 | 1.006 |  |
| 10.5m-1t/streaming/star-join/exec | 14.8004 | 14.8431 | 1.003 |  |
| 10.5m-1t/eager/star-lookup/exec | 0.0574 | 0.0577 | 1.005 | slower, short |
| 10.5m-1t/streaming/star-lookup/exec | 0.0611 | 0.0583 | 0.954 |  |
| 10.5m-1t/eager/subquery-agg/exec | 1.6985 | 1.6937 | 0.997 |  |
| 10.5m-1t/streaming/subquery-agg/exec | 1.6515 | 1.6263 | 0.985 |  |
| 10.5m-1t/eager/two-hop-count/exec | 27.7387 | 27.585 | 0.994 |  |
| 10.5m-1t/streaming/two-hop-count/exec | 27.8409 | 27.5679 | 0.990 |  |
| 10.5m-1t/eager/types-grouped/exec | 0.0089 | 0.0083 | 0.933 |  |
| 10.5m-1t/streaming/types-grouped/exec | 0.0127 | 0.0128 | 1.008 | slower, short |
| 10.5m-1t/eager/values-star/exec | 0.0161 | 0.0162 | 1.006 | slower, short |
| 10.5m-1t/streaming/values-star/exec | 0.0207 | 0.0198 | 0.957 |  |
| 10.5m-12t/eager/class-predicate-list/exec | 10.213 | 10.4838 | 1.027 |  |
| 10.5m-12t/streaming/class-predicate-list/exec | 10.5404 | 10.6001 | 1.006 |  |
| 10.5m-12t/eager/class-predicates/exec | 10.1471 | 10.2841 | 1.014 |  |
| 10.5m-12t/streaming/class-predicates/exec | 10.5623 | 10.7224 | 1.015 |  |
| 10.5m-12t/eager/contains/exec | 4.2087 | 4.2557 | 1.011 |  |
| 10.5m-12t/streaming/contains/exec | 4.1981 | 4.2746 | 1.018 |  |
| 10.5m-12t/eager/count-all/exec | 0.0091 | 0.0086 | 0.945 |  |
| 10.5m-12t/streaming/count-all/exec | 0.0107 | 0.0114 | 1.065 | slower, short |
| 10.5m-12t/eager/distinct-join/exec | 39.015 | 39.2794 | 1.007 |  |
| 10.5m-12t/streaming/distinct-join/exec | 37.7993 | 39.0919 | 1.034 |  |
| 10.5m-12t/eager/distinct-obj/exec | 0.0063 | 0.0067 | 1.063 | slower, short |
| 10.5m-12t/streaming/distinct-obj/exec | 0.0087 | 0.0095 | 1.092 | slower, short |
| 10.5m-12t/eager/employee-docs/exec | 0.0674 | 0.0686 | 1.018 | slower, short |
| 10.5m-12t/streaming/employee-docs/exec | 0.0804 | 0.0766 | 0.953 |  |
| 10.5m-12t/eager/exists-join/exec | 8.9689 | 8.9501 | 0.998 |  |
| 10.5m-12t/streaming/exists-join/exec | 8.8335 | 8.9148 | 1.009 |  |
| 10.5m-12t/eager/export-500k/exec | 2.5833 | 2.198 | 0.851 |  |
| 10.5m-12t/streaming/export-500k/exec | 0.4231 | 0.3828 | 0.905 |  |
| 10.5m-12t/eager/expr-agg-arg/exec | 28.7974 | 27.6744 | 0.961 |  |
| 10.5m-12t/streaming/expr-agg-arg/exec | 23.193 | 23.4144 | 1.010 |  |
| 10.5m-12t/eager/expr-bind-group/exec | 4.8261 | 4.7512 | 0.984 |  |
| 10.5m-12t/streaming/expr-bind-group/exec | 3.4278 | 3.413 | 0.996 |  |
| 10.5m-12t/eager/expr-order-key/exec | 10.1057 | 10.1028 | 1.000 |  |
| 10.5m-12t/streaming/expr-order-key/exec | 11.1112 | 10.9592 | 0.986 |  |
| 10.5m-12t/eager/group-avg/exec | 46.1542 | 45.4409 | 0.985 |  |
| 10.5m-12t/streaming/group-avg/exec | 37.5777 | 37.7191 | 1.004 |  |
| 10.5m-12t/eager/knows-reach/exec | 143.563 | 141.111 | 0.983 |  |
| 10.5m-12t/streaming/knows-reach/exec | 140.487 | 139.26 | 0.991 |  |
| 10.5m-12t/eager/lang-filter/exec | 2.9065 | 3.1237 | 1.075 | slower |
| 10.5m-12t/streaming/lang-filter/exec | 2.9165 | 3.0565 | 1.048 |  |
| 10.5m-12t/eager/minus/exec | 3.3023 | 4.3026 | 1.303 | slower |
| 10.5m-12t/streaming/minus/exec | 2.571 | 2.5863 | 1.006 |  |
| 10.5m-12t/eager/not-exists/exec | 10.1632 | 9.755 | 0.960 |  |
| 10.5m-12t/streaming/not-exists/exec | 9.0994 | 8.9493 | 0.984 |  |
| 10.5m-12t/eager/optional-chain/exec | 45.2343 | 25.1457 | 0.556 |  |
| 10.5m-12t/streaming/optional-chain/exec | 25.4781 | 26.5077 | 1.040 |  |
| 10.5m-12t/eager/optional-count/exec | 5.4636 | 5.4023 | 0.989 |  |
| 10.5m-12t/streaming/optional-count/exec | 5.1799 | 5.4524 | 1.053 | slower |
| 10.5m-12t/eager/order-by-full/exec | 187.553 | 108.713 | 0.580 |  |
| 10.5m-12t/streaming/order-by-full/exec | 191.771 | 107.363 | 0.560 |  |
| 10.5m-12t/eager/path-plus/exec | 0.0153 | 0.0125 | 0.817 |  |
| 10.5m-12t/streaming/path-plus/exec | 0.0149 | 0.0153 | 1.027 | slower, short |
| 10.5m-12t/eager/predicate-counts/exec | 0.0076 | 0.0067 | 0.882 |  |
| 10.5m-12t/streaming/predicate-counts/exec | 0.0111 | 0.0109 | 0.982 |  |
| 10.5m-12t/eager/predicate-subjects/exec | 0.0091 | 0.0091 | 1.000 |  |
| 10.5m-12t/streaming/predicate-subjects/exec | 0.0145 | 0.0167 | 1.152 | slower, short |
| 10.5m-12t/eager/range-topk/exec | 2.1753 | 2.1106 | 0.970 |  |
| 10.5m-12t/streaming/range-topk/exec | 2.2206 | 2.0712 | 0.933 |  |
| 10.5m-12t/eager/regex-iri/exec | 6.2021 | 6.4991 | 1.048 |  |
| 10.5m-12t/streaming/regex-iri/exec | 6.2201 | 6.503 | 1.045 |  |
| 10.5m-12t/eager/star-join/exec | 13.7188 | 13.3085 | 0.970 |  |
| 10.5m-12t/streaming/star-join/exec | 14.0274 | 13.9195 | 0.992 |  |
| 10.5m-12t/eager/star-lookup/exec | 0.0733 | 0.0734 | 1.001 | slower, short |
| 10.5m-12t/streaming/star-lookup/exec | 0.0806 | 0.0842 | 1.045 | slower, short |
| 10.5m-12t/eager/subquery-agg/exec | 0.8063 | 0.8224 | 1.020 |  |
| 10.5m-12t/streaming/subquery-agg/exec | 0.8356 | 0.7732 | 0.925 |  |
| 10.5m-12t/eager/two-hop-count/exec | 11.3232 | 11.3129 | 0.999 |  |
| 10.5m-12t/streaming/two-hop-count/exec | 11.2561 | 11.1765 | 0.993 |  |
| 10.5m-12t/eager/types-grouped/exec | 0.0115 | 0.0122 | 1.061 | slower, short |
| 10.5m-12t/streaming/types-grouped/exec | 0.0196 | 0.0201 | 1.026 | slower, short |
| 10.5m-12t/eager/values-star/exec | 0.0206 | 0.0207 | 1.005 | slower, short |
| 10.5m-12t/streaming/values-star/exec | 0.0325 | 0.0275 | 0.846 |  |

248 metrics compared, 67 flagged, 0 failed checks, 0 missing checks, 0 changed fingerprints.
