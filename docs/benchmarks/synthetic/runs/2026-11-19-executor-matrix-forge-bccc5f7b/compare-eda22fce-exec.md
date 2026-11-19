# executors gate: candidate against baseline

- baseline: host forge, commit eda22fce, started 2026-11-19T02:31:44Z
- candidate: host forge, commit bccc5f7b, started 2026-11-19T00:08:03Z
- threshold: 5.0% slower, 0.0% below 0.3 ms

| metric | baseline | candidate | ratio | flag |
|---|--:|--:|--:|---|
| 1.05m-1t/eager/class-predicate-list/exec | 0.9351 | 0.9425 | 1.008 |  |
| 1.05m-1t/streaming/class-predicate-list/exec | 0.9282 | 0.9335 | 1.006 |  |
| 1.05m-1t/eager/class-predicates/exec | 0.9398 | 0.9542 | 1.015 |  |
| 1.05m-1t/streaming/class-predicates/exec | 0.9366 | 0.945 | 1.009 |  |
| 1.05m-1t/eager/contains/exec | 2.5511 | 2.5341 | 0.993 |  |
| 1.05m-1t/streaming/contains/exec | 2.5533 | 2.5357 | 0.993 |  |
| 1.05m-1t/eager/count-all/exec | 0.0051 | 0.0051 | 1.000 |  |
| 1.05m-1t/streaming/count-all/exec | 0.006 | 0.0062 | 1.033 | slower, short |
| 1.05m-1t/eager/distinct-join/exec | 3.637 | 3.556 | 0.978 |  |
| 1.05m-1t/streaming/distinct-join/exec | 3.8231 | 3.7412 | 0.979 |  |
| 1.05m-1t/eager/distinct-obj/exec | 0.0042 | 0.0042 | 1.000 |  |
| 1.05m-1t/streaming/distinct-obj/exec | 0.0052 | 0.0052 | 1.000 |  |
| 1.05m-1t/eager/employee-docs/exec | 0.0379 | 0.0379 | 1.000 |  |
| 1.05m-1t/streaming/employee-docs/exec | 0.0416 | 0.0397 | 0.954 |  |
| 1.05m-1t/eager/exists-join/exec | 1.0705 | 1.0598 | 0.990 |  |
| 1.05m-1t/streaming/exists-join/exec | 1.0442 | 1.038 | 0.994 |  |
| 1.05m-1t/eager/export-500k/exec | 2.3049 | 2.1283 | 0.923 |  |
| 1.05m-1t/streaming/export-500k/exec | 0.3933 | 0.4029 | 1.024 |  |
| 1.05m-1t/eager/expr-agg-arg/exec | 2.5585 | 2.2869 | 0.894 |  |
| 1.05m-1t/streaming/expr-agg-arg/exec | 1.9207 | 1.9303 | 1.005 |  |
| 1.05m-1t/eager/expr-bind-group/exec | 0.6017 | 0.5071 | 0.843 |  |
| 1.05m-1t/streaming/expr-bind-group/exec | 0.3723 | 0.3637 | 0.977 |  |
| 1.05m-1t/eager/expr-order-key/exec | 1.1538 | 1.1427 | 0.990 |  |
| 1.05m-1t/streaming/expr-order-key/exec | 1.1389 | 1.1415 | 1.002 |  |
| 1.05m-1t/eager/group-avg/exec | 3.8431 | 3.5797 | 0.931 |  |
| 1.05m-1t/streaming/group-avg/exec | 3.1943 | 3.1865 | 0.998 |  |
| 1.05m-1t/eager/knows-reach/exec | 11.5181 | 11.339 | 0.984 |  |
| 1.05m-1t/streaming/knows-reach/exec | 11.2934 | 11.3938 | 1.009 |  |
| 1.05m-1t/eager/lang-filter/exec | 1.8666 | 1.7853 | 0.956 |  |
| 1.05m-1t/streaming/lang-filter/exec | 1.8642 | 1.7853 | 0.958 |  |
| 1.05m-1t/eager/minus/exec | 0.412 | 0.3652 | 0.886 |  |
| 1.05m-1t/streaming/minus/exec | 0.2613 | 0.263 | 1.007 | slower, short |
| 1.05m-1t/eager/not-exists/exec | 0.8045 | 0.7642 | 0.950 |  |
| 1.05m-1t/streaming/not-exists/exec | 0.7419 | 0.7418 | 1.000 |  |
| 1.05m-1t/eager/optional-chain/exec | 2.0203 | 2.0081 | 0.994 |  |
| 1.05m-1t/streaming/optional-chain/exec | 2.0655 | 2.0606 | 0.998 |  |
| 1.05m-1t/eager/optional-count/exec | 0.4837 | 0.4164 | 0.861 |  |
| 1.05m-1t/streaming/optional-count/exec | 0.5416 | 0.5412 | 0.999 |  |
| 1.05m-1t/eager/order-by-full/exec | 41.2255 | 41.7813 | 1.013 |  |
| 1.05m-1t/streaming/order-by-full/exec | 41.5499 | 41.5443 | 1.000 |  |
| 1.05m-1t/eager/path-plus/exec | 0.0118 | 0.0128 | 1.085 | slower, short |
| 1.05m-1t/streaming/path-plus/exec | 0.0129 | 0.0128 | 0.992 |  |
| 1.05m-1t/eager/predicate-counts/exec | 0.005 | 0.0052 | 1.040 | slower, short |
| 1.05m-1t/streaming/predicate-counts/exec | 0.0072 | 0.0069 | 0.958 |  |
| 1.05m-1t/eager/predicate-subjects/exec | 0.0076 | 0.0071 | 0.934 |  |
| 1.05m-1t/streaming/predicate-subjects/exec | 0.0093 | 0.0089 | 0.957 |  |
| 1.05m-1t/eager/range-topk/exec | 0.5426 | 0.5417 | 0.998 |  |
| 1.05m-1t/streaming/range-topk/exec | 0.5366 | 0.535 | 0.997 |  |
| 1.05m-1t/eager/regex-iri/exec | 3.7465 | 3.7539 | 1.002 |  |
| 1.05m-1t/streaming/regex-iri/exec | 3.7252 | 3.7667 | 1.011 |  |
| 1.05m-1t/eager/star-join/exec | 1.4143 | 1.3985 | 0.989 |  |
| 1.05m-1t/streaming/star-join/exec | 1.4315 | 1.4166 | 0.990 |  |
| 1.05m-1t/eager/star-lookup/exec | 0.0633 | 0.0617 | 0.975 |  |
| 1.05m-1t/streaming/star-lookup/exec | 0.0665 | 0.0626 | 0.941 |  |
| 1.05m-1t/eager/subquery-agg/exec | 0.1873 | 0.1851 | 0.988 |  |
| 1.05m-1t/streaming/subquery-agg/exec | 0.1857 | 0.1855 | 0.999 |  |
| 1.05m-1t/eager/two-hop-count/exec | 2.6324 | 2.6354 | 1.001 |  |
| 1.05m-1t/streaming/two-hop-count/exec | 2.6261 | 2.6254 | 1.000 |  |
| 1.05m-1t/eager/types-grouped/exec | 0.0081 | 0.008 | 0.988 |  |
| 1.05m-1t/streaming/types-grouped/exec | 0.0133 | 0.013 | 0.977 |  |
| 1.05m-1t/eager/values-star/exec | 0.0171 | 0.0161 | 0.942 |  |
| 1.05m-1t/streaming/values-star/exec | 0.021 | 0.0195 | 0.929 |  |
| 1.05m-12t/eager/class-predicate-list/exec | 0.9923 | 0.9889 | 0.997 |  |
| 1.05m-12t/streaming/class-predicate-list/exec | 0.9911 | 1.0285 | 1.038 |  |
| 1.05m-12t/eager/class-predicates/exec | 1.0059 | 0.9972 | 0.991 |  |
| 1.05m-12t/streaming/class-predicates/exec | 1.0061 | 1.0521 | 1.046 |  |
| 1.05m-12t/eager/contains/exec | 0.7354 | 0.7315 | 0.995 |  |
| 1.05m-12t/streaming/contains/exec | 0.7417 | 0.7625 | 1.028 |  |
| 1.05m-12t/eager/count-all/exec | 0.0067 | 0.0066 | 0.985 |  |
| 1.05m-12t/streaming/count-all/exec | 0.0105 | 0.01 | 0.952 |  |
| 1.05m-12t/eager/distinct-join/exec | 3.8617 | 3.6951 | 0.957 |  |
| 1.05m-12t/streaming/distinct-join/exec | 3.9699 | 3.9186 | 0.987 |  |
| 1.05m-12t/eager/distinct-obj/exec | 0.006 | 0.0062 | 1.033 | slower, short |
| 1.05m-12t/streaming/distinct-obj/exec | 0.0113 | 0.0087 | 0.770 |  |
| 1.05m-12t/eager/employee-docs/exec | 0.0518 | 0.0505 | 0.975 |  |
| 1.05m-12t/streaming/employee-docs/exec | 0.055 | 0.0561 | 1.020 | slower, short |
| 1.05m-12t/eager/exists-join/exec | 1.1625 | 1.1449 | 0.985 |  |
| 1.05m-12t/streaming/exists-join/exec | 1.1414 | 1.1417 | 1.000 |  |
| 1.05m-12t/eager/export-500k/exec | 2.3447 | 2.3765 | 1.014 |  |
| 1.05m-12t/streaming/export-500k/exec | 0.3765 | 0.3764 | 1.000 |  |
| 1.05m-12t/eager/expr-agg-arg/exec | 2.8954 | 2.6789 | 0.925 |  |
| 1.05m-12t/streaming/expr-agg-arg/exec | 2.0282 | 2.0678 | 1.020 |  |
| 1.05m-12t/eager/expr-bind-group/exec | 0.8281 | 0.6675 | 0.806 |  |
| 1.05m-12t/streaming/expr-bind-group/exec | 0.3917 | 0.3951 | 1.009 |  |
| 1.05m-12t/eager/expr-order-key/exec | 1.2549 | 1.2528 | 0.998 |  |
| 1.05m-12t/streaming/expr-order-key/exec | 1.2376 | 1.2707 | 1.027 |  |
| 1.05m-12t/eager/group-avg/exec | 4.0508 | 3.8866 | 0.959 |  |
| 1.05m-12t/streaming/group-avg/exec | 3.3589 | 3.4177 | 1.018 |  |
| 1.05m-12t/eager/knows-reach/exec | 11.7945 | 11.6403 | 0.987 |  |
| 1.05m-12t/streaming/knows-reach/exec | 11.5128 | 11.7048 | 1.017 |  |
| 1.05m-12t/eager/lang-filter/exec | 0.8024 | 0.7557 | 0.942 |  |
| 1.05m-12t/streaming/lang-filter/exec | 0.7663 | 0.8162 | 1.065 | slower |
| 1.05m-12t/eager/minus/exec | 0.4577 | 0.4354 | 0.951 |  |
| 1.05m-12t/streaming/minus/exec | 0.2853 | 0.2865 | 1.004 | slower, short |
| 1.05m-12t/eager/not-exists/exec | 0.8687 | 0.8329 | 0.959 |  |
| 1.05m-12t/streaming/not-exists/exec | 0.7965 | 0.8133 | 1.021 |  |
| 1.05m-12t/eager/optional-chain/exec | 2.1283 | 2.1405 | 1.006 |  |
| 1.05m-12t/streaming/optional-chain/exec | 2.1998 | 2.1728 | 0.988 |  |
| 1.05m-12t/eager/optional-count/exec | 0.575 | 0.4689 | 0.815 |  |
| 1.05m-12t/streaming/optional-count/exec | 0.5797 | 0.6016 | 1.038 |  |
| 1.05m-12t/eager/order-by-full/exec | 8.7521 | 9.4152 | 1.076 | slower |
| 1.05m-12t/streaming/order-by-full/exec | 9.1738 | 9.6813 | 1.055 | slower |
| 1.05m-12t/eager/path-plus/exec | 0.0183 | 0.0198 | 1.082 | slower, short |
| 1.05m-12t/streaming/path-plus/exec | 0.0208 | 0.0189 | 0.909 |  |
| 1.05m-12t/eager/predicate-counts/exec | 0.0069 | 0.0072 | 1.043 | slower, short |
| 1.05m-12t/streaming/predicate-counts/exec | 0.0107 | 0.0111 | 1.037 | slower, short |
| 1.05m-12t/eager/predicate-subjects/exec | 0.0102 | 0.0102 | 1.000 |  |
| 1.05m-12t/streaming/predicate-subjects/exec | 0.0155 | 0.0161 | 1.039 | slower, short |
| 1.05m-12t/eager/range-topk/exec | 0.5892 | 0.5704 | 0.968 |  |
| 1.05m-12t/streaming/range-topk/exec | 0.5799 | 0.5791 | 0.999 |  |
| 1.05m-12t/eager/regex-iri/exec | 1.0558 | 0.9741 | 0.923 |  |
| 1.05m-12t/streaming/regex-iri/exec | 1.0866 | 1.025 | 0.943 |  |
| 1.05m-12t/eager/star-join/exec | 1.6431 | 1.5399 | 0.937 |  |
| 1.05m-12t/streaming/star-join/exec | 1.66 | 1.6374 | 0.986 |  |
| 1.05m-12t/eager/star-lookup/exec | 0.0862 | 0.0869 | 1.008 | slower, short |
| 1.05m-12t/streaming/star-lookup/exec | 0.089 | 0.0918 | 1.031 | slower, short |
| 1.05m-12t/eager/subquery-agg/exec | 0.2505 | 0.2412 | 0.963 |  |
| 1.05m-12t/streaming/subquery-agg/exec | 0.2221 | 0.231 | 1.040 | slower, short |
| 1.05m-12t/eager/two-hop-count/exec | 1.2374 | 1.242 | 1.004 |  |
| 1.05m-12t/streaming/two-hop-count/exec | 1.2279 | 1.2518 | 1.019 |  |
| 1.05m-12t/eager/types-grouped/exec | 0.0119 | 0.0113 | 0.950 |  |
| 1.05m-12t/streaming/types-grouped/exec | 0.0189 | 0.0208 | 1.101 | slower, short |
| 1.05m-12t/eager/values-star/exec | 0.0213 | 0.0204 | 0.958 |  |
| 1.05m-12t/streaming/values-star/exec | 0.027 | 0.029 | 1.074 | slower, short |
| 10.5m-1t/eager/class-predicate-list/exec | 10.0573 | 10.0343 | 0.998 |  |
| 10.5m-1t/streaming/class-predicate-list/exec | 10.345 | 10.436 | 1.009 |  |
| 10.5m-1t/eager/class-predicates/exec | 9.9658 | 10.1323 | 1.017 |  |
| 10.5m-1t/streaming/class-predicates/exec | 10.3754 | 10.4014 | 1.003 |  |
| 10.5m-1t/eager/contains/exec | 24.7219 | 24.5739 | 0.994 |  |
| 10.5m-1t/streaming/contains/exec | 24.6633 | 24.593 | 0.997 |  |
| 10.5m-1t/eager/count-all/exec | 0.0059 | 0.0059 | 1.000 |  |
| 10.5m-1t/streaming/count-all/exec | 0.007 | 0.0074 | 1.057 | slower, short |
| 10.5m-1t/eager/distinct-join/exec | 39.112 | 38.4555 | 0.983 |  |
| 10.5m-1t/streaming/distinct-join/exec | 38.223 | 37.5512 | 0.982 |  |
| 10.5m-1t/eager/distinct-obj/exec | 0.0045 | 0.0042 | 0.933 |  |
| 10.5m-1t/streaming/distinct-obj/exec | 0.0052 | 0.0055 | 1.058 | slower, short |
| 10.5m-1t/eager/employee-docs/exec | 0.0495 | 0.0494 | 0.998 |  |
| 10.5m-1t/streaming/employee-docs/exec | 0.0533 | 0.0539 | 1.011 | slower, short |
| 10.5m-1t/eager/exists-join/exec | 8.5973 | 8.5598 | 0.996 |  |
| 10.5m-1t/streaming/exists-join/exec | 8.588 | 8.576 | 0.999 |  |
| 10.5m-1t/eager/export-500k/exec | 2.182 | 2.3891 | 1.095 | slower |
| 10.5m-1t/streaming/export-500k/exec | 0.4036 | 0.4176 | 1.035 |  |
| 10.5m-1t/eager/expr-agg-arg/exec | 28.0454 | 25.2713 | 0.901 |  |
| 10.5m-1t/streaming/expr-agg-arg/exec | 23.0515 | 22.7009 | 0.985 |  |
| 10.5m-1t/eager/expr-bind-group/exec | 5.6352 | 4.7988 | 0.852 |  |
| 10.5m-1t/streaming/expr-bind-group/exec | 3.2927 | 3.2089 | 0.975 |  |
| 10.5m-1t/eager/expr-order-key/exec | 11.057 | 11.0278 | 0.997 |  |
| 10.5m-1t/streaming/expr-order-key/exec | 10.7384 | 10.9184 | 1.017 |  |
| 10.5m-1t/eager/group-avg/exec | 44.5917 | 39.4715 | 0.885 |  |
| 10.5m-1t/streaming/group-avg/exec | 36.9266 | 37.9805 | 1.029 |  |
| 10.5m-1t/eager/knows-reach/exec | 139.016 | 136.81 | 0.984 |  |
| 10.5m-1t/streaming/knows-reach/exec | 136.442 | 137.127 | 1.005 |  |
| 10.5m-1t/eager/lang-filter/exec | 18.3818 | 17.5737 | 0.956 |  |
| 10.5m-1t/streaming/lang-filter/exec | 18.456 | 17.6311 | 0.955 |  |
| 10.5m-1t/eager/minus/exec | 4.2318 | 3.7737 | 0.892 |  |
| 10.5m-1t/streaming/minus/exec | 2.4642 | 2.4634 | 1.000 |  |
| 10.5m-1t/eager/not-exists/exec | 9.4508 | 8.9848 | 0.951 |  |
| 10.5m-1t/streaming/not-exists/exec | 8.7239 | 8.6244 | 0.989 |  |
| 10.5m-1t/eager/optional-chain/exec | 26.7278 | 26.2702 | 0.983 |  |
| 10.5m-1t/streaming/optional-chain/exec | 24.8735 | 25.1218 | 1.010 |  |
| 10.5m-1t/eager/optional-count/exec | 5.2754 | 4.5102 | 0.855 |  |
| 10.5m-1t/streaming/optional-count/exec | 5.275 | 5.3028 | 1.005 |  |
| 10.5m-1t/eager/order-by-full/exec | 514.678 | 518.709 | 1.008 |  |
| 10.5m-1t/streaming/order-by-full/exec | 518.713 | 520.744 | 1.004 |  |
| 10.5m-1t/eager/path-plus/exec | 0.0088 | 0.0102 | 1.159 | slower, short |
| 10.5m-1t/streaming/path-plus/exec | 0.0093 | 0.0098 | 1.054 | slower, short |
| 10.5m-1t/eager/predicate-counts/exec | 0.0052 | 0.0048 | 0.923 |  |
| 10.5m-1t/streaming/predicate-counts/exec | 0.0064 | 0.0072 | 1.125 | slower, short |
| 10.5m-1t/eager/predicate-subjects/exec | 0.007 | 0.007 | 1.000 |  |
| 10.5m-1t/streaming/predicate-subjects/exec | 0.0087 | 0.0094 | 1.080 | slower, short |
| 10.5m-1t/eager/range-topk/exec | 5.1506 | 5.1241 | 0.995 |  |
| 10.5m-1t/streaming/range-topk/exec | 5.0912 | 5.0589 | 0.994 |  |
| 10.5m-1t/eager/regex-iri/exec | 37.6599 | 37.7425 | 1.002 |  |
| 10.5m-1t/streaming/regex-iri/exec | 37.6355 | 37.7833 | 1.004 |  |
| 10.5m-1t/eager/star-join/exec | 14.0741 | 13.9379 | 0.990 |  |
| 10.5m-1t/streaming/star-join/exec | 14.8748 | 14.6403 | 0.984 |  |
| 10.5m-1t/eager/star-lookup/exec | 0.058 | 0.0582 | 1.003 | slower, short |
| 10.5m-1t/streaming/star-lookup/exec | 0.0607 | 0.0609 | 1.003 | slower, short |
| 10.5m-1t/eager/subquery-agg/exec | 1.697 | 1.6993 | 1.001 |  |
| 10.5m-1t/streaming/subquery-agg/exec | 1.6321 | 1.6437 | 1.007 |  |
| 10.5m-1t/eager/two-hop-count/exec | 27.6557 | 27.8177 | 1.006 |  |
| 10.5m-1t/streaming/two-hop-count/exec | 27.6555 | 27.8699 | 1.008 |  |
| 10.5m-1t/eager/types-grouped/exec | 0.0081 | 0.0082 | 1.012 | slower, short |
| 10.5m-1t/streaming/types-grouped/exec | 0.0128 | 0.0131 | 1.023 | slower, short |
| 10.5m-1t/eager/values-star/exec | 0.017 | 0.0169 | 0.994 |  |
| 10.5m-1t/streaming/values-star/exec | 0.0208 | 0.0214 | 1.029 | slower, short |
| 10.5m-12t/eager/class-predicate-list/exec | 10.187 | 10.4325 | 1.024 |  |
| 10.5m-12t/streaming/class-predicate-list/exec | 10.7466 | 10.6903 | 0.995 |  |
| 10.5m-12t/eager/class-predicates/exec | 10.1525 | 10.3289 | 1.017 |  |
| 10.5m-12t/streaming/class-predicates/exec | 10.6942 | 10.678 | 0.998 |  |
| 10.5m-12t/eager/contains/exec | 4.2729 | 4.2908 | 1.004 |  |
| 10.5m-12t/streaming/contains/exec | 4.2514 | 4.263 | 1.003 |  |
| 10.5m-12t/eager/count-all/exec | 0.0089 | 0.0089 | 1.000 |  |
| 10.5m-12t/streaming/count-all/exec | 0.0118 | 0.0114 | 0.966 |  |
| 10.5m-12t/eager/distinct-join/exec | 39.425 | 38.8905 | 0.986 |  |
| 10.5m-12t/streaming/distinct-join/exec | 39.1068 | 38.0947 | 0.974 |  |
| 10.5m-12t/eager/distinct-obj/exec | 0.0061 | 0.0069 | 1.131 | slower, short |
| 10.5m-12t/streaming/distinct-obj/exec | 0.0091 | 0.009 | 0.989 |  |
| 10.5m-12t/eager/employee-docs/exec | 0.0705 | 0.0686 | 0.973 |  |
| 10.5m-12t/streaming/employee-docs/exec | 0.0782 | 0.0793 | 1.014 | slower, short |
| 10.5m-12t/eager/exists-join/exec | 8.9409 | 9.0845 | 1.016 |  |
| 10.5m-12t/streaming/exists-join/exec | 8.9535 | 8.7666 | 0.979 |  |
| 10.5m-12t/eager/export-500k/exec | 2.3314 | 2.3064 | 0.989 |  |
| 10.5m-12t/streaming/export-500k/exec | 0.3858 | 0.3829 | 0.992 |  |
| 10.5m-12t/eager/expr-agg-arg/exec | 27.5282 | 23.9869 | 0.871 |  |
| 10.5m-12t/streaming/expr-agg-arg/exec | 23.4391 | 23.3457 | 0.996 |  |
| 10.5m-12t/eager/expr-bind-group/exec | 4.8567 | 3.6836 | 0.758 |  |
| 10.5m-12t/streaming/expr-bind-group/exec | 3.4508 | 3.379 | 0.979 |  |
| 10.5m-12t/eager/expr-order-key/exec | 10.167 | 10.0788 | 0.991 |  |
| 10.5m-12t/streaming/expr-order-key/exec | 11.0418 | 11.2747 | 1.021 |  |
| 10.5m-12t/eager/group-avg/exec | 45.628 | 40.3942 | 0.885 |  |
| 10.5m-12t/streaming/group-avg/exec | 37.877 | 39.0808 | 1.032 |  |
| 10.5m-12t/eager/knows-reach/exec | 141.198 | 141.859 | 1.005 |  |
| 10.5m-12t/streaming/knows-reach/exec | 139.67 | 140.383 | 1.005 |  |
| 10.5m-12t/eager/lang-filter/exec | 3.1255 | 3.047 | 0.975 |  |
| 10.5m-12t/streaming/lang-filter/exec | 3.1091 | 3.0595 | 0.984 |  |
| 10.5m-12t/eager/minus/exec | 4.3339 | 3.9065 | 0.901 |  |
| 10.5m-12t/streaming/minus/exec | 2.5813 | 2.6218 | 1.016 |  |
| 10.5m-12t/eager/not-exists/exec | 9.6533 | 9.183 | 0.951 |  |
| 10.5m-12t/streaming/not-exists/exec | 8.9508 | 9.1442 | 1.022 |  |
| 10.5m-12t/eager/optional-chain/exec | 26.8487 | 27.5346 | 1.026 |  |
| 10.5m-12t/streaming/optional-chain/exec | 26.8467 | 27.6282 | 1.029 |  |
| 10.5m-12t/eager/optional-count/exec | 5.4027 | 4.6455 | 0.860 |  |
| 10.5m-12t/streaming/optional-count/exec | 5.4838 | 5.5205 | 1.007 |  |
| 10.5m-12t/eager/order-by-full/exec | 121.73 | 146.426 | 1.203 | slower |
| 10.5m-12t/streaming/order-by-full/exec | 125.833 | 166.012 | 1.319 | slower |
| 10.5m-12t/eager/path-plus/exec | 0.014 | 0.0164 | 1.171 | slower, short |
| 10.5m-12t/streaming/path-plus/exec | 0.0145 | 0.0146 | 1.007 | slower, short |
| 10.5m-12t/eager/predicate-counts/exec | 0.0071 | 0.008 | 1.127 | slower, short |
| 10.5m-12t/streaming/predicate-counts/exec | 0.0114 | 0.0111 | 0.974 |  |
| 10.5m-12t/eager/predicate-subjects/exec | 0.0093 | 0.0109 | 1.172 | slower, short |
| 10.5m-12t/streaming/predicate-subjects/exec | 0.0163 | 0.0154 | 0.945 |  |
| 10.5m-12t/eager/range-topk/exec | 2.0682 | 2.0935 | 1.012 |  |
| 10.5m-12t/streaming/range-topk/exec | 2.0104 | 2.277 | 1.133 | slower |
| 10.5m-12t/eager/regex-iri/exec | 6.316 | 6.3188 | 1.000 |  |
| 10.5m-12t/streaming/regex-iri/exec | 6.3288 | 6.3301 | 1.000 |  |
| 10.5m-12t/eager/star-join/exec | 13.2739 | 13.2456 | 0.998 |  |
| 10.5m-12t/streaming/star-join/exec | 13.7042 | 13.7189 | 1.001 |  |
| 10.5m-12t/eager/star-lookup/exec | 0.0727 | 0.0723 | 0.994 |  |
| 10.5m-12t/streaming/star-lookup/exec | 0.084 | 0.0811 | 0.965 |  |
| 10.5m-12t/eager/subquery-agg/exec | 0.8068 | 0.783 | 0.971 |  |
| 10.5m-12t/streaming/subquery-agg/exec | 0.8455 | 0.7995 | 0.946 |  |
| 10.5m-12t/eager/two-hop-count/exec | 11.3696 | 11.3824 | 1.001 |  |
| 10.5m-12t/streaming/two-hop-count/exec | 11.1666 | 11.3629 | 1.018 |  |
| 10.5m-12t/eager/types-grouped/exec | 0.012 | 0.0126 | 1.050 | slower, short |
| 10.5m-12t/streaming/types-grouped/exec | 0.0195 | 0.0203 | 1.041 | slower, short |
| 10.5m-12t/eager/values-star/exec | 0.0219 | 0.0208 | 0.950 |  |
| 10.5m-12t/streaming/values-star/exec | 0.0289 | 0.0276 | 0.955 |  |

248 metrics compared, 43 flagged, 0 failed checks, 0 missing checks, 0 changed fingerprints.
