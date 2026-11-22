# executors gate: candidate against baseline

- baseline: host forge, commit e52937ca, started 2026-11-02T03:37:44Z
- candidate: host forge, commit 8baa5686, started 2026-11-18T09:27:30Z
- threshold: 5.0% slower, 0.0% below 0.3 ms
- compared although different: manifest build_config: '{"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{},"mbx_policy":"d8bf0b48d620f6bf62929d3db32bd253653505cdb33a5bd1efed1e79aaca05b4","profile":"release"}' against '{"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1},"sparkles-owl":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{"CARGO_INCREMENTAL":"0"},"mbx_policy":"779a99e845a5ac3f1c4d64a3c95bcc74b33f47418e829bf008542ec2dbbcd151","profile":"release"}'

| metric | baseline | candidate | ratio | flag |
|---|--:|--:|--:|---|
| 1.05m-1t/eager/class-predicate-list/exec | 0.9288 | 0.9286 | 1.000 |  |
| 1.05m-1t/streaming/class-predicate-list/exec | 0.9166 | 0.9251 | 1.009 |  |
| 1.05m-1t/eager/class-predicates/exec | 0.932 | 0.9376 | 1.006 |  |
| 1.05m-1t/streaming/class-predicates/exec | 0.9294 | 0.9378 | 1.009 |  |
| 1.05m-1t/eager/contains/exec | 2.4102 | 2.5478 | 1.057 | slower |
| 1.05m-1t/streaming/contains/exec | 2.4732 | 2.5484 | 1.030 |  |
| 1.05m-1t/eager/count-all/exec | 0.0047 | 0.0047 | 1.000 |  |
| 1.05m-1t/streaming/count-all/exec | 0.0055 | 0.0065 | 1.182 | slower, short |
| 1.05m-1t/eager/distinct-join/exec | 3.4881 | 3.5928 | 1.030 |  |
| 1.05m-1t/streaming/distinct-join/exec | 3.6425 | 3.8374 | 1.054 | slower |
| 1.05m-1t/eager/distinct-obj/exec | 0.0037 | 0.0039 | 1.054 | slower, short |
| 1.05m-1t/streaming/distinct-obj/exec | 0.005 | 0.0052 | 1.040 | slower, short |
| 1.05m-1t/eager/employee-docs/exec | 0.0361 | 0.0367 | 1.017 | slower, short |
| 1.05m-1t/streaming/employee-docs/exec | 0.0404 | 0.0397 | 0.983 |  |
| 1.05m-1t/eager/exists-join/exec | 1.0253 | 1.0493 | 1.023 |  |
| 1.05m-1t/streaming/exists-join/exec | 1.0387 | 1.0432 | 1.004 |  |
| 1.05m-1t/eager/export-500k/exec | 2.2474 | 2.0962 | 0.933 |  |
| 1.05m-1t/streaming/export-500k/exec | 0.4282 | 0.3945 | 0.921 |  |
| 1.05m-1t/eager/expr-agg-arg/exec | 2.5477 | 2.5452 | 0.999 |  |
| 1.05m-1t/streaming/expr-agg-arg/exec | 1.8986 | 1.9221 | 1.012 |  |
| 1.05m-1t/eager/expr-bind-group/exec | 0.5931 | 0.601 | 1.013 |  |
| 1.05m-1t/streaming/expr-bind-group/exec | 0.3687 | 0.3705 | 1.005 |  |
| 1.05m-1t/eager/expr-order-key/exec | 1.1837 | 1.1594 | 0.979 |  |
| 1.05m-1t/streaming/expr-order-key/exec | 1.1107 | 1.1466 | 1.032 |  |
| 1.05m-1t/eager/group-avg/exec | 4.0122 | 3.8903 | 0.970 |  |
| 1.05m-1t/streaming/group-avg/exec | 3.4463 | 3.1995 | 0.928 |  |
| 1.05m-1t/eager/knows-reach/exec | 11.5309 | 11.4978 | 0.997 |  |
| 1.05m-1t/streaming/knows-reach/exec | 11.3615 | 11.3287 | 0.997 |  |
| 1.05m-1t/eager/lang-filter/exec | 1.606 | 1.8326 | 1.141 | slower |
| 1.05m-1t/streaming/lang-filter/exec | 1.6268 | 1.8391 | 1.131 | slower |
| 1.05m-1t/eager/minus/exec | 0.3034 | 0.4119 | 1.358 | slower |
| 1.05m-1t/streaming/minus/exec | 0.2586 | 0.2617 | 1.012 | slower, short |
| 1.05m-1t/eager/not-exists/exec | 0.7992 | 0.806 | 1.009 |  |
| 1.05m-1t/streaming/not-exists/exec | 0.7498 | 0.7415 | 0.989 |  |
| 1.05m-1t/eager/optional-chain/exec | 1.9906 | 2.0171 | 1.013 |  |
| 1.05m-1t/streaming/optional-chain/exec | 1.9686 | 2.0606 | 1.047 |  |
| 1.05m-1t/eager/optional-count/exec | 0.4879 | 0.4841 | 0.992 |  |
| 1.05m-1t/streaming/optional-count/exec | 0.5188 | 0.5438 | 1.048 |  |
| 1.05m-1t/eager/order-by-full/exec | 48.8854 | 41.0957 | 0.841 |  |
| 1.05m-1t/streaming/order-by-full/exec | 45.1315 | 41.5359 | 0.920 |  |
| 1.05m-1t/eager/path-plus/exec | 0.0108 | 0.0112 | 1.037 | slower, short |
| 1.05m-1t/streaming/path-plus/exec | 0.0121 | 0.0133 | 1.099 | slower, short |
| 1.05m-1t/eager/predicate-counts/exec | 0.0046 | 0.0045 | 0.978 |  |
| 1.05m-1t/streaming/predicate-counts/exec | 0.0061 | 0.0069 | 1.131 | slower, short |
| 1.05m-1t/eager/predicate-subjects/exec | 0.0068 | 0.0068 | 1.000 |  |
| 1.05m-1t/streaming/predicate-subjects/exec | 0.0082 | 0.0093 | 1.134 | slower, short |
| 1.05m-1t/eager/range-topk/exec | 0.6636 | 0.5427 | 0.818 |  |
| 1.05m-1t/streaming/range-topk/exec | 0.5285 | 0.5369 | 1.016 |  |
| 1.05m-1t/eager/regex-iri/exec | 3.5426 | 3.8049 | 1.074 | slower |
| 1.05m-1t/streaming/regex-iri/exec | 3.6781 | 3.7996 | 1.033 |  |
| 1.05m-1t/eager/star-join/exec | 1.3829 | 1.4137 | 1.022 |  |
| 1.05m-1t/streaming/star-join/exec | 1.4227 | 1.4395 | 1.012 |  |
| 1.05m-1t/eager/star-lookup/exec | 0.0608 | 0.0621 | 1.021 | slower, short |
| 1.05m-1t/streaming/star-lookup/exec | 0.0659 | 0.0645 | 0.979 |  |
| 1.05m-1t/eager/subquery-agg/exec | 0.188 | 0.1862 | 0.990 |  |
| 1.05m-1t/streaming/subquery-agg/exec | 0.183 | 0.1836 | 1.003 | slower, short |
| 1.05m-1t/eager/two-hop-count/exec | 2.62 | 2.6222 | 1.001 |  |
| 1.05m-1t/streaming/two-hop-count/exec | 2.6149 | 2.6159 | 1.000 |  |
| 1.05m-1t/eager/types-grouped/exec | 0.0074 | 0.0081 | 1.095 | slower, short |
| 1.05m-1t/streaming/types-grouped/exec | 0.0124 | 0.0128 | 1.032 | slower, short |
| 1.05m-1t/eager/values-star/exec | 0.0148 | 0.0152 | 1.027 | slower, short |
| 1.05m-1t/streaming/values-star/exec | 0.0189 | 0.02 | 1.058 | slower, short |
| 1.05m-12t/eager/class-predicate-list/exec | 0.973 | 0.9861 | 1.013 |  |
| 1.05m-12t/streaming/class-predicate-list/exec | 1.005 | 1.0122 | 1.007 |  |
| 1.05m-12t/eager/class-predicates/exec | 0.9814 | 0.9958 | 1.015 |  |
| 1.05m-12t/streaming/class-predicates/exec | 1.0255 | 1.0197 | 0.994 |  |
| 1.05m-12t/eager/contains/exec | 0.7176 | 0.7585 | 1.057 | slower |
| 1.05m-12t/streaming/contains/exec | 0.7499 | 0.7729 | 1.031 |  |
| 1.05m-12t/eager/count-all/exec | 0.0074 | 0.008 | 1.081 | slower, short |
| 1.05m-12t/streaming/count-all/exec | 0.0105 | 0.0103 | 0.981 |  |
| 1.05m-12t/eager/distinct-join/exec | 3.5292 | 3.7598 | 1.065 | slower |
| 1.05m-12t/streaming/distinct-join/exec | 3.8761 | 4.015 | 1.036 |  |
| 1.05m-12t/eager/distinct-obj/exec | 0.006 | 0.0068 | 1.133 | slower, short |
| 1.05m-12t/streaming/distinct-obj/exec | 0.0091 | 0.0087 | 0.956 |  |
| 1.05m-12t/eager/employee-docs/exec | 0.0494 | 0.0505 | 1.022 | slower, short |
| 1.05m-12t/streaming/employee-docs/exec | 0.0566 | 0.0542 | 0.958 |  |
| 1.05m-12t/eager/exists-join/exec | 1.0915 | 1.1391 | 1.044 |  |
| 1.05m-12t/streaming/exists-join/exec | 1.1348 | 1.1496 | 1.013 |  |
| 1.05m-12t/eager/export-500k/exec | 2.4226 | 2.3889 | 0.986 |  |
| 1.05m-12t/streaming/export-500k/exec | 0.4075 | 0.3735 | 0.917 |  |
| 1.05m-12t/eager/expr-agg-arg/exec | 3.0024 | 2.9012 | 0.966 |  |
| 1.05m-12t/streaming/expr-agg-arg/exec | 2.0102 | 2.0462 | 1.018 |  |
| 1.05m-12t/eager/expr-bind-group/exec | 0.8553 | 0.7623 | 0.891 |  |
| 1.05m-12t/streaming/expr-bind-group/exec | 0.3928 | 0.3979 | 1.013 |  |
| 1.05m-12t/eager/expr-order-key/exec | 1.2888 | 1.2422 | 0.964 |  |
| 1.05m-12t/streaming/expr-order-key/exec | 1.2076 | 1.2247 | 1.014 |  |
| 1.05m-12t/eager/group-avg/exec | 4.4798 | 4.0346 | 0.901 |  |
| 1.05m-12t/streaming/group-avg/exec | 3.5906 | 3.3616 | 0.936 |  |
| 1.05m-12t/eager/knows-reach/exec | 11.8375 | 11.8109 | 0.998 |  |
| 1.05m-12t/streaming/knows-reach/exec | 11.688 | 11.6777 | 0.999 |  |
| 1.05m-12t/eager/lang-filter/exec | 0.705 | 0.7779 | 1.103 | slower |
| 1.05m-12t/streaming/lang-filter/exec | 0.7477 | 0.8097 | 1.083 | slower |
| 1.05m-12t/eager/minus/exec | 0.3522 | 0.4813 | 1.367 | slower |
| 1.05m-12t/streaming/minus/exec | 0.2815 | 0.2805 | 0.996 |  |
| 1.05m-12t/eager/not-exists/exec | 0.8651 | 0.8673 | 1.003 |  |
| 1.05m-12t/streaming/not-exists/exec | 0.7986 | 0.7928 | 0.993 |  |
| 1.05m-12t/eager/optional-chain/exec | 2.1357 | 2.1128 | 0.989 |  |
| 1.05m-12t/streaming/optional-chain/exec | 2.0777 | 2.2341 | 1.075 | slower |
| 1.05m-12t/eager/optional-count/exec | 0.5425 | 0.5478 | 1.010 |  |
| 1.05m-12t/streaming/optional-count/exec | 0.5556 | 0.5751 | 1.035 |  |
| 1.05m-12t/eager/order-by-full/exec | 14.0955 | 8.7682 | 0.622 |  |
| 1.05m-12t/streaming/order-by-full/exec | 15.7521 | 9.0326 | 0.573 |  |
| 1.05m-12t/eager/path-plus/exec | 0.018 | 0.0175 | 0.972 |  |
| 1.05m-12t/streaming/path-plus/exec | 0.0224 | 0.0201 | 0.897 |  |
| 1.05m-12t/eager/predicate-counts/exec | 0.0068 | 0.0074 | 1.088 | slower, short |
| 1.05m-12t/streaming/predicate-counts/exec | 0.011 | 0.0108 | 0.982 |  |
| 1.05m-12t/eager/predicate-subjects/exec | 0.0095 | 0.0083 | 0.874 |  |
| 1.05m-12t/streaming/predicate-subjects/exec | 0.0154 | 0.0148 | 0.961 |  |
| 1.05m-12t/eager/range-topk/exec | 0.7128 | 0.582 | 0.816 |  |
| 1.05m-12t/streaming/range-topk/exec | 0.5688 | 0.5746 | 1.010 |  |
| 1.05m-12t/eager/regex-iri/exec | 0.9223 | 1.0236 | 1.110 | slower |
| 1.05m-12t/streaming/regex-iri/exec | 0.9973 | 1.017 | 1.020 |  |
| 1.05m-12t/eager/star-join/exec | 1.5408 | 1.5913 | 1.033 |  |
| 1.05m-12t/streaming/star-join/exec | 1.6506 | 1.6511 | 1.000 |  |
| 1.05m-12t/eager/star-lookup/exec | 0.0837 | 0.0831 | 0.993 |  |
| 1.05m-12t/streaming/star-lookup/exec | 0.0886 | 0.0933 | 1.053 | slower, short |
| 1.05m-12t/eager/subquery-agg/exec | 0.2389 | 0.2184 | 0.914 |  |
| 1.05m-12t/streaming/subquery-agg/exec | 0.2225 | 0.2094 | 0.941 |  |
| 1.05m-12t/eager/two-hop-count/exec | 1.2677 | 1.2316 | 0.972 |  |
| 1.05m-12t/streaming/two-hop-count/exec | 1.2492 | 1.2287 | 0.984 |  |
| 1.05m-12t/eager/types-grouped/exec | 0.0115 | 0.0122 | 1.061 | slower, short |
| 1.05m-12t/streaming/types-grouped/exec | 0.0193 | 0.0209 | 1.083 | slower, short |
| 1.05m-12t/eager/values-star/exec | 0.0205 | 0.021 | 1.024 | slower, short |
| 1.05m-12t/streaming/values-star/exec | 0.0262 | 0.0313 | 1.195 | slower, short |
| 10.5m-1t/eager/class-predicate-list/exec | 9.8319 | 10.1028 | 1.028 |  |
| 10.5m-1t/streaming/class-predicate-list/exec | 10.2733 | 10.3444 | 1.007 |  |
| 10.5m-1t/eager/class-predicates/exec | 9.8483 | 10.0737 | 1.023 |  |
| 10.5m-1t/streaming/class-predicates/exec | 10.2449 | 10.335 | 1.009 |  |
| 10.5m-1t/eager/contains/exec | 23.4661 | 24.6489 | 1.050 | slower |
| 10.5m-1t/streaming/contains/exec | 24.0445 | 24.6929 | 1.027 |  |
| 10.5m-1t/eager/count-all/exec | 0.0058 | 0.0063 | 1.086 | slower, short |
| 10.5m-1t/streaming/count-all/exec | 0.0068 | 0.0076 | 1.118 | slower, short |
| 10.5m-1t/eager/distinct-join/exec | 38.2491 | 39.1584 | 1.024 |  |
| 10.5m-1t/streaming/distinct-join/exec | 37.9224 | 38.2143 | 1.008 |  |
| 10.5m-1t/eager/distinct-obj/exec | 0.0039 | 0.0043 | 1.103 | slower, short |
| 10.5m-1t/streaming/distinct-obj/exec | 0.005 | 0.0054 | 1.080 | slower, short |
| 10.5m-1t/eager/employee-docs/exec | 0.0482 | 0.05 | 1.037 | slower, short |
| 10.5m-1t/streaming/employee-docs/exec | 0.0535 | 0.0538 | 1.006 | slower, short |
| 10.5m-1t/eager/exists-join/exec | 8.5635 | 8.6015 | 1.004 |  |
| 10.5m-1t/streaming/exists-join/exec | 8.5601 | 8.6288 | 1.008 |  |
| 10.5m-1t/eager/export-500k/exec | 2.2178 | 2.3207 | 1.046 |  |
| 10.5m-1t/streaming/export-500k/exec | 0.4425 | 0.3997 | 0.903 |  |
| 10.5m-1t/eager/expr-agg-arg/exec | 29.4458 | 28.5302 | 0.969 |  |
| 10.5m-1t/streaming/expr-agg-arg/exec | 23.0005 | 23.1268 | 1.005 |  |
| 10.5m-1t/eager/expr-bind-group/exec | 5.5701 | 5.7654 | 1.035 |  |
| 10.5m-1t/streaming/expr-bind-group/exec | 3.2995 | 3.2952 | 0.999 |  |
| 10.5m-1t/eager/expr-order-key/exec | 11.8812 | 11.0564 | 0.931 |  |
| 10.5m-1t/streaming/expr-order-key/exec | 10.5345 | 10.6506 | 1.011 |  |
| 10.5m-1t/eager/group-avg/exec | 47.5151 | 45.2064 | 0.951 |  |
| 10.5m-1t/streaming/group-avg/exec | 39.3425 | 37.2281 | 0.946 |  |
| 10.5m-1t/eager/knows-reach/exec | 141.565 | 140.011 | 0.989 |  |
| 10.5m-1t/streaming/knows-reach/exec | 137.47 | 137.307 | 0.999 |  |
| 10.5m-1t/eager/lang-filter/exec | 15.6537 | 18.0412 | 1.153 | slower |
| 10.5m-1t/streaming/lang-filter/exec | 15.9092 | 18.0744 | 1.136 | slower |
| 10.5m-1t/eager/minus/exec | 3.1705 | 4.3002 | 1.356 | slower |
| 10.5m-1t/streaming/minus/exec | 2.4382 | 2.4674 | 1.012 |  |
| 10.5m-1t/eager/not-exists/exec | 9.7704 | 9.477 | 0.970 |  |
| 10.5m-1t/streaming/not-exists/exec | 8.8687 | 8.737 | 0.985 |  |
| 10.5m-1t/eager/optional-chain/exec | 29.844 | 26.9698 | 0.904 |  |
| 10.5m-1t/streaming/optional-chain/exec | 22.8527 | 25.3125 | 1.108 | slower |
| 10.5m-1t/eager/optional-count/exec | 5.3228 | 5.3051 | 0.997 |  |
| 10.5m-1t/streaming/optional-count/exec | 5.0155 | 5.2789 | 1.053 | slower |
| 10.5m-1t/eager/order-by-full/exec | 677.315 | 517.2 | 0.764 |  |
| 10.5m-1t/streaming/order-by-full/exec | 596.671 | 519.819 | 0.871 |  |
| 10.5m-1t/eager/path-plus/exec | 0.0089 | 0.0089 | 1.000 |  |
| 10.5m-1t/streaming/path-plus/exec | 0.0091 | 0.0094 | 1.033 | slower, short |
| 10.5m-1t/eager/predicate-counts/exec | 0.0047 | 0.0046 | 0.979 |  |
| 10.5m-1t/streaming/predicate-counts/exec | 0.0064 | 0.0069 | 1.078 | slower, short |
| 10.5m-1t/eager/predicate-subjects/exec | 0.0063 | 0.0069 | 1.095 | slower, short |
| 10.5m-1t/streaming/predicate-subjects/exec | 0.0085 | 0.009 | 1.059 | slower, short |
| 10.5m-1t/eager/range-topk/exec | 5.8508 | 5.1719 | 0.884 |  |
| 10.5m-1t/streaming/range-topk/exec | 5.0948 | 5.0924 | 1.000 |  |
| 10.5m-1t/eager/regex-iri/exec | 35.5455 | 38.1641 | 1.074 | slower |
| 10.5m-1t/streaming/regex-iri/exec | 36.7402 | 38.1572 | 1.039 |  |
| 10.5m-1t/eager/star-join/exec | 13.8913 | 14.1484 | 1.019 |  |
| 10.5m-1t/streaming/star-join/exec | 14.5118 | 14.8431 | 1.023 |  |
| 10.5m-1t/eager/star-lookup/exec | 0.0556 | 0.0577 | 1.038 | slower, short |
| 10.5m-1t/streaming/star-lookup/exec | 0.0605 | 0.0583 | 0.964 |  |
| 10.5m-1t/eager/subquery-agg/exec | 1.6531 | 1.6937 | 1.025 |  |
| 10.5m-1t/streaming/subquery-agg/exec | 1.6389 | 1.6263 | 0.992 |  |
| 10.5m-1t/eager/two-hop-count/exec | 27.6608 | 27.585 | 0.997 |  |
| 10.5m-1t/streaming/two-hop-count/exec | 27.7318 | 27.5679 | 0.994 |  |
| 10.5m-1t/eager/types-grouped/exec | 0.0074 | 0.0083 | 1.122 | slower, short |
| 10.5m-1t/streaming/types-grouped/exec | 0.0134 | 0.0128 | 0.955 |  |
| 10.5m-1t/eager/values-star/exec | 0.0168 | 0.0162 | 0.964 |  |
| 10.5m-1t/streaming/values-star/exec | 0.0196 | 0.0198 | 1.010 | slower, short |
| 10.5m-12t/eager/class-predicate-list/exec | 10.178 | 10.4838 | 1.030 |  |
| 10.5m-12t/streaming/class-predicate-list/exec | 10.4968 | 10.6001 | 1.010 |  |
| 10.5m-12t/eager/class-predicates/exec | 10.1159 | 10.2841 | 1.017 |  |
| 10.5m-12t/streaming/class-predicates/exec | 10.5169 | 10.7224 | 1.020 |  |
| 10.5m-12t/eager/contains/exec | 4.004 | 4.2557 | 1.063 | slower |
| 10.5m-12t/streaming/contains/exec | 4.0798 | 4.2746 | 1.048 |  |
| 10.5m-12t/eager/count-all/exec | 0.008 | 0.0086 | 1.075 | slower, short |
| 10.5m-12t/streaming/count-all/exec | 0.0119 | 0.0114 | 0.958 |  |
| 10.5m-12t/eager/distinct-join/exec | 38.5196 | 39.2794 | 1.020 |  |
| 10.5m-12t/streaming/distinct-join/exec | 38.051 | 39.0919 | 1.027 |  |
| 10.5m-12t/eager/distinct-obj/exec | 0.0058 | 0.0067 | 1.155 | slower, short |
| 10.5m-12t/streaming/distinct-obj/exec | 0.0093 | 0.0095 | 1.022 | slower, short |
| 10.5m-12t/eager/employee-docs/exec | 0.0678 | 0.0686 | 1.012 | slower, short |
| 10.5m-12t/streaming/employee-docs/exec | 0.0789 | 0.0766 | 0.971 |  |
| 10.5m-12t/eager/exists-join/exec | 8.852 | 8.9501 | 1.011 |  |
| 10.5m-12t/streaming/exists-join/exec | 8.8247 | 8.9148 | 1.010 |  |
| 10.5m-12t/eager/export-500k/exec | 2.4799 | 2.198 | 0.886 |  |
| 10.5m-12t/streaming/export-500k/exec | 0.4091 | 0.3828 | 0.936 |  |
| 10.5m-12t/eager/expr-agg-arg/exec | 28.0343 | 27.6744 | 0.987 |  |
| 10.5m-12t/streaming/expr-agg-arg/exec | 23.2254 | 23.4144 | 1.008 |  |
| 10.5m-12t/eager/expr-bind-group/exec | 4.8124 | 4.7512 | 0.987 |  |
| 10.5m-12t/streaming/expr-bind-group/exec | 3.4362 | 3.413 | 0.993 |  |
| 10.5m-12t/eager/expr-order-key/exec | 10.3228 | 10.1028 | 0.979 |  |
| 10.5m-12t/streaming/expr-order-key/exec | 10.7844 | 10.9592 | 1.016 |  |
| 10.5m-12t/eager/group-avg/exec | 47.8198 | 45.4409 | 0.950 |  |
| 10.5m-12t/streaming/group-avg/exec | 40.0891 | 37.7191 | 0.941 |  |
| 10.5m-12t/eager/knows-reach/exec | 142.627 | 141.111 | 0.989 |  |
| 10.5m-12t/streaming/knows-reach/exec | 140.373 | 139.26 | 0.992 |  |
| 10.5m-12t/eager/lang-filter/exec | 2.8064 | 3.1237 | 1.113 | slower |
| 10.5m-12t/streaming/lang-filter/exec | 2.8694 | 3.0565 | 1.065 | slower |
| 10.5m-12t/eager/minus/exec | 3.238 | 4.3026 | 1.329 | slower |
| 10.5m-12t/streaming/minus/exec | 2.6027 | 2.5863 | 0.994 |  |
| 10.5m-12t/eager/not-exists/exec | 9.7976 | 9.755 | 0.996 |  |
| 10.5m-12t/streaming/not-exists/exec | 8.9343 | 8.9493 | 1.002 |  |
| 10.5m-12t/eager/optional-chain/exec | 28.4546 | 25.1457 | 0.884 |  |
| 10.5m-12t/streaming/optional-chain/exec | 25.5174 | 26.5077 | 1.039 |  |
| 10.5m-12t/eager/optional-count/exec | 5.3987 | 5.4023 | 1.001 |  |
| 10.5m-12t/streaming/optional-count/exec | 5.22 | 5.4524 | 1.045 |  |
| 10.5m-12t/eager/order-by-full/exec | 239.272 | 108.713 | 0.454 |  |
| 10.5m-12t/streaming/order-by-full/exec | 266.403 | 107.363 | 0.403 |  |
| 10.5m-12t/eager/path-plus/exec | 0.0137 | 0.0125 | 0.912 |  |
| 10.5m-12t/streaming/path-plus/exec | 0.0142 | 0.0153 | 1.077 | slower, short |
| 10.5m-12t/eager/predicate-counts/exec | 0.0068 | 0.0067 | 0.985 |  |
| 10.5m-12t/streaming/predicate-counts/exec | 0.0111 | 0.0109 | 0.982 |  |
| 10.5m-12t/eager/predicate-subjects/exec | 0.0079 | 0.0091 | 1.152 | slower, short |
| 10.5m-12t/streaming/predicate-subjects/exec | 0.0157 | 0.0167 | 1.064 | slower, short |
| 10.5m-12t/eager/range-topk/exec | 2.456 | 2.1106 | 0.859 |  |
| 10.5m-12t/streaming/range-topk/exec | 2.2472 | 2.0712 | 0.922 |  |
| 10.5m-12t/eager/regex-iri/exec | 5.9913 | 6.4991 | 1.085 | slower |
| 10.5m-12t/streaming/regex-iri/exec | 6.2395 | 6.503 | 1.042 |  |
| 10.5m-12t/eager/star-join/exec | 13.0634 | 13.3085 | 1.019 |  |
| 10.5m-12t/streaming/star-join/exec | 13.5013 | 13.9195 | 1.031 |  |
| 10.5m-12t/eager/star-lookup/exec | 0.0725 | 0.0734 | 1.012 | slower, short |
| 10.5m-12t/streaming/star-lookup/exec | 0.082 | 0.0842 | 1.027 | slower, short |
| 10.5m-12t/eager/subquery-agg/exec | 0.7566 | 0.8224 | 1.087 | slower |
| 10.5m-12t/streaming/subquery-agg/exec | 0.83 | 0.7732 | 0.932 |  |
| 10.5m-12t/eager/two-hop-count/exec | 11.1937 | 11.3129 | 1.011 |  |
| 10.5m-12t/streaming/two-hop-count/exec | 11.2398 | 11.1765 | 0.994 |  |
| 10.5m-12t/eager/types-grouped/exec | 0.0105 | 0.0122 | 1.162 | slower, short |
| 10.5m-12t/streaming/types-grouped/exec | 0.0203 | 0.0201 | 0.990 |  |
| 10.5m-12t/eager/values-star/exec | 0.0202 | 0.0207 | 1.025 | slower, short |
| 10.5m-12t/streaming/values-star/exec | 0.0266 | 0.0275 | 1.034 | slower, short |

248 metrics compared, 75 flagged, 0 failed checks, 0 missing checks, 0 changed fingerprints.
