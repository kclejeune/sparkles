# executors gate: candidate against baseline

- baseline: host forge, commit e52937ca, started 2026-11-02T03:37:44Z
- candidate: host forge, commit cc94ac7f, started 2026-11-18T07:04:20Z
- threshold: 5.0% slower, 0.0% below 0.3 ms
- compared although different: manifest build_config: '{"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{},"mbx_policy":"d8bf0b48d620f6bf62929d3db32bd253653505cdb33a5bd1efed1e79aaca05b4","profile":"release"}' against '{"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1},"sparkles-owl":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{"CARGO_INCREMENTAL":"0"},"mbx_policy":"779a99e845a5ac3f1c4d64a3c95bcc74b33f47418e829bf008542ec2dbbcd151","profile":"release"}'

| metric | baseline | candidate | ratio | flag |
|---|--:|--:|--:|---|
| 1.05m-1t/eager/class-predicate-list/exec | 0.9288 | 0.9277 | 0.999 |  |
| 1.05m-1t/streaming/class-predicate-list/exec | 0.9166 | 0.9164 | 1.000 |  |
| 1.05m-1t/eager/class-predicates/exec | 0.932 | 0.9344 | 1.003 |  |
| 1.05m-1t/streaming/class-predicates/exec | 0.9294 | 0.9288 | 0.999 |  |
| 1.05m-1t/eager/contains/exec | 2.4102 | 2.5103 | 1.042 |  |
| 1.05m-1t/streaming/contains/exec | 2.4732 | 2.5137 | 1.016 |  |
| 1.05m-1t/eager/count-all/exec | 0.0047 | 0.0049 | 1.043 | slower, short |
| 1.05m-1t/streaming/count-all/exec | 0.0055 | 0.0055 | 1.000 |  |
| 1.05m-1t/eager/distinct-join/exec | 3.4881 | 3.4591 | 0.992 |  |
| 1.05m-1t/streaming/distinct-join/exec | 3.6425 | 3.7005 | 1.016 |  |
| 1.05m-1t/eager/distinct-obj/exec | 0.0037 | 0.0043 | 1.162 | slower, short |
| 1.05m-1t/streaming/distinct-obj/exec | 0.005 | 0.0048 | 0.960 |  |
| 1.05m-1t/eager/employee-docs/exec | 0.0361 | 0.0378 | 1.047 | slower, short |
| 1.05m-1t/streaming/employee-docs/exec | 0.0404 | 0.038 | 0.941 |  |
| 1.05m-1t/eager/exists-join/exec | 1.0253 | 1.0268 | 1.001 |  |
| 1.05m-1t/streaming/exists-join/exec | 1.0387 | 1.032 | 0.994 |  |
| 1.05m-1t/eager/export-500k/exec | 2.2474 | 2.1529 | 0.958 |  |
| 1.05m-1t/streaming/export-500k/exec | 0.4282 | 0.4137 | 0.966 |  |
| 1.05m-1t/eager/expr-agg-arg/exec | 2.5477 | 2.5984 | 1.020 |  |
| 1.05m-1t/streaming/expr-agg-arg/exec | 1.8986 | 1.8951 | 0.998 |  |
| 1.05m-1t/eager/expr-bind-group/exec | 0.5931 | 0.6045 | 1.019 |  |
| 1.05m-1t/streaming/expr-bind-group/exec | 0.3687 | 0.3711 | 1.007 |  |
| 1.05m-1t/eager/expr-order-key/exec | 1.1837 | 1.1463 | 0.968 |  |
| 1.05m-1t/streaming/expr-order-key/exec | 1.1107 | 1.1418 | 1.028 |  |
| 1.05m-1t/eager/group-avg/exec | 4.0122 | 3.9521 | 0.985 |  |
| 1.05m-1t/streaming/group-avg/exec | 3.4463 | 3.1703 | 0.920 |  |
| 1.05m-1t/eager/knows-reach/exec | 11.5309 | 11.514 | 0.999 |  |
| 1.05m-1t/streaming/knows-reach/exec | 11.3615 | 11.3391 | 0.998 |  |
| 1.05m-1t/eager/lang-filter/exec | 1.606 | 1.6404 | 1.021 |  |
| 1.05m-1t/streaming/lang-filter/exec | 1.6268 | 1.6413 | 1.009 |  |
| 1.05m-1t/eager/minus/exec | 0.3034 | 0.3123 | 1.029 |  |
| 1.05m-1t/streaming/minus/exec | 0.2586 | 0.2571 | 0.994 |  |
| 1.05m-1t/eager/not-exists/exec | 0.7992 | 0.7998 | 1.001 |  |
| 1.05m-1t/streaming/not-exists/exec | 0.7498 | 0.74 | 0.987 |  |
| 1.05m-1t/eager/optional-chain/exec | 1.9906 | 2.0023 | 1.006 |  |
| 1.05m-1t/streaming/optional-chain/exec | 1.9686 | 1.951 | 0.991 |  |
| 1.05m-1t/eager/optional-count/exec | 0.4879 | 0.4877 | 1.000 |  |
| 1.05m-1t/streaming/optional-count/exec | 0.5188 | 0.5162 | 0.995 |  |
| 1.05m-1t/eager/order-by-full/exec | 48.8854 | 41.1682 | 0.842 |  |
| 1.05m-1t/streaming/order-by-full/exec | 45.1315 | 41.5046 | 0.920 |  |
| 1.05m-1t/eager/path-plus/exec | 0.0108 | 0.012 | 1.111 | slower, short |
| 1.05m-1t/streaming/path-plus/exec | 0.0121 | 0.0123 | 1.017 | slower, short |
| 1.05m-1t/eager/predicate-counts/exec | 0.0046 | 0.0052 | 1.130 | slower, short |
| 1.05m-1t/streaming/predicate-counts/exec | 0.0061 | 0.0064 | 1.049 | slower, short |
| 1.05m-1t/eager/predicate-subjects/exec | 0.0068 | 0.0072 | 1.059 | slower, short |
| 1.05m-1t/streaming/predicate-subjects/exec | 0.0082 | 0.0088 | 1.073 | slower, short |
| 1.05m-1t/eager/range-topk/exec | 0.6636 | 0.5352 | 0.807 |  |
| 1.05m-1t/streaming/range-topk/exec | 0.5285 | 0.5288 | 1.001 |  |
| 1.05m-1t/eager/regex-iri/exec | 3.5426 | 3.4805 | 0.982 |  |
| 1.05m-1t/streaming/regex-iri/exec | 3.6781 | 3.4798 | 0.946 |  |
| 1.05m-1t/eager/star-join/exec | 1.3829 | 1.4114 | 1.021 |  |
| 1.05m-1t/streaming/star-join/exec | 1.4227 | 1.4248 | 1.001 |  |
| 1.05m-1t/eager/star-lookup/exec | 0.0608 | 0.0633 | 1.041 | slower, short |
| 1.05m-1t/streaming/star-lookup/exec | 0.0659 | 0.0641 | 0.973 |  |
| 1.05m-1t/eager/subquery-agg/exec | 0.188 | 0.1866 | 0.993 |  |
| 1.05m-1t/streaming/subquery-agg/exec | 0.183 | 0.1859 | 1.016 | slower, short |
| 1.05m-1t/eager/two-hop-count/exec | 2.62 | 2.6461 | 1.010 |  |
| 1.05m-1t/streaming/two-hop-count/exec | 2.6149 | 2.6282 | 1.005 |  |
| 1.05m-1t/eager/types-grouped/exec | 0.0074 | 0.0085 | 1.149 | slower, short |
| 1.05m-1t/streaming/types-grouped/exec | 0.0124 | 0.0128 | 1.032 | slower, short |
| 1.05m-1t/eager/values-star/exec | 0.0148 | 0.0153 | 1.034 | slower, short |
| 1.05m-1t/streaming/values-star/exec | 0.0189 | 0.0198 | 1.048 | slower, short |
| 1.05m-12t/eager/class-predicate-list/exec | 0.973 | 0.9796 | 1.007 |  |
| 1.05m-12t/streaming/class-predicate-list/exec | 1.005 | 0.9774 | 0.973 |  |
| 1.05m-12t/eager/class-predicates/exec | 0.9814 | 1.0028 | 1.022 |  |
| 1.05m-12t/streaming/class-predicates/exec | 1.0255 | 0.985 | 0.961 |  |
| 1.05m-12t/eager/contains/exec | 0.7176 | 0.7304 | 1.018 |  |
| 1.05m-12t/streaming/contains/exec | 0.7499 | 0.7594 | 1.013 |  |
| 1.05m-12t/eager/count-all/exec | 0.0074 | 0.0072 | 0.973 |  |
| 1.05m-12t/streaming/count-all/exec | 0.0105 | 0.0092 | 0.876 |  |
| 1.05m-12t/eager/distinct-join/exec | 3.5292 | 3.579 | 1.014 |  |
| 1.05m-12t/streaming/distinct-join/exec | 3.8761 | 3.7664 | 0.972 |  |
| 1.05m-12t/eager/distinct-obj/exec | 0.006 | 0.0066 | 1.100 | slower, short |
| 1.05m-12t/streaming/distinct-obj/exec | 0.0091 | 0.0076 | 0.835 |  |
| 1.05m-12t/eager/employee-docs/exec | 0.0494 | 0.0494 | 1.000 |  |
| 1.05m-12t/streaming/employee-docs/exec | 0.0566 | 0.0532 | 0.940 |  |
| 1.05m-12t/eager/exists-join/exec | 1.0915 | 1.104 | 1.011 |  |
| 1.05m-12t/streaming/exists-join/exec | 1.1348 | 1.1012 | 0.970 |  |
| 1.05m-12t/eager/export-500k/exec | 2.4226 | 2.33 | 0.962 |  |
| 1.05m-12t/streaming/export-500k/exec | 0.4075 | 0.4109 | 1.008 |  |
| 1.05m-12t/eager/expr-agg-arg/exec | 3.0024 | 2.9364 | 0.978 |  |
| 1.05m-12t/streaming/expr-agg-arg/exec | 2.0102 | 2.0075 | 0.999 |  |
| 1.05m-12t/eager/expr-bind-group/exec | 0.8553 | 0.8229 | 0.962 |  |
| 1.05m-12t/streaming/expr-bind-group/exec | 0.3928 | 0.3984 | 1.014 |  |
| 1.05m-12t/eager/expr-order-key/exec | 1.2888 | 1.2338 | 0.957 |  |
| 1.05m-12t/streaming/expr-order-key/exec | 1.2076 | 1.2804 | 1.060 | slower |
| 1.05m-12t/eager/group-avg/exec | 4.4798 | 4.1406 | 0.924 |  |
| 1.05m-12t/streaming/group-avg/exec | 3.5906 | 3.4011 | 0.947 |  |
| 1.05m-12t/eager/knows-reach/exec | 11.8375 | 11.7683 | 0.994 |  |
| 1.05m-12t/streaming/knows-reach/exec | 11.688 | 11.6548 | 0.997 |  |
| 1.05m-12t/eager/lang-filter/exec | 0.705 | 0.7539 | 1.069 | slower |
| 1.05m-12t/streaming/lang-filter/exec | 0.7477 | 0.7287 | 0.975 |  |
| 1.05m-12t/eager/minus/exec | 0.3522 | 0.3401 | 0.966 |  |
| 1.05m-12t/streaming/minus/exec | 0.2815 | 0.2777 | 0.987 |  |
| 1.05m-12t/eager/not-exists/exec | 0.8651 | 0.8565 | 0.990 |  |
| 1.05m-12t/streaming/not-exists/exec | 0.7986 | 0.7975 | 0.999 |  |
| 1.05m-12t/eager/optional-chain/exec | 2.1357 | 2.1289 | 0.997 |  |
| 1.05m-12t/streaming/optional-chain/exec | 2.0777 | 2.0969 | 1.009 |  |
| 1.05m-12t/eager/optional-count/exec | 0.5425 | 0.5365 | 0.989 |  |
| 1.05m-12t/streaming/optional-count/exec | 0.5556 | 0.558 | 1.004 |  |
| 1.05m-12t/eager/order-by-full/exec | 14.0955 | 8.9195 | 0.633 |  |
| 1.05m-12t/streaming/order-by-full/exec | 15.7521 | 9.2472 | 0.587 |  |
| 1.05m-12t/eager/path-plus/exec | 0.018 | 0.0185 | 1.028 | slower, short |
| 1.05m-12t/streaming/path-plus/exec | 0.0224 | 0.02 | 0.893 |  |
| 1.05m-12t/eager/predicate-counts/exec | 0.0068 | 0.0069 | 1.015 | slower, short |
| 1.05m-12t/streaming/predicate-counts/exec | 0.011 | 0.0103 | 0.936 |  |
| 1.05m-12t/eager/predicate-subjects/exec | 0.0095 | 0.009 | 0.947 |  |
| 1.05m-12t/streaming/predicate-subjects/exec | 0.0154 | 0.0152 | 0.987 |  |
| 1.05m-12t/eager/range-topk/exec | 0.7128 | 0.5715 | 0.802 |  |
| 1.05m-12t/streaming/range-topk/exec | 0.5688 | 0.5668 | 0.996 |  |
| 1.05m-12t/eager/regex-iri/exec | 0.9223 | 0.9856 | 1.069 | slower |
| 1.05m-12t/streaming/regex-iri/exec | 0.9973 | 0.9581 | 0.961 |  |
| 1.05m-12t/eager/star-join/exec | 1.5408 | 1.6144 | 1.048 |  |
| 1.05m-12t/streaming/star-join/exec | 1.6506 | 1.6428 | 0.995 |  |
| 1.05m-12t/eager/star-lookup/exec | 0.0837 | 0.0842 | 1.006 | slower, short |
| 1.05m-12t/streaming/star-lookup/exec | 0.0886 | 0.094 | 1.061 | slower, short |
| 1.05m-12t/eager/subquery-agg/exec | 0.2389 | 0.2334 | 0.977 |  |
| 1.05m-12t/streaming/subquery-agg/exec | 0.2225 | 0.2253 | 1.013 | slower, short |
| 1.05m-12t/eager/two-hop-count/exec | 1.2677 | 1.2139 | 0.958 |  |
| 1.05m-12t/streaming/two-hop-count/exec | 1.2492 | 1.2527 | 1.003 |  |
| 1.05m-12t/eager/types-grouped/exec | 0.0115 | 0.0118 | 1.026 | slower, short |
| 1.05m-12t/streaming/types-grouped/exec | 0.0193 | 0.0185 | 0.959 |  |
| 1.05m-12t/eager/values-star/exec | 0.0205 | 0.0192 | 0.937 |  |
| 1.05m-12t/streaming/values-star/exec | 0.0262 | 0.0277 | 1.057 | slower, short |
| 10.5m-1t/eager/class-predicate-list/exec | 9.8319 | 9.8738 | 1.004 |  |
| 10.5m-1t/streaming/class-predicate-list/exec | 10.2733 | 10.2686 | 1.000 |  |
| 10.5m-1t/eager/class-predicates/exec | 9.8483 | 9.8502 | 1.000 |  |
| 10.5m-1t/streaming/class-predicates/exec | 10.2449 | 10.2741 | 1.003 |  |
| 10.5m-1t/eager/contains/exec | 23.4661 | 24.4183 | 1.041 |  |
| 10.5m-1t/streaming/contains/exec | 24.0445 | 24.4599 | 1.017 |  |
| 10.5m-1t/eager/count-all/exec | 0.0058 | 0.0061 | 1.052 | slower, short |
| 10.5m-1t/streaming/count-all/exec | 0.0068 | 0.0071 | 1.044 | slower, short |
| 10.5m-1t/eager/distinct-join/exec | 38.2491 | 38.4891 | 1.006 |  |
| 10.5m-1t/streaming/distinct-join/exec | 37.9224 | 37.4851 | 0.988 |  |
| 10.5m-1t/eager/distinct-obj/exec | 0.0039 | 0.0047 | 1.205 | slower, short |
| 10.5m-1t/streaming/distinct-obj/exec | 0.005 | 0.0053 | 1.060 | slower, short |
| 10.5m-1t/eager/employee-docs/exec | 0.0482 | 0.0493 | 1.023 | slower, short |
| 10.5m-1t/streaming/employee-docs/exec | 0.0535 | 0.0552 | 1.032 | slower, short |
| 10.5m-1t/eager/exists-join/exec | 8.5635 | 8.4838 | 0.991 |  |
| 10.5m-1t/streaming/exists-join/exec | 8.5601 | 8.4718 | 0.990 |  |
| 10.5m-1t/eager/export-500k/exec | 2.2178 | 2.2054 | 0.994 |  |
| 10.5m-1t/streaming/export-500k/exec | 0.4425 | 0.4301 | 0.972 |  |
| 10.5m-1t/eager/expr-agg-arg/exec | 29.4458 | 29.2226 | 0.992 |  |
| 10.5m-1t/streaming/expr-agg-arg/exec | 23.0005 | 22.7784 | 0.990 |  |
| 10.5m-1t/eager/expr-bind-group/exec | 5.5701 | 5.7527 | 1.033 |  |
| 10.5m-1t/streaming/expr-bind-group/exec | 3.2995 | 3.298 | 1.000 |  |
| 10.5m-1t/eager/expr-order-key/exec | 11.8812 | 10.9126 | 0.918 |  |
| 10.5m-1t/streaming/expr-order-key/exec | 10.5345 | 10.7785 | 1.023 |  |
| 10.5m-1t/eager/group-avg/exec | 47.5151 | 45.314 | 0.954 |  |
| 10.5m-1t/streaming/group-avg/exec | 39.3425 | 36.706 | 0.933 |  |
| 10.5m-1t/eager/knows-reach/exec | 141.565 | 139.616 | 0.986 |  |
| 10.5m-1t/streaming/knows-reach/exec | 137.47 | 136.913 | 0.996 |  |
| 10.5m-1t/eager/lang-filter/exec | 15.6537 | 16.0053 | 1.022 |  |
| 10.5m-1t/streaming/lang-filter/exec | 15.9092 | 15.9799 | 1.004 |  |
| 10.5m-1t/eager/minus/exec | 3.1705 | 3.2511 | 1.025 |  |
| 10.5m-1t/streaming/minus/exec | 2.4382 | 2.4409 | 1.001 |  |
| 10.5m-1t/eager/not-exists/exec | 9.7704 | 9.4401 | 0.966 |  |
| 10.5m-1t/streaming/not-exists/exec | 8.8687 | 8.6438 | 0.975 |  |
| 10.5m-1t/eager/optional-chain/exec | 29.844 | 27.6928 | 0.928 |  |
| 10.5m-1t/streaming/optional-chain/exec | 22.8527 | 22.5187 | 0.985 |  |
| 10.5m-1t/eager/optional-count/exec | 5.3228 | 5.3185 | 0.999 |  |
| 10.5m-1t/streaming/optional-count/exec | 5.0155 | 4.9994 | 0.997 |  |
| 10.5m-1t/eager/order-by-full/exec | 677.315 | 514.724 | 0.760 |  |
| 10.5m-1t/streaming/order-by-full/exec | 596.671 | 517.212 | 0.867 |  |
| 10.5m-1t/eager/path-plus/exec | 0.0089 | 0.0095 | 1.067 | slower, short |
| 10.5m-1t/streaming/path-plus/exec | 0.0091 | 0.0094 | 1.033 | slower, short |
| 10.5m-1t/eager/predicate-counts/exec | 0.0047 | 0.0054 | 1.149 | slower, short |
| 10.5m-1t/streaming/predicate-counts/exec | 0.0064 | 0.0066 | 1.031 | slower, short |
| 10.5m-1t/eager/predicate-subjects/exec | 0.0063 | 0.0075 | 1.190 | slower, short |
| 10.5m-1t/streaming/predicate-subjects/exec | 0.0085 | 0.0087 | 1.024 | slower, short |
| 10.5m-1t/eager/range-topk/exec | 5.8508 | 5.0799 | 0.868 |  |
| 10.5m-1t/streaming/range-topk/exec | 5.0948 | 5.0418 | 0.990 |  |
| 10.5m-1t/eager/regex-iri/exec | 35.5455 | 34.4965 | 0.970 |  |
| 10.5m-1t/streaming/regex-iri/exec | 36.7402 | 34.5189 | 0.940 |  |
| 10.5m-1t/eager/star-join/exec | 13.8913 | 14.067 | 1.013 |  |
| 10.5m-1t/streaming/star-join/exec | 14.5118 | 14.8004 | 1.020 |  |
| 10.5m-1t/eager/star-lookup/exec | 0.0556 | 0.0574 | 1.032 | slower, short |
| 10.5m-1t/streaming/star-lookup/exec | 0.0605 | 0.0611 | 1.010 | slower, short |
| 10.5m-1t/eager/subquery-agg/exec | 1.6531 | 1.6985 | 1.027 |  |
| 10.5m-1t/streaming/subquery-agg/exec | 1.6389 | 1.6515 | 1.008 |  |
| 10.5m-1t/eager/two-hop-count/exec | 27.6608 | 27.7387 | 1.003 |  |
| 10.5m-1t/streaming/two-hop-count/exec | 27.7318 | 27.8409 | 1.004 |  |
| 10.5m-1t/eager/types-grouped/exec | 0.0074 | 0.0089 | 1.203 | slower, short |
| 10.5m-1t/streaming/types-grouped/exec | 0.0134 | 0.0127 | 0.948 |  |
| 10.5m-1t/eager/values-star/exec | 0.0168 | 0.0161 | 0.958 |  |
| 10.5m-1t/streaming/values-star/exec | 0.0196 | 0.0207 | 1.056 | slower, short |
| 10.5m-12t/eager/class-predicate-list/exec | 10.178 | 10.213 | 1.003 |  |
| 10.5m-12t/streaming/class-predicate-list/exec | 10.4968 | 10.5404 | 1.004 |  |
| 10.5m-12t/eager/class-predicates/exec | 10.1159 | 10.1471 | 1.003 |  |
| 10.5m-12t/streaming/class-predicates/exec | 10.5169 | 10.5623 | 1.004 |  |
| 10.5m-12t/eager/contains/exec | 4.004 | 4.2087 | 1.051 | slower |
| 10.5m-12t/streaming/contains/exec | 4.0798 | 4.1981 | 1.029 |  |
| 10.5m-12t/eager/count-all/exec | 0.008 | 0.0091 | 1.137 | slower, short |
| 10.5m-12t/streaming/count-all/exec | 0.0119 | 0.0107 | 0.899 |  |
| 10.5m-12t/eager/distinct-join/exec | 38.5196 | 39.015 | 1.013 |  |
| 10.5m-12t/streaming/distinct-join/exec | 38.051 | 37.7993 | 0.993 |  |
| 10.5m-12t/eager/distinct-obj/exec | 0.0058 | 0.0063 | 1.086 | slower, short |
| 10.5m-12t/streaming/distinct-obj/exec | 0.0093 | 0.0087 | 0.935 |  |
| 10.5m-12t/eager/employee-docs/exec | 0.0678 | 0.0674 | 0.994 |  |
| 10.5m-12t/streaming/employee-docs/exec | 0.0789 | 0.0804 | 1.019 | slower, short |
| 10.5m-12t/eager/exists-join/exec | 8.852 | 8.9689 | 1.013 |  |
| 10.5m-12t/streaming/exists-join/exec | 8.8247 | 8.8335 | 1.001 |  |
| 10.5m-12t/eager/export-500k/exec | 2.4799 | 2.5833 | 1.042 |  |
| 10.5m-12t/streaming/export-500k/exec | 0.4091 | 0.4231 | 1.034 |  |
| 10.5m-12t/eager/expr-agg-arg/exec | 28.0343 | 28.7974 | 1.027 |  |
| 10.5m-12t/streaming/expr-agg-arg/exec | 23.2254 | 23.193 | 0.999 |  |
| 10.5m-12t/eager/expr-bind-group/exec | 4.8124 | 4.8261 | 1.003 |  |
| 10.5m-12t/streaming/expr-bind-group/exec | 3.4362 | 3.4278 | 0.998 |  |
| 10.5m-12t/eager/expr-order-key/exec | 10.3228 | 10.1057 | 0.979 |  |
| 10.5m-12t/streaming/expr-order-key/exec | 10.7844 | 11.1112 | 1.030 |  |
| 10.5m-12t/eager/group-avg/exec | 47.8198 | 46.1542 | 0.965 |  |
| 10.5m-12t/streaming/group-avg/exec | 40.0891 | 37.5777 | 0.937 |  |
| 10.5m-12t/eager/knows-reach/exec | 142.627 | 143.563 | 1.007 |  |
| 10.5m-12t/streaming/knows-reach/exec | 140.373 | 140.487 | 1.001 |  |
| 10.5m-12t/eager/lang-filter/exec | 2.8064 | 2.9065 | 1.036 |  |
| 10.5m-12t/streaming/lang-filter/exec | 2.8694 | 2.9165 | 1.016 |  |
| 10.5m-12t/eager/minus/exec | 3.238 | 3.3023 | 1.020 |  |
| 10.5m-12t/streaming/minus/exec | 2.6027 | 2.571 | 0.988 |  |
| 10.5m-12t/eager/not-exists/exec | 9.7976 | 10.1632 | 1.037 |  |
| 10.5m-12t/streaming/not-exists/exec | 8.9343 | 9.0994 | 1.018 |  |
| 10.5m-12t/eager/optional-chain/exec | 28.4546 | 45.2343 | 1.590 | slower |
| 10.5m-12t/streaming/optional-chain/exec | 25.5174 | 25.4781 | 0.998 |  |
| 10.5m-12t/eager/optional-count/exec | 5.3987 | 5.4636 | 1.012 |  |
| 10.5m-12t/streaming/optional-count/exec | 5.22 | 5.1799 | 0.992 |  |
| 10.5m-12t/eager/order-by-full/exec | 239.272 | 187.553 | 0.784 |  |
| 10.5m-12t/streaming/order-by-full/exec | 266.403 | 191.771 | 0.720 |  |
| 10.5m-12t/eager/path-plus/exec | 0.0137 | 0.0153 | 1.117 | slower, short |
| 10.5m-12t/streaming/path-plus/exec | 0.0142 | 0.0149 | 1.049 | slower, short |
| 10.5m-12t/eager/predicate-counts/exec | 0.0068 | 0.0076 | 1.118 | slower, short |
| 10.5m-12t/streaming/predicate-counts/exec | 0.0111 | 0.0111 | 1.000 |  |
| 10.5m-12t/eager/predicate-subjects/exec | 0.0079 | 0.0091 | 1.152 | slower, short |
| 10.5m-12t/streaming/predicate-subjects/exec | 0.0157 | 0.0145 | 0.924 |  |
| 10.5m-12t/eager/range-topk/exec | 2.456 | 2.1753 | 0.886 |  |
| 10.5m-12t/streaming/range-topk/exec | 2.2472 | 2.2206 | 0.988 |  |
| 10.5m-12t/eager/regex-iri/exec | 5.9913 | 6.2021 | 1.035 |  |
| 10.5m-12t/streaming/regex-iri/exec | 6.2395 | 6.2201 | 0.997 |  |
| 10.5m-12t/eager/star-join/exec | 13.0634 | 13.7188 | 1.050 | slower |
| 10.5m-12t/streaming/star-join/exec | 13.5013 | 14.0274 | 1.039 |  |
| 10.5m-12t/eager/star-lookup/exec | 0.0725 | 0.0733 | 1.011 | slower, short |
| 10.5m-12t/streaming/star-lookup/exec | 0.082 | 0.0806 | 0.983 |  |
| 10.5m-12t/eager/subquery-agg/exec | 0.7566 | 0.8063 | 1.066 | slower |
| 10.5m-12t/streaming/subquery-agg/exec | 0.83 | 0.8356 | 1.007 |  |
| 10.5m-12t/eager/two-hop-count/exec | 11.1937 | 11.3232 | 1.012 |  |
| 10.5m-12t/streaming/two-hop-count/exec | 11.2398 | 11.2561 | 1.001 |  |
| 10.5m-12t/eager/types-grouped/exec | 0.0105 | 0.0115 | 1.095 | slower, short |
| 10.5m-12t/streaming/types-grouped/exec | 0.0203 | 0.0196 | 0.966 |  |
| 10.5m-12t/eager/values-star/exec | 0.0202 | 0.0206 | 1.020 | slower, short |
| 10.5m-12t/streaming/values-star/exec | 0.0266 | 0.0325 | 1.222 | slower, short |

248 metrics compared, 57 flagged, 0 failed checks, 0 missing checks, 0 changed fingerprints.
