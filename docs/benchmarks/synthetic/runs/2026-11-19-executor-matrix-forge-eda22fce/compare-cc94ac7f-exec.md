# executors gate: candidate against baseline

- baseline: host forge, commit cc94ac7f, started 2026-11-18T07:04:20Z
- candidate: host forge, commit eda22fce, started 2026-11-19T02:31:44Z
- threshold: 5.0% slower, 0.0% below 0.3 ms

| metric | baseline | candidate | ratio | flag |
|---|--:|--:|--:|---|
| 1.05m-1t/eager/class-predicate-list/exec | 0.9277 | 0.9351 | 1.008 |  |
| 1.05m-1t/streaming/class-predicate-list/exec | 0.9164 | 0.9282 | 1.013 |  |
| 1.05m-1t/eager/class-predicates/exec | 0.9344 | 0.9398 | 1.006 |  |
| 1.05m-1t/streaming/class-predicates/exec | 0.9288 | 0.9366 | 1.008 |  |
| 1.05m-1t/eager/contains/exec | 2.5103 | 2.5511 | 1.016 |  |
| 1.05m-1t/streaming/contains/exec | 2.5137 | 2.5533 | 1.016 |  |
| 1.05m-1t/eager/count-all/exec | 0.0049 | 0.0051 | 1.041 | slower, short |
| 1.05m-1t/streaming/count-all/exec | 0.0055 | 0.006 | 1.091 | slower, short |
| 1.05m-1t/eager/distinct-join/exec | 3.4591 | 3.637 | 1.051 | slower |
| 1.05m-1t/streaming/distinct-join/exec | 3.7005 | 3.8231 | 1.033 |  |
| 1.05m-1t/eager/distinct-obj/exec | 0.0043 | 0.0042 | 0.977 |  |
| 1.05m-1t/streaming/distinct-obj/exec | 0.0048 | 0.0052 | 1.083 | slower, short |
| 1.05m-1t/eager/employee-docs/exec | 0.0378 | 0.0379 | 1.003 | slower, short |
| 1.05m-1t/streaming/employee-docs/exec | 0.038 | 0.0416 | 1.095 | slower, short |
| 1.05m-1t/eager/exists-join/exec | 1.0268 | 1.0705 | 1.043 |  |
| 1.05m-1t/streaming/exists-join/exec | 1.032 | 1.0442 | 1.012 |  |
| 1.05m-1t/eager/export-500k/exec | 2.1529 | 2.3049 | 1.071 | slower |
| 1.05m-1t/streaming/export-500k/exec | 0.4137 | 0.3933 | 0.951 |  |
| 1.05m-1t/eager/expr-agg-arg/exec | 2.5984 | 2.5585 | 0.985 |  |
| 1.05m-1t/streaming/expr-agg-arg/exec | 1.8951 | 1.9207 | 1.014 |  |
| 1.05m-1t/eager/expr-bind-group/exec | 0.6045 | 0.6017 | 0.995 |  |
| 1.05m-1t/streaming/expr-bind-group/exec | 0.3711 | 0.3723 | 1.003 |  |
| 1.05m-1t/eager/expr-order-key/exec | 1.1463 | 1.1538 | 1.007 |  |
| 1.05m-1t/streaming/expr-order-key/exec | 1.1418 | 1.1389 | 0.997 |  |
| 1.05m-1t/eager/group-avg/exec | 3.9521 | 3.8431 | 0.972 |  |
| 1.05m-1t/streaming/group-avg/exec | 3.1703 | 3.1943 | 1.008 |  |
| 1.05m-1t/eager/knows-reach/exec | 11.514 | 11.5181 | 1.000 |  |
| 1.05m-1t/streaming/knows-reach/exec | 11.3391 | 11.2934 | 0.996 |  |
| 1.05m-1t/eager/lang-filter/exec | 1.6404 | 1.8666 | 1.138 | slower |
| 1.05m-1t/streaming/lang-filter/exec | 1.6413 | 1.8642 | 1.136 | slower |
| 1.05m-1t/eager/minus/exec | 0.3123 | 0.412 | 1.319 | slower |
| 1.05m-1t/streaming/minus/exec | 0.2571 | 0.2613 | 1.016 | slower, short |
| 1.05m-1t/eager/not-exists/exec | 0.7998 | 0.8045 | 1.006 |  |
| 1.05m-1t/streaming/not-exists/exec | 0.74 | 0.7419 | 1.003 |  |
| 1.05m-1t/eager/optional-chain/exec | 2.0023 | 2.0203 | 1.009 |  |
| 1.05m-1t/streaming/optional-chain/exec | 1.951 | 2.0655 | 1.059 | slower |
| 1.05m-1t/eager/optional-count/exec | 0.4877 | 0.4837 | 0.992 |  |
| 1.05m-1t/streaming/optional-count/exec | 0.5162 | 0.5416 | 1.049 |  |
| 1.05m-1t/eager/order-by-full/exec | 41.1682 | 41.2255 | 1.001 |  |
| 1.05m-1t/streaming/order-by-full/exec | 41.5046 | 41.5499 | 1.001 |  |
| 1.05m-1t/eager/path-plus/exec | 0.012 | 0.0118 | 0.983 |  |
| 1.05m-1t/streaming/path-plus/exec | 0.0123 | 0.0129 | 1.049 | slower, short |
| 1.05m-1t/eager/predicate-counts/exec | 0.0052 | 0.005 | 0.962 |  |
| 1.05m-1t/streaming/predicate-counts/exec | 0.0064 | 0.0072 | 1.125 | slower, short |
| 1.05m-1t/eager/predicate-subjects/exec | 0.0072 | 0.0076 | 1.056 | slower, short |
| 1.05m-1t/streaming/predicate-subjects/exec | 0.0088 | 0.0093 | 1.057 | slower, short |
| 1.05m-1t/eager/range-topk/exec | 0.5352 | 0.5426 | 1.014 |  |
| 1.05m-1t/streaming/range-topk/exec | 0.5288 | 0.5366 | 1.015 |  |
| 1.05m-1t/eager/regex-iri/exec | 3.4805 | 3.7465 | 1.076 | slower |
| 1.05m-1t/streaming/regex-iri/exec | 3.4798 | 3.7252 | 1.071 | slower |
| 1.05m-1t/eager/star-join/exec | 1.4114 | 1.4143 | 1.002 |  |
| 1.05m-1t/streaming/star-join/exec | 1.4248 | 1.4315 | 1.005 |  |
| 1.05m-1t/eager/star-lookup/exec | 0.0633 | 0.0633 | 1.000 |  |
| 1.05m-1t/streaming/star-lookup/exec | 0.0641 | 0.0665 | 1.037 | slower, short |
| 1.05m-1t/eager/subquery-agg/exec | 0.1866 | 0.1873 | 1.004 | slower, short |
| 1.05m-1t/streaming/subquery-agg/exec | 0.1859 | 0.1857 | 0.999 |  |
| 1.05m-1t/eager/two-hop-count/exec | 2.6461 | 2.6324 | 0.995 |  |
| 1.05m-1t/streaming/two-hop-count/exec | 2.6282 | 2.6261 | 0.999 |  |
| 1.05m-1t/eager/types-grouped/exec | 0.0085 | 0.0081 | 0.953 |  |
| 1.05m-1t/streaming/types-grouped/exec | 0.0128 | 0.0133 | 1.039 | slower, short |
| 1.05m-1t/eager/values-star/exec | 0.0153 | 0.0171 | 1.118 | slower, short |
| 1.05m-1t/streaming/values-star/exec | 0.0198 | 0.021 | 1.061 | slower, short |
| 1.05m-12t/eager/class-predicate-list/exec | 0.9796 | 0.9923 | 1.013 |  |
| 1.05m-12t/streaming/class-predicate-list/exec | 0.9774 | 0.9911 | 1.014 |  |
| 1.05m-12t/eager/class-predicates/exec | 1.0028 | 1.0059 | 1.003 |  |
| 1.05m-12t/streaming/class-predicates/exec | 0.985 | 1.0061 | 1.021 |  |
| 1.05m-12t/eager/contains/exec | 0.7304 | 0.7354 | 1.007 |  |
| 1.05m-12t/streaming/contains/exec | 0.7594 | 0.7417 | 0.977 |  |
| 1.05m-12t/eager/count-all/exec | 0.0072 | 0.0067 | 0.931 |  |
| 1.05m-12t/streaming/count-all/exec | 0.0092 | 0.0105 | 1.141 | slower, short |
| 1.05m-12t/eager/distinct-join/exec | 3.579 | 3.8617 | 1.079 | slower |
| 1.05m-12t/streaming/distinct-join/exec | 3.7664 | 3.9699 | 1.054 | slower |
| 1.05m-12t/eager/distinct-obj/exec | 0.0066 | 0.006 | 0.909 |  |
| 1.05m-12t/streaming/distinct-obj/exec | 0.0076 | 0.0113 | 1.487 | slower, short |
| 1.05m-12t/eager/employee-docs/exec | 0.0494 | 0.0518 | 1.049 | slower, short |
| 1.05m-12t/streaming/employee-docs/exec | 0.0532 | 0.055 | 1.034 | slower, short |
| 1.05m-12t/eager/exists-join/exec | 1.104 | 1.1625 | 1.053 | slower |
| 1.05m-12t/streaming/exists-join/exec | 1.1012 | 1.1414 | 1.037 |  |
| 1.05m-12t/eager/export-500k/exec | 2.33 | 2.3447 | 1.006 |  |
| 1.05m-12t/streaming/export-500k/exec | 0.4109 | 0.3765 | 0.916 |  |
| 1.05m-12t/eager/expr-agg-arg/exec | 2.9364 | 2.8954 | 0.986 |  |
| 1.05m-12t/streaming/expr-agg-arg/exec | 2.0075 | 2.0282 | 1.010 |  |
| 1.05m-12t/eager/expr-bind-group/exec | 0.8229 | 0.8281 | 1.006 |  |
| 1.05m-12t/streaming/expr-bind-group/exec | 0.3984 | 0.3917 | 0.983 |  |
| 1.05m-12t/eager/expr-order-key/exec | 1.2338 | 1.2549 | 1.017 |  |
| 1.05m-12t/streaming/expr-order-key/exec | 1.2804 | 1.2376 | 0.967 |  |
| 1.05m-12t/eager/group-avg/exec | 4.1406 | 4.0508 | 0.978 |  |
| 1.05m-12t/streaming/group-avg/exec | 3.4011 | 3.3589 | 0.988 |  |
| 1.05m-12t/eager/knows-reach/exec | 11.7683 | 11.7945 | 1.002 |  |
| 1.05m-12t/streaming/knows-reach/exec | 11.6548 | 11.5128 | 0.988 |  |
| 1.05m-12t/eager/lang-filter/exec | 0.7539 | 0.8024 | 1.064 | slower |
| 1.05m-12t/streaming/lang-filter/exec | 0.7287 | 0.7663 | 1.052 | slower |
| 1.05m-12t/eager/minus/exec | 0.3401 | 0.4577 | 1.346 | slower |
| 1.05m-12t/streaming/minus/exec | 0.2777 | 0.2853 | 1.027 | slower, short |
| 1.05m-12t/eager/not-exists/exec | 0.8565 | 0.8687 | 1.014 |  |
| 1.05m-12t/streaming/not-exists/exec | 0.7975 | 0.7965 | 0.999 |  |
| 1.05m-12t/eager/optional-chain/exec | 2.1289 | 2.1283 | 1.000 |  |
| 1.05m-12t/streaming/optional-chain/exec | 2.0969 | 2.1998 | 1.049 |  |
| 1.05m-12t/eager/optional-count/exec | 0.5365 | 0.575 | 1.072 | slower |
| 1.05m-12t/streaming/optional-count/exec | 0.558 | 0.5797 | 1.039 |  |
| 1.05m-12t/eager/order-by-full/exec | 8.9195 | 8.7521 | 0.981 |  |
| 1.05m-12t/streaming/order-by-full/exec | 9.2472 | 9.1738 | 0.992 |  |
| 1.05m-12t/eager/path-plus/exec | 0.0185 | 0.0183 | 0.989 |  |
| 1.05m-12t/streaming/path-plus/exec | 0.02 | 0.0208 | 1.040 | slower, short |
| 1.05m-12t/eager/predicate-counts/exec | 0.0069 | 0.0069 | 1.000 |  |
| 1.05m-12t/streaming/predicate-counts/exec | 0.0103 | 0.0107 | 1.039 | slower, short |
| 1.05m-12t/eager/predicate-subjects/exec | 0.009 | 0.0102 | 1.133 | slower, short |
| 1.05m-12t/streaming/predicate-subjects/exec | 0.0152 | 0.0155 | 1.020 | slower, short |
| 1.05m-12t/eager/range-topk/exec | 0.5715 | 0.5892 | 1.031 |  |
| 1.05m-12t/streaming/range-topk/exec | 0.5668 | 0.5799 | 1.023 |  |
| 1.05m-12t/eager/regex-iri/exec | 0.9856 | 1.0558 | 1.071 | slower |
| 1.05m-12t/streaming/regex-iri/exec | 0.9581 | 1.0866 | 1.134 | slower |
| 1.05m-12t/eager/star-join/exec | 1.6144 | 1.6431 | 1.018 |  |
| 1.05m-12t/streaming/star-join/exec | 1.6428 | 1.66 | 1.010 |  |
| 1.05m-12t/eager/star-lookup/exec | 0.0842 | 0.0862 | 1.024 | slower, short |
| 1.05m-12t/streaming/star-lookup/exec | 0.094 | 0.089 | 0.947 |  |
| 1.05m-12t/eager/subquery-agg/exec | 0.2334 | 0.2505 | 1.073 | slower, short |
| 1.05m-12t/streaming/subquery-agg/exec | 0.2253 | 0.2221 | 0.986 |  |
| 1.05m-12t/eager/two-hop-count/exec | 1.2139 | 1.2374 | 1.019 |  |
| 1.05m-12t/streaming/two-hop-count/exec | 1.2527 | 1.2279 | 0.980 |  |
| 1.05m-12t/eager/types-grouped/exec | 0.0118 | 0.0119 | 1.008 | slower, short |
| 1.05m-12t/streaming/types-grouped/exec | 0.0185 | 0.0189 | 1.022 | slower, short |
| 1.05m-12t/eager/values-star/exec | 0.0192 | 0.0213 | 1.109 | slower, short |
| 1.05m-12t/streaming/values-star/exec | 0.0277 | 0.027 | 0.975 |  |
| 10.5m-1t/eager/class-predicate-list/exec | 9.8738 | 10.0573 | 1.019 |  |
| 10.5m-1t/streaming/class-predicate-list/exec | 10.2686 | 10.345 | 1.007 |  |
| 10.5m-1t/eager/class-predicates/exec | 9.8502 | 9.9658 | 1.012 |  |
| 10.5m-1t/streaming/class-predicates/exec | 10.2741 | 10.3754 | 1.010 |  |
| 10.5m-1t/eager/contains/exec | 24.4183 | 24.7219 | 1.012 |  |
| 10.5m-1t/streaming/contains/exec | 24.4599 | 24.6633 | 1.008 |  |
| 10.5m-1t/eager/count-all/exec | 0.0061 | 0.0059 | 0.967 |  |
| 10.5m-1t/streaming/count-all/exec | 0.0071 | 0.007 | 0.986 |  |
| 10.5m-1t/eager/distinct-join/exec | 38.4891 | 39.112 | 1.016 |  |
| 10.5m-1t/streaming/distinct-join/exec | 37.4851 | 38.223 | 1.020 |  |
| 10.5m-1t/eager/distinct-obj/exec | 0.0047 | 0.0045 | 0.957 |  |
| 10.5m-1t/streaming/distinct-obj/exec | 0.0053 | 0.0052 | 0.981 |  |
| 10.5m-1t/eager/employee-docs/exec | 0.0493 | 0.0495 | 1.004 | slower, short |
| 10.5m-1t/streaming/employee-docs/exec | 0.0552 | 0.0533 | 0.966 |  |
| 10.5m-1t/eager/exists-join/exec | 8.4838 | 8.5973 | 1.013 |  |
| 10.5m-1t/streaming/exists-join/exec | 8.4718 | 8.588 | 1.014 |  |
| 10.5m-1t/eager/export-500k/exec | 2.2054 | 2.182 | 0.989 |  |
| 10.5m-1t/streaming/export-500k/exec | 0.4301 | 0.4036 | 0.938 |  |
| 10.5m-1t/eager/expr-agg-arg/exec | 29.2226 | 28.0454 | 0.960 |  |
| 10.5m-1t/streaming/expr-agg-arg/exec | 22.7784 | 23.0515 | 1.012 |  |
| 10.5m-1t/eager/expr-bind-group/exec | 5.7527 | 5.6352 | 0.980 |  |
| 10.5m-1t/streaming/expr-bind-group/exec | 3.298 | 3.2927 | 0.998 |  |
| 10.5m-1t/eager/expr-order-key/exec | 10.9126 | 11.057 | 1.013 |  |
| 10.5m-1t/streaming/expr-order-key/exec | 10.7785 | 10.7384 | 0.996 |  |
| 10.5m-1t/eager/group-avg/exec | 45.314 | 44.5917 | 0.984 |  |
| 10.5m-1t/streaming/group-avg/exec | 36.706 | 36.9266 | 1.006 |  |
| 10.5m-1t/eager/knows-reach/exec | 139.616 | 139.016 | 0.996 |  |
| 10.5m-1t/streaming/knows-reach/exec | 136.913 | 136.442 | 0.997 |  |
| 10.5m-1t/eager/lang-filter/exec | 16.0053 | 18.3818 | 1.148 | slower |
| 10.5m-1t/streaming/lang-filter/exec | 15.9799 | 18.456 | 1.155 | slower |
| 10.5m-1t/eager/minus/exec | 3.2511 | 4.2318 | 1.302 | slower |
| 10.5m-1t/streaming/minus/exec | 2.4409 | 2.4642 | 1.010 |  |
| 10.5m-1t/eager/not-exists/exec | 9.4401 | 9.4508 | 1.001 |  |
| 10.5m-1t/streaming/not-exists/exec | 8.6438 | 8.7239 | 1.009 |  |
| 10.5m-1t/eager/optional-chain/exec | 27.6928 | 26.7278 | 0.965 |  |
| 10.5m-1t/streaming/optional-chain/exec | 22.5187 | 24.8735 | 1.105 | slower |
| 10.5m-1t/eager/optional-count/exec | 5.3185 | 5.2754 | 0.992 |  |
| 10.5m-1t/streaming/optional-count/exec | 4.9994 | 5.275 | 1.055 | slower |
| 10.5m-1t/eager/order-by-full/exec | 514.724 | 514.678 | 1.000 |  |
| 10.5m-1t/streaming/order-by-full/exec | 517.212 | 518.713 | 1.003 |  |
| 10.5m-1t/eager/path-plus/exec | 0.0095 | 0.0088 | 0.926 |  |
| 10.5m-1t/streaming/path-plus/exec | 0.0094 | 0.0093 | 0.989 |  |
| 10.5m-1t/eager/predicate-counts/exec | 0.0054 | 0.0052 | 0.963 |  |
| 10.5m-1t/streaming/predicate-counts/exec | 0.0066 | 0.0064 | 0.970 |  |
| 10.5m-1t/eager/predicate-subjects/exec | 0.0075 | 0.007 | 0.933 |  |
| 10.5m-1t/streaming/predicate-subjects/exec | 0.0087 | 0.0087 | 1.000 |  |
| 10.5m-1t/eager/range-topk/exec | 5.0799 | 5.1506 | 1.014 |  |
| 10.5m-1t/streaming/range-topk/exec | 5.0418 | 5.0912 | 1.010 |  |
| 10.5m-1t/eager/regex-iri/exec | 34.4965 | 37.6599 | 1.092 | slower |
| 10.5m-1t/streaming/regex-iri/exec | 34.5189 | 37.6355 | 1.090 | slower |
| 10.5m-1t/eager/star-join/exec | 14.067 | 14.0741 | 1.001 |  |
| 10.5m-1t/streaming/star-join/exec | 14.8004 | 14.8748 | 1.005 |  |
| 10.5m-1t/eager/star-lookup/exec | 0.0574 | 0.058 | 1.010 | slower, short |
| 10.5m-1t/streaming/star-lookup/exec | 0.0611 | 0.0607 | 0.993 |  |
| 10.5m-1t/eager/subquery-agg/exec | 1.6985 | 1.697 | 0.999 |  |
| 10.5m-1t/streaming/subquery-agg/exec | 1.6515 | 1.6321 | 0.988 |  |
| 10.5m-1t/eager/two-hop-count/exec | 27.7387 | 27.6557 | 0.997 |  |
| 10.5m-1t/streaming/two-hop-count/exec | 27.8409 | 27.6555 | 0.993 |  |
| 10.5m-1t/eager/types-grouped/exec | 0.0089 | 0.0081 | 0.910 |  |
| 10.5m-1t/streaming/types-grouped/exec | 0.0127 | 0.0128 | 1.008 | slower, short |
| 10.5m-1t/eager/values-star/exec | 0.0161 | 0.017 | 1.056 | slower, short |
| 10.5m-1t/streaming/values-star/exec | 0.0207 | 0.0208 | 1.005 | slower, short |
| 10.5m-12t/eager/class-predicate-list/exec | 10.213 | 10.187 | 0.997 |  |
| 10.5m-12t/streaming/class-predicate-list/exec | 10.5404 | 10.7466 | 1.020 |  |
| 10.5m-12t/eager/class-predicates/exec | 10.1471 | 10.1525 | 1.001 |  |
| 10.5m-12t/streaming/class-predicates/exec | 10.5623 | 10.6942 | 1.012 |  |
| 10.5m-12t/eager/contains/exec | 4.2087 | 4.2729 | 1.015 |  |
| 10.5m-12t/streaming/contains/exec | 4.1981 | 4.2514 | 1.013 |  |
| 10.5m-12t/eager/count-all/exec | 0.0091 | 0.0089 | 0.978 |  |
| 10.5m-12t/streaming/count-all/exec | 0.0107 | 0.0118 | 1.103 | slower, short |
| 10.5m-12t/eager/distinct-join/exec | 39.015 | 39.425 | 1.011 |  |
| 10.5m-12t/streaming/distinct-join/exec | 37.7993 | 39.1068 | 1.035 |  |
| 10.5m-12t/eager/distinct-obj/exec | 0.0063 | 0.0061 | 0.968 |  |
| 10.5m-12t/streaming/distinct-obj/exec | 0.0087 | 0.0091 | 1.046 | slower, short |
| 10.5m-12t/eager/employee-docs/exec | 0.0674 | 0.0705 | 1.046 | slower, short |
| 10.5m-12t/streaming/employee-docs/exec | 0.0804 | 0.0782 | 0.973 |  |
| 10.5m-12t/eager/exists-join/exec | 8.9689 | 8.9409 | 0.997 |  |
| 10.5m-12t/streaming/exists-join/exec | 8.8335 | 8.9535 | 1.014 |  |
| 10.5m-12t/eager/export-500k/exec | 2.5833 | 2.3314 | 0.902 |  |
| 10.5m-12t/streaming/export-500k/exec | 0.4231 | 0.3858 | 0.912 |  |
| 10.5m-12t/eager/expr-agg-arg/exec | 28.7974 | 27.5282 | 0.956 |  |
| 10.5m-12t/streaming/expr-agg-arg/exec | 23.193 | 23.4391 | 1.011 |  |
| 10.5m-12t/eager/expr-bind-group/exec | 4.8261 | 4.8567 | 1.006 |  |
| 10.5m-12t/streaming/expr-bind-group/exec | 3.4278 | 3.4508 | 1.007 |  |
| 10.5m-12t/eager/expr-order-key/exec | 10.1057 | 10.167 | 1.006 |  |
| 10.5m-12t/streaming/expr-order-key/exec | 11.1112 | 11.0418 | 0.994 |  |
| 10.5m-12t/eager/group-avg/exec | 46.1542 | 45.628 | 0.989 |  |
| 10.5m-12t/streaming/group-avg/exec | 37.5777 | 37.877 | 1.008 |  |
| 10.5m-12t/eager/knows-reach/exec | 143.563 | 141.198 | 0.984 |  |
| 10.5m-12t/streaming/knows-reach/exec | 140.487 | 139.67 | 0.994 |  |
| 10.5m-12t/eager/lang-filter/exec | 2.9065 | 3.1255 | 1.075 | slower |
| 10.5m-12t/streaming/lang-filter/exec | 2.9165 | 3.1091 | 1.066 | slower |
| 10.5m-12t/eager/minus/exec | 3.3023 | 4.3339 | 1.312 | slower |
| 10.5m-12t/streaming/minus/exec | 2.571 | 2.5813 | 1.004 |  |
| 10.5m-12t/eager/not-exists/exec | 10.1632 | 9.6533 | 0.950 |  |
| 10.5m-12t/streaming/not-exists/exec | 9.0994 | 8.9508 | 0.984 |  |
| 10.5m-12t/eager/optional-chain/exec | 45.2343 | 26.8487 | 0.594 |  |
| 10.5m-12t/streaming/optional-chain/exec | 25.4781 | 26.8467 | 1.054 | slower |
| 10.5m-12t/eager/optional-count/exec | 5.4636 | 5.4027 | 0.989 |  |
| 10.5m-12t/streaming/optional-count/exec | 5.1799 | 5.4838 | 1.059 | slower |
| 10.5m-12t/eager/order-by-full/exec | 187.553 | 121.73 | 0.649 |  |
| 10.5m-12t/streaming/order-by-full/exec | 191.771 | 125.833 | 0.656 |  |
| 10.5m-12t/eager/path-plus/exec | 0.0153 | 0.014 | 0.915 |  |
| 10.5m-12t/streaming/path-plus/exec | 0.0149 | 0.0145 | 0.973 |  |
| 10.5m-12t/eager/predicate-counts/exec | 0.0076 | 0.0071 | 0.934 |  |
| 10.5m-12t/streaming/predicate-counts/exec | 0.0111 | 0.0114 | 1.027 | slower, short |
| 10.5m-12t/eager/predicate-subjects/exec | 0.0091 | 0.0093 | 1.022 | slower, short |
| 10.5m-12t/streaming/predicate-subjects/exec | 0.0145 | 0.0163 | 1.124 | slower, short |
| 10.5m-12t/eager/range-topk/exec | 2.1753 | 2.0682 | 0.951 |  |
| 10.5m-12t/streaming/range-topk/exec | 2.2206 | 2.0104 | 0.905 |  |
| 10.5m-12t/eager/regex-iri/exec | 6.2021 | 6.316 | 1.018 |  |
| 10.5m-12t/streaming/regex-iri/exec | 6.2201 | 6.3288 | 1.017 |  |
| 10.5m-12t/eager/star-join/exec | 13.7188 | 13.2739 | 0.968 |  |
| 10.5m-12t/streaming/star-join/exec | 14.0274 | 13.7042 | 0.977 |  |
| 10.5m-12t/eager/star-lookup/exec | 0.0733 | 0.0727 | 0.992 |  |
| 10.5m-12t/streaming/star-lookup/exec | 0.0806 | 0.084 | 1.042 | slower, short |
| 10.5m-12t/eager/subquery-agg/exec | 0.8063 | 0.8068 | 1.001 |  |
| 10.5m-12t/streaming/subquery-agg/exec | 0.8356 | 0.8455 | 1.012 |  |
| 10.5m-12t/eager/two-hop-count/exec | 11.3232 | 11.3696 | 1.004 |  |
| 10.5m-12t/streaming/two-hop-count/exec | 11.2561 | 11.1666 | 0.992 |  |
| 10.5m-12t/eager/types-grouped/exec | 0.0115 | 0.012 | 1.043 | slower, short |
| 10.5m-12t/streaming/types-grouped/exec | 0.0196 | 0.0195 | 0.995 |  |
| 10.5m-12t/eager/values-star/exec | 0.0206 | 0.0219 | 1.063 | slower, short |
| 10.5m-12t/streaming/values-star/exec | 0.0325 | 0.0289 | 0.889 |  |

248 metrics compared, 72 flagged, 0 failed checks, 0 missing checks, 0 changed fingerprints.
