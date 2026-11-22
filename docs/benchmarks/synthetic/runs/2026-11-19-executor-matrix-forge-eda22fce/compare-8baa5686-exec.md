# executors gate: candidate against baseline

- baseline: host forge, commit 8baa5686, started 2026-11-18T09:27:30Z
- candidate: host forge, commit eda22fce, started 2026-11-19T02:31:44Z
- threshold: 5.0% slower, 0.0% below 0.3 ms

| metric | baseline | candidate | ratio | flag |
|---|--:|--:|--:|---|
| 1.05m-1t/eager/class-predicate-list/exec | 0.9286 | 0.9351 | 1.007 |  |
| 1.05m-1t/streaming/class-predicate-list/exec | 0.9251 | 0.9282 | 1.003 |  |
| 1.05m-1t/eager/class-predicates/exec | 0.9376 | 0.9398 | 1.002 |  |
| 1.05m-1t/streaming/class-predicates/exec | 0.9378 | 0.9366 | 0.999 |  |
| 1.05m-1t/eager/contains/exec | 2.5478 | 2.5511 | 1.001 |  |
| 1.05m-1t/streaming/contains/exec | 2.5484 | 2.5533 | 1.002 |  |
| 1.05m-1t/eager/count-all/exec | 0.0047 | 0.0051 | 1.085 | slower, short |
| 1.05m-1t/streaming/count-all/exec | 0.0065 | 0.006 | 0.923 |  |
| 1.05m-1t/eager/distinct-join/exec | 3.5928 | 3.637 | 1.012 |  |
| 1.05m-1t/streaming/distinct-join/exec | 3.8374 | 3.8231 | 0.996 |  |
| 1.05m-1t/eager/distinct-obj/exec | 0.0039 | 0.0042 | 1.077 | slower, short |
| 1.05m-1t/streaming/distinct-obj/exec | 0.0052 | 0.0052 | 1.000 |  |
| 1.05m-1t/eager/employee-docs/exec | 0.0367 | 0.0379 | 1.033 | slower, short |
| 1.05m-1t/streaming/employee-docs/exec | 0.0397 | 0.0416 | 1.048 | slower, short |
| 1.05m-1t/eager/exists-join/exec | 1.0493 | 1.0705 | 1.020 |  |
| 1.05m-1t/streaming/exists-join/exec | 1.0432 | 1.0442 | 1.001 |  |
| 1.05m-1t/eager/export-500k/exec | 2.0962 | 2.3049 | 1.100 | slower |
| 1.05m-1t/streaming/export-500k/exec | 0.3945 | 0.3933 | 0.997 |  |
| 1.05m-1t/eager/expr-agg-arg/exec | 2.5452 | 2.5585 | 1.005 |  |
| 1.05m-1t/streaming/expr-agg-arg/exec | 1.9221 | 1.9207 | 0.999 |  |
| 1.05m-1t/eager/expr-bind-group/exec | 0.601 | 0.6017 | 1.001 |  |
| 1.05m-1t/streaming/expr-bind-group/exec | 0.3705 | 0.3723 | 1.005 |  |
| 1.05m-1t/eager/expr-order-key/exec | 1.1594 | 1.1538 | 0.995 |  |
| 1.05m-1t/streaming/expr-order-key/exec | 1.1466 | 1.1389 | 0.993 |  |
| 1.05m-1t/eager/group-avg/exec | 3.8903 | 3.8431 | 0.988 |  |
| 1.05m-1t/streaming/group-avg/exec | 3.1995 | 3.1943 | 0.998 |  |
| 1.05m-1t/eager/knows-reach/exec | 11.4978 | 11.5181 | 1.002 |  |
| 1.05m-1t/streaming/knows-reach/exec | 11.3287 | 11.2934 | 0.997 |  |
| 1.05m-1t/eager/lang-filter/exec | 1.8326 | 1.8666 | 1.019 |  |
| 1.05m-1t/streaming/lang-filter/exec | 1.8391 | 1.8642 | 1.014 |  |
| 1.05m-1t/eager/minus/exec | 0.4119 | 0.412 | 1.000 |  |
| 1.05m-1t/streaming/minus/exec | 0.2617 | 0.2613 | 0.998 |  |
| 1.05m-1t/eager/not-exists/exec | 0.806 | 0.8045 | 0.998 |  |
| 1.05m-1t/streaming/not-exists/exec | 0.7415 | 0.7419 | 1.001 |  |
| 1.05m-1t/eager/optional-chain/exec | 2.0171 | 2.0203 | 1.002 |  |
| 1.05m-1t/streaming/optional-chain/exec | 2.0606 | 2.0655 | 1.002 |  |
| 1.05m-1t/eager/optional-count/exec | 0.4841 | 0.4837 | 0.999 |  |
| 1.05m-1t/streaming/optional-count/exec | 0.5438 | 0.5416 | 0.996 |  |
| 1.05m-1t/eager/order-by-full/exec | 41.0957 | 41.2255 | 1.003 |  |
| 1.05m-1t/streaming/order-by-full/exec | 41.5359 | 41.5499 | 1.000 |  |
| 1.05m-1t/eager/path-plus/exec | 0.0112 | 0.0118 | 1.054 | slower, short |
| 1.05m-1t/streaming/path-plus/exec | 0.0133 | 0.0129 | 0.970 |  |
| 1.05m-1t/eager/predicate-counts/exec | 0.0045 | 0.005 | 1.111 | slower, short |
| 1.05m-1t/streaming/predicate-counts/exec | 0.0069 | 0.0072 | 1.043 | slower, short |
| 1.05m-1t/eager/predicate-subjects/exec | 0.0068 | 0.0076 | 1.118 | slower, short |
| 1.05m-1t/streaming/predicate-subjects/exec | 0.0093 | 0.0093 | 1.000 |  |
| 1.05m-1t/eager/range-topk/exec | 0.5427 | 0.5426 | 1.000 |  |
| 1.05m-1t/streaming/range-topk/exec | 0.5369 | 0.5366 | 0.999 |  |
| 1.05m-1t/eager/regex-iri/exec | 3.8049 | 3.7465 | 0.985 |  |
| 1.05m-1t/streaming/regex-iri/exec | 3.7996 | 3.7252 | 0.980 |  |
| 1.05m-1t/eager/star-join/exec | 1.4137 | 1.4143 | 1.000 |  |
| 1.05m-1t/streaming/star-join/exec | 1.4395 | 1.4315 | 0.994 |  |
| 1.05m-1t/eager/star-lookup/exec | 0.0621 | 0.0633 | 1.019 | slower, short |
| 1.05m-1t/streaming/star-lookup/exec | 0.0645 | 0.0665 | 1.031 | slower, short |
| 1.05m-1t/eager/subquery-agg/exec | 0.1862 | 0.1873 | 1.006 | slower, short |
| 1.05m-1t/streaming/subquery-agg/exec | 0.1836 | 0.1857 | 1.011 | slower, short |
| 1.05m-1t/eager/two-hop-count/exec | 2.6222 | 2.6324 | 1.004 |  |
| 1.05m-1t/streaming/two-hop-count/exec | 2.6159 | 2.6261 | 1.004 |  |
| 1.05m-1t/eager/types-grouped/exec | 0.0081 | 0.0081 | 1.000 |  |
| 1.05m-1t/streaming/types-grouped/exec | 0.0128 | 0.0133 | 1.039 | slower, short |
| 1.05m-1t/eager/values-star/exec | 0.0152 | 0.0171 | 1.125 | slower, short |
| 1.05m-1t/streaming/values-star/exec | 0.02 | 0.021 | 1.050 | slower, short |
| 1.05m-12t/eager/class-predicate-list/exec | 0.9861 | 0.9923 | 1.006 |  |
| 1.05m-12t/streaming/class-predicate-list/exec | 1.0122 | 0.9911 | 0.979 |  |
| 1.05m-12t/eager/class-predicates/exec | 0.9958 | 1.0059 | 1.010 |  |
| 1.05m-12t/streaming/class-predicates/exec | 1.0197 | 1.0061 | 0.987 |  |
| 1.05m-12t/eager/contains/exec | 0.7585 | 0.7354 | 0.970 |  |
| 1.05m-12t/streaming/contains/exec | 0.7729 | 0.7417 | 0.960 |  |
| 1.05m-12t/eager/count-all/exec | 0.008 | 0.0067 | 0.838 |  |
| 1.05m-12t/streaming/count-all/exec | 0.0103 | 0.0105 | 1.019 | slower, short |
| 1.05m-12t/eager/distinct-join/exec | 3.7598 | 3.8617 | 1.027 |  |
| 1.05m-12t/streaming/distinct-join/exec | 4.015 | 3.9699 | 0.989 |  |
| 1.05m-12t/eager/distinct-obj/exec | 0.0068 | 0.006 | 0.882 |  |
| 1.05m-12t/streaming/distinct-obj/exec | 0.0087 | 0.0113 | 1.299 | slower, short |
| 1.05m-12t/eager/employee-docs/exec | 0.0505 | 0.0518 | 1.026 | slower, short |
| 1.05m-12t/streaming/employee-docs/exec | 0.0542 | 0.055 | 1.015 | slower, short |
| 1.05m-12t/eager/exists-join/exec | 1.1391 | 1.1625 | 1.021 |  |
| 1.05m-12t/streaming/exists-join/exec | 1.1496 | 1.1414 | 0.993 |  |
| 1.05m-12t/eager/export-500k/exec | 2.3889 | 2.3447 | 0.981 |  |
| 1.05m-12t/streaming/export-500k/exec | 0.3735 | 0.3765 | 1.008 |  |
| 1.05m-12t/eager/expr-agg-arg/exec | 2.9012 | 2.8954 | 0.998 |  |
| 1.05m-12t/streaming/expr-agg-arg/exec | 2.0462 | 2.0282 | 0.991 |  |
| 1.05m-12t/eager/expr-bind-group/exec | 0.7623 | 0.8281 | 1.086 | slower |
| 1.05m-12t/streaming/expr-bind-group/exec | 0.3979 | 0.3917 | 0.984 |  |
| 1.05m-12t/eager/expr-order-key/exec | 1.2422 | 1.2549 | 1.010 |  |
| 1.05m-12t/streaming/expr-order-key/exec | 1.2247 | 1.2376 | 1.011 |  |
| 1.05m-12t/eager/group-avg/exec | 4.0346 | 4.0508 | 1.004 |  |
| 1.05m-12t/streaming/group-avg/exec | 3.3616 | 3.3589 | 0.999 |  |
| 1.05m-12t/eager/knows-reach/exec | 11.8109 | 11.7945 | 0.999 |  |
| 1.05m-12t/streaming/knows-reach/exec | 11.6777 | 11.5128 | 0.986 |  |
| 1.05m-12t/eager/lang-filter/exec | 0.7779 | 0.8024 | 1.031 |  |
| 1.05m-12t/streaming/lang-filter/exec | 0.8097 | 0.7663 | 0.946 |  |
| 1.05m-12t/eager/minus/exec | 0.4813 | 0.4577 | 0.951 |  |
| 1.05m-12t/streaming/minus/exec | 0.2805 | 0.2853 | 1.017 | slower, short |
| 1.05m-12t/eager/not-exists/exec | 0.8673 | 0.8687 | 1.002 |  |
| 1.05m-12t/streaming/not-exists/exec | 0.7928 | 0.7965 | 1.005 |  |
| 1.05m-12t/eager/optional-chain/exec | 2.1128 | 2.1283 | 1.007 |  |
| 1.05m-12t/streaming/optional-chain/exec | 2.2341 | 2.1998 | 0.985 |  |
| 1.05m-12t/eager/optional-count/exec | 0.5478 | 0.575 | 1.050 |  |
| 1.05m-12t/streaming/optional-count/exec | 0.5751 | 0.5797 | 1.008 |  |
| 1.05m-12t/eager/order-by-full/exec | 8.7682 | 8.7521 | 0.998 |  |
| 1.05m-12t/streaming/order-by-full/exec | 9.0326 | 9.1738 | 1.016 |  |
| 1.05m-12t/eager/path-plus/exec | 0.0175 | 0.0183 | 1.046 | slower, short |
| 1.05m-12t/streaming/path-plus/exec | 0.0201 | 0.0208 | 1.035 | slower, short |
| 1.05m-12t/eager/predicate-counts/exec | 0.0074 | 0.0069 | 0.932 |  |
| 1.05m-12t/streaming/predicate-counts/exec | 0.0108 | 0.0107 | 0.991 |  |
| 1.05m-12t/eager/predicate-subjects/exec | 0.0083 | 0.0102 | 1.229 | slower, short |
| 1.05m-12t/streaming/predicate-subjects/exec | 0.0148 | 0.0155 | 1.047 | slower, short |
| 1.05m-12t/eager/range-topk/exec | 0.582 | 0.5892 | 1.012 |  |
| 1.05m-12t/streaming/range-topk/exec | 0.5746 | 0.5799 | 1.009 |  |
| 1.05m-12t/eager/regex-iri/exec | 1.0236 | 1.0558 | 1.031 |  |
| 1.05m-12t/streaming/regex-iri/exec | 1.017 | 1.0866 | 1.068 | slower |
| 1.05m-12t/eager/star-join/exec | 1.5913 | 1.6431 | 1.033 |  |
| 1.05m-12t/streaming/star-join/exec | 1.6511 | 1.66 | 1.005 |  |
| 1.05m-12t/eager/star-lookup/exec | 0.0831 | 0.0862 | 1.037 | slower, short |
| 1.05m-12t/streaming/star-lookup/exec | 0.0933 | 0.089 | 0.954 |  |
| 1.05m-12t/eager/subquery-agg/exec | 0.2184 | 0.2505 | 1.147 | slower, short |
| 1.05m-12t/streaming/subquery-agg/exec | 0.2094 | 0.2221 | 1.061 | slower, short |
| 1.05m-12t/eager/two-hop-count/exec | 1.2316 | 1.2374 | 1.005 |  |
| 1.05m-12t/streaming/two-hop-count/exec | 1.2287 | 1.2279 | 0.999 |  |
| 1.05m-12t/eager/types-grouped/exec | 0.0122 | 0.0119 | 0.975 |  |
| 1.05m-12t/streaming/types-grouped/exec | 0.0209 | 0.0189 | 0.904 |  |
| 1.05m-12t/eager/values-star/exec | 0.021 | 0.0213 | 1.014 | slower, short |
| 1.05m-12t/streaming/values-star/exec | 0.0313 | 0.027 | 0.863 |  |
| 10.5m-1t/eager/class-predicate-list/exec | 10.1028 | 10.0573 | 0.995 |  |
| 10.5m-1t/streaming/class-predicate-list/exec | 10.3444 | 10.345 | 1.000 |  |
| 10.5m-1t/eager/class-predicates/exec | 10.0737 | 9.9658 | 0.989 |  |
| 10.5m-1t/streaming/class-predicates/exec | 10.335 | 10.3754 | 1.004 |  |
| 10.5m-1t/eager/contains/exec | 24.6489 | 24.7219 | 1.003 |  |
| 10.5m-1t/streaming/contains/exec | 24.6929 | 24.6633 | 0.999 |  |
| 10.5m-1t/eager/count-all/exec | 0.0063 | 0.0059 | 0.937 |  |
| 10.5m-1t/streaming/count-all/exec | 0.0076 | 0.007 | 0.921 |  |
| 10.5m-1t/eager/distinct-join/exec | 39.1584 | 39.112 | 0.999 |  |
| 10.5m-1t/streaming/distinct-join/exec | 38.2143 | 38.223 | 1.000 |  |
| 10.5m-1t/eager/distinct-obj/exec | 0.0043 | 0.0045 | 1.047 | slower, short |
| 10.5m-1t/streaming/distinct-obj/exec | 0.0054 | 0.0052 | 0.963 |  |
| 10.5m-1t/eager/employee-docs/exec | 0.05 | 0.0495 | 0.990 |  |
| 10.5m-1t/streaming/employee-docs/exec | 0.0538 | 0.0533 | 0.991 |  |
| 10.5m-1t/eager/exists-join/exec | 8.6015 | 8.5973 | 1.000 |  |
| 10.5m-1t/streaming/exists-join/exec | 8.6288 | 8.588 | 0.995 |  |
| 10.5m-1t/eager/export-500k/exec | 2.3207 | 2.182 | 0.940 |  |
| 10.5m-1t/streaming/export-500k/exec | 0.3997 | 0.4036 | 1.010 |  |
| 10.5m-1t/eager/expr-agg-arg/exec | 28.5302 | 28.0454 | 0.983 |  |
| 10.5m-1t/streaming/expr-agg-arg/exec | 23.1268 | 23.0515 | 0.997 |  |
| 10.5m-1t/eager/expr-bind-group/exec | 5.7654 | 5.6352 | 0.977 |  |
| 10.5m-1t/streaming/expr-bind-group/exec | 3.2952 | 3.2927 | 0.999 |  |
| 10.5m-1t/eager/expr-order-key/exec | 11.0564 | 11.057 | 1.000 |  |
| 10.5m-1t/streaming/expr-order-key/exec | 10.6506 | 10.7384 | 1.008 |  |
| 10.5m-1t/eager/group-avg/exec | 45.2064 | 44.5917 | 0.986 |  |
| 10.5m-1t/streaming/group-avg/exec | 37.2281 | 36.9266 | 0.992 |  |
| 10.5m-1t/eager/knows-reach/exec | 140.011 | 139.016 | 0.993 |  |
| 10.5m-1t/streaming/knows-reach/exec | 137.307 | 136.442 | 0.994 |  |
| 10.5m-1t/eager/lang-filter/exec | 18.0412 | 18.3818 | 1.019 |  |
| 10.5m-1t/streaming/lang-filter/exec | 18.0744 | 18.456 | 1.021 |  |
| 10.5m-1t/eager/minus/exec | 4.3002 | 4.2318 | 0.984 |  |
| 10.5m-1t/streaming/minus/exec | 2.4674 | 2.4642 | 0.999 |  |
| 10.5m-1t/eager/not-exists/exec | 9.477 | 9.4508 | 0.997 |  |
| 10.5m-1t/streaming/not-exists/exec | 8.737 | 8.7239 | 0.999 |  |
| 10.5m-1t/eager/optional-chain/exec | 26.9698 | 26.7278 | 0.991 |  |
| 10.5m-1t/streaming/optional-chain/exec | 25.3125 | 24.8735 | 0.983 |  |
| 10.5m-1t/eager/optional-count/exec | 5.3051 | 5.2754 | 0.994 |  |
| 10.5m-1t/streaming/optional-count/exec | 5.2789 | 5.275 | 0.999 |  |
| 10.5m-1t/eager/order-by-full/exec | 517.2 | 514.678 | 0.995 |  |
| 10.5m-1t/streaming/order-by-full/exec | 519.819 | 518.713 | 0.998 |  |
| 10.5m-1t/eager/path-plus/exec | 0.0089 | 0.0088 | 0.989 |  |
| 10.5m-1t/streaming/path-plus/exec | 0.0094 | 0.0093 | 0.989 |  |
| 10.5m-1t/eager/predicate-counts/exec | 0.0046 | 0.0052 | 1.130 | slower, short |
| 10.5m-1t/streaming/predicate-counts/exec | 0.0069 | 0.0064 | 0.928 |  |
| 10.5m-1t/eager/predicate-subjects/exec | 0.0069 | 0.007 | 1.014 | slower, short |
| 10.5m-1t/streaming/predicate-subjects/exec | 0.009 | 0.0087 | 0.967 |  |
| 10.5m-1t/eager/range-topk/exec | 5.1719 | 5.1506 | 0.996 |  |
| 10.5m-1t/streaming/range-topk/exec | 5.0924 | 5.0912 | 1.000 |  |
| 10.5m-1t/eager/regex-iri/exec | 38.1641 | 37.6599 | 0.987 |  |
| 10.5m-1t/streaming/regex-iri/exec | 38.1572 | 37.6355 | 0.986 |  |
| 10.5m-1t/eager/star-join/exec | 14.1484 | 14.0741 | 0.995 |  |
| 10.5m-1t/streaming/star-join/exec | 14.8431 | 14.8748 | 1.002 |  |
| 10.5m-1t/eager/star-lookup/exec | 0.0577 | 0.058 | 1.005 | slower, short |
| 10.5m-1t/streaming/star-lookup/exec | 0.0583 | 0.0607 | 1.041 | slower, short |
| 10.5m-1t/eager/subquery-agg/exec | 1.6937 | 1.697 | 1.002 |  |
| 10.5m-1t/streaming/subquery-agg/exec | 1.6263 | 1.6321 | 1.004 |  |
| 10.5m-1t/eager/two-hop-count/exec | 27.585 | 27.6557 | 1.003 |  |
| 10.5m-1t/streaming/two-hop-count/exec | 27.5679 | 27.6555 | 1.003 |  |
| 10.5m-1t/eager/types-grouped/exec | 0.0083 | 0.0081 | 0.976 |  |
| 10.5m-1t/streaming/types-grouped/exec | 0.0128 | 0.0128 | 1.000 |  |
| 10.5m-1t/eager/values-star/exec | 0.0162 | 0.017 | 1.049 | slower, short |
| 10.5m-1t/streaming/values-star/exec | 0.0198 | 0.0208 | 1.051 | slower, short |
| 10.5m-12t/eager/class-predicate-list/exec | 10.4838 | 10.187 | 0.972 |  |
| 10.5m-12t/streaming/class-predicate-list/exec | 10.6001 | 10.7466 | 1.014 |  |
| 10.5m-12t/eager/class-predicates/exec | 10.2841 | 10.1525 | 0.987 |  |
| 10.5m-12t/streaming/class-predicates/exec | 10.7224 | 10.6942 | 0.997 |  |
| 10.5m-12t/eager/contains/exec | 4.2557 | 4.2729 | 1.004 |  |
| 10.5m-12t/streaming/contains/exec | 4.2746 | 4.2514 | 0.995 |  |
| 10.5m-12t/eager/count-all/exec | 0.0086 | 0.0089 | 1.035 | slower, short |
| 10.5m-12t/streaming/count-all/exec | 0.0114 | 0.0118 | 1.035 | slower, short |
| 10.5m-12t/eager/distinct-join/exec | 39.2794 | 39.425 | 1.004 |  |
| 10.5m-12t/streaming/distinct-join/exec | 39.0919 | 39.1068 | 1.000 |  |
| 10.5m-12t/eager/distinct-obj/exec | 0.0067 | 0.0061 | 0.910 |  |
| 10.5m-12t/streaming/distinct-obj/exec | 0.0095 | 0.0091 | 0.958 |  |
| 10.5m-12t/eager/employee-docs/exec | 0.0686 | 0.0705 | 1.028 | slower, short |
| 10.5m-12t/streaming/employee-docs/exec | 0.0766 | 0.0782 | 1.021 | slower, short |
| 10.5m-12t/eager/exists-join/exec | 8.9501 | 8.9409 | 0.999 |  |
| 10.5m-12t/streaming/exists-join/exec | 8.9148 | 8.9535 | 1.004 |  |
| 10.5m-12t/eager/export-500k/exec | 2.198 | 2.3314 | 1.061 | slower |
| 10.5m-12t/streaming/export-500k/exec | 0.3828 | 0.3858 | 1.008 |  |
| 10.5m-12t/eager/expr-agg-arg/exec | 27.6744 | 27.5282 | 0.995 |  |
| 10.5m-12t/streaming/expr-agg-arg/exec | 23.4144 | 23.4391 | 1.001 |  |
| 10.5m-12t/eager/expr-bind-group/exec | 4.7512 | 4.8567 | 1.022 |  |
| 10.5m-12t/streaming/expr-bind-group/exec | 3.413 | 3.4508 | 1.011 |  |
| 10.5m-12t/eager/expr-order-key/exec | 10.1028 | 10.167 | 1.006 |  |
| 10.5m-12t/streaming/expr-order-key/exec | 10.9592 | 11.0418 | 1.008 |  |
| 10.5m-12t/eager/group-avg/exec | 45.4409 | 45.628 | 1.004 |  |
| 10.5m-12t/streaming/group-avg/exec | 37.7191 | 37.877 | 1.004 |  |
| 10.5m-12t/eager/knows-reach/exec | 141.111 | 141.198 | 1.001 |  |
| 10.5m-12t/streaming/knows-reach/exec | 139.26 | 139.67 | 1.003 |  |
| 10.5m-12t/eager/lang-filter/exec | 3.1237 | 3.1255 | 1.001 |  |
| 10.5m-12t/streaming/lang-filter/exec | 3.0565 | 3.1091 | 1.017 |  |
| 10.5m-12t/eager/minus/exec | 4.3026 | 4.3339 | 1.007 |  |
| 10.5m-12t/streaming/minus/exec | 2.5863 | 2.5813 | 0.998 |  |
| 10.5m-12t/eager/not-exists/exec | 9.755 | 9.6533 | 0.990 |  |
| 10.5m-12t/streaming/not-exists/exec | 8.9493 | 8.9508 | 1.000 |  |
| 10.5m-12t/eager/optional-chain/exec | 25.1457 | 26.8487 | 1.068 | slower |
| 10.5m-12t/streaming/optional-chain/exec | 26.5077 | 26.8467 | 1.013 |  |
| 10.5m-12t/eager/optional-count/exec | 5.4023 | 5.4027 | 1.000 |  |
| 10.5m-12t/streaming/optional-count/exec | 5.4524 | 5.4838 | 1.006 |  |
| 10.5m-12t/eager/order-by-full/exec | 108.713 | 121.73 | 1.120 | slower |
| 10.5m-12t/streaming/order-by-full/exec | 107.363 | 125.833 | 1.172 | slower |
| 10.5m-12t/eager/path-plus/exec | 0.0125 | 0.014 | 1.120 | slower, short |
| 10.5m-12t/streaming/path-plus/exec | 0.0153 | 0.0145 | 0.948 |  |
| 10.5m-12t/eager/predicate-counts/exec | 0.0067 | 0.0071 | 1.060 | slower, short |
| 10.5m-12t/streaming/predicate-counts/exec | 0.0109 | 0.0114 | 1.046 | slower, short |
| 10.5m-12t/eager/predicate-subjects/exec | 0.0091 | 0.0093 | 1.022 | slower, short |
| 10.5m-12t/streaming/predicate-subjects/exec | 0.0167 | 0.0163 | 0.976 |  |
| 10.5m-12t/eager/range-topk/exec | 2.1106 | 2.0682 | 0.980 |  |
| 10.5m-12t/streaming/range-topk/exec | 2.0712 | 2.0104 | 0.971 |  |
| 10.5m-12t/eager/regex-iri/exec | 6.4991 | 6.316 | 0.972 |  |
| 10.5m-12t/streaming/regex-iri/exec | 6.503 | 6.3288 | 0.973 |  |
| 10.5m-12t/eager/star-join/exec | 13.3085 | 13.2739 | 0.997 |  |
| 10.5m-12t/streaming/star-join/exec | 13.9195 | 13.7042 | 0.985 |  |
| 10.5m-12t/eager/star-lookup/exec | 0.0734 | 0.0727 | 0.990 |  |
| 10.5m-12t/streaming/star-lookup/exec | 0.0842 | 0.084 | 0.998 |  |
| 10.5m-12t/eager/subquery-agg/exec | 0.8224 | 0.8068 | 0.981 |  |
| 10.5m-12t/streaming/subquery-agg/exec | 0.7732 | 0.8455 | 1.094 | slower |
| 10.5m-12t/eager/two-hop-count/exec | 11.3129 | 11.3696 | 1.005 |  |
| 10.5m-12t/streaming/two-hop-count/exec | 11.1765 | 11.1666 | 0.999 |  |
| 10.5m-12t/eager/types-grouped/exec | 0.0122 | 0.012 | 0.984 |  |
| 10.5m-12t/streaming/types-grouped/exec | 0.0201 | 0.0195 | 0.970 |  |
| 10.5m-12t/eager/values-star/exec | 0.0207 | 0.0219 | 1.058 | slower, short |
| 10.5m-12t/streaming/values-star/exec | 0.0275 | 0.0289 | 1.051 | slower, short |

248 metrics compared, 53 flagged, 0 failed checks, 0 missing checks, 0 changed fingerprints.
