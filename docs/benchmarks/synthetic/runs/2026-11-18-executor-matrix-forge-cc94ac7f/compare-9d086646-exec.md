# executors gate: candidate against baseline

- baseline: host forge, commit final, started 2026-11-02T18:32:08Z
- candidate: host forge, commit cc94ac7f, started 2026-11-18T07:04:20Z
- threshold: 5.0% slower, 0.0% below 0.3 ms
- compared although different: setting run: '31 queries, scales 100000 1000000 people, threads 12, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server' against '31 queries, scales 100000 1000000 people, threads 12 1, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server'
- compared although different: manifest build_config: '{"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{},"mbx_policy":"779a99e845a5ac3f1c4d64a3c95bcc74b33f47418e829bf008542ec2dbbcd151","profile":"release"}' against '{"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1},"sparkles-owl":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{"CARGO_INCREMENTAL":"0"},"mbx_policy":"779a99e845a5ac3f1c4d64a3c95bcc74b33f47418e829bf008542ec2dbbcd151","profile":"release"}'

| metric | baseline | candidate | ratio | flag |
|---|--:|--:|--:|---|
| 1.05m-12t/eager/class-predicate-list/exec | 0.9658 | 0.9796 | 1.014 |  |
| 1.05m-12t/streaming/class-predicate-list/exec | 0.9937 | 0.9774 | 0.984 |  |
| 1.05m-12t/eager/class-predicates/exec | 0.9727 | 1.0028 | 1.031 |  |
| 1.05m-12t/streaming/class-predicates/exec | 1.009 | 0.985 | 0.976 |  |
| 1.05m-12t/eager/contains/exec | 0.7178 | 0.7304 | 1.018 |  |
| 1.05m-12t/streaming/contains/exec | 0.7083 | 0.7594 | 1.072 | slower |
| 1.05m-12t/eager/count-all/exec | 0.008 | 0.0072 | 0.900 |  |
| 1.05m-12t/streaming/count-all/exec | 0.0103 | 0.0092 | 0.893 |  |
| 1.05m-12t/eager/distinct-join/exec | 3.4968 | 3.579 | 1.024 |  |
| 1.05m-12t/streaming/distinct-join/exec | 4.0037 | 3.7664 | 0.941 |  |
| 1.05m-12t/eager/distinct-obj/exec | 0.0068 | 0.0066 | 0.971 |  |
| 1.05m-12t/streaming/distinct-obj/exec | 0.0094 | 0.0076 | 0.809 |  |
| 1.05m-12t/eager/employee-docs/exec | 0.0503 | 0.0494 | 0.982 |  |
| 1.05m-12t/streaming/employee-docs/exec | 0.0544 | 0.0532 | 0.978 |  |
| 1.05m-12t/eager/exists-join/exec | 1.1039 | 1.104 | 1.000 |  |
| 1.05m-12t/streaming/exists-join/exec | 1.141 | 1.1012 | 0.965 |  |
| 1.05m-12t/eager/export-500k/exec | 2.3697 | 2.33 | 0.983 |  |
| 1.05m-12t/streaming/export-500k/exec | 0.4078 | 0.4109 | 1.008 |  |
| 1.05m-12t/eager/expr-agg-arg/exec | 2.9354 | 2.9364 | 1.000 |  |
| 1.05m-12t/streaming/expr-agg-arg/exec | 2.0795 | 2.0075 | 0.965 |  |
| 1.05m-12t/eager/expr-bind-group/exec | 0.7893 | 0.8229 | 1.043 |  |
| 1.05m-12t/streaming/expr-bind-group/exec | 0.401 | 0.3984 | 0.994 |  |
| 1.05m-12t/eager/expr-order-key/exec | 1.2469 | 1.2338 | 0.989 |  |
| 1.05m-12t/streaming/expr-order-key/exec | 1.2613 | 1.2804 | 1.015 |  |
| 1.05m-12t/eager/group-avg/exec | 4.3705 | 4.1406 | 0.947 |  |
| 1.05m-12t/streaming/group-avg/exec | 3.5623 | 3.4011 | 0.955 |  |
| 1.05m-12t/eager/knows-reach/exec | 11.7889 | 11.7683 | 0.998 |  |
| 1.05m-12t/streaming/knows-reach/exec | 11.5946 | 11.6548 | 1.005 |  |
| 1.05m-12t/eager/lang-filter/exec | 0.7751 | 0.7539 | 0.973 |  |
| 1.05m-12t/streaming/lang-filter/exec | 0.7599 | 0.7287 | 0.959 |  |
| 1.05m-12t/eager/minus/exec | 0.3484 | 0.3401 | 0.976 |  |
| 1.05m-12t/streaming/minus/exec | 0.2979 | 0.2777 | 0.932 |  |
| 1.05m-12t/eager/not-exists/exec | 0.8784 | 0.8565 | 0.975 |  |
| 1.05m-12t/streaming/not-exists/exec | 0.8179 | 0.7975 | 0.975 |  |
| 1.05m-12t/eager/optional-chain/exec | 2.1473 | 2.1289 | 0.991 |  |
| 1.05m-12t/streaming/optional-chain/exec | 2.1129 | 2.0969 | 0.992 |  |
| 1.05m-12t/eager/optional-count/exec | 0.5549 | 0.5365 | 0.967 |  |
| 1.05m-12t/streaming/optional-count/exec | 0.5445 | 0.558 | 1.025 |  |
| 1.05m-12t/eager/order-by-full/exec | 14.1676 | 8.9195 | 0.630 |  |
| 1.05m-12t/streaming/order-by-full/exec | 14.482 | 9.2472 | 0.639 |  |
| 1.05m-12t/eager/path-plus/exec | 0.0182 | 0.0185 | 1.016 | slower, short |
| 1.05m-12t/streaming/path-plus/exec | 0.0201 | 0.02 | 0.995 |  |
| 1.05m-12t/eager/predicate-counts/exec | 0.0061 | 0.0069 | 1.131 | slower, short |
| 1.05m-12t/streaming/predicate-counts/exec | 0.0104 | 0.0103 | 0.990 |  |
| 1.05m-12t/eager/predicate-subjects/exec | 0.0106 | 0.009 | 0.849 |  |
| 1.05m-12t/streaming/predicate-subjects/exec | 0.0148 | 0.0152 | 1.027 | slower, short |
| 1.05m-12t/eager/range-topk/exec | 0.5682 | 0.5715 | 1.006 |  |
| 1.05m-12t/streaming/range-topk/exec | 0.5725 | 0.5668 | 0.990 |  |
| 1.05m-12t/eager/regex-iri/exec | 1.0356 | 0.9856 | 0.952 |  |
| 1.05m-12t/streaming/regex-iri/exec | 1.0471 | 0.9581 | 0.915 |  |
| 1.05m-12t/eager/star-join/exec | 1.6445 | 1.6144 | 0.982 |  |
| 1.05m-12t/streaming/star-join/exec | 1.6748 | 1.6428 | 0.981 |  |
| 1.05m-12t/eager/star-lookup/exec | 0.0841 | 0.0842 | 1.001 | slower, short |
| 1.05m-12t/streaming/star-lookup/exec | 0.0888 | 0.094 | 1.059 | slower, short |
| 1.05m-12t/eager/subquery-agg/exec | 0.2207 | 0.2334 | 1.058 | slower, short |
| 1.05m-12t/streaming/subquery-agg/exec | 0.1884 | 0.2253 | 1.196 | slower, short |
| 1.05m-12t/eager/two-hop-count/exec | 1.2314 | 1.2139 | 0.986 |  |
| 1.05m-12t/streaming/two-hop-count/exec | 1.2518 | 1.2527 | 1.001 |  |
| 1.05m-12t/eager/types-grouped/exec | 0.0122 | 0.0118 | 0.967 |  |
| 1.05m-12t/streaming/types-grouped/exec | 0.0224 | 0.0185 | 0.826 |  |
| 1.05m-12t/eager/values-star/exec | 0.0203 | 0.0192 | 0.946 |  |
| 1.05m-12t/streaming/values-star/exec | 0.0323 | 0.0277 | 0.858 |  |
| 10.5m-12t/eager/class-predicate-list/exec | 9.9453 | 10.213 | 1.027 |  |
| 10.5m-12t/streaming/class-predicate-list/exec | 10.5076 | 10.5404 | 1.003 |  |
| 10.5m-12t/eager/class-predicates/exec | 10.0375 | 10.1471 | 1.011 |  |
| 10.5m-12t/streaming/class-predicates/exec | 10.4419 | 10.5623 | 1.012 |  |
| 10.5m-12t/eager/contains/exec | 4.1414 | 4.2087 | 1.016 |  |
| 10.5m-12t/streaming/contains/exec | 4.1481 | 4.1981 | 1.012 |  |
| 10.5m-12t/eager/count-all/exec | 0.0094 | 0.0091 | 0.968 |  |
| 10.5m-12t/streaming/count-all/exec | 0.0126 | 0.0107 | 0.849 |  |
| 10.5m-12t/eager/distinct-join/exec | 38.5522 | 39.015 | 1.012 |  |
| 10.5m-12t/streaming/distinct-join/exec | 38.8756 | 37.7993 | 0.972 |  |
| 10.5m-12t/eager/distinct-obj/exec | 0.0065 | 0.0063 | 0.969 |  |
| 10.5m-12t/streaming/distinct-obj/exec | 0.0108 | 0.0087 | 0.806 |  |
| 10.5m-12t/eager/employee-docs/exec | 0.0681 | 0.0674 | 0.990 |  |
| 10.5m-12t/streaming/employee-docs/exec | 0.0771 | 0.0804 | 1.043 | slower, short |
| 10.5m-12t/eager/exists-join/exec | 8.9121 | 8.9689 | 1.006 |  |
| 10.5m-12t/streaming/exists-join/exec | 8.9344 | 8.8335 | 0.989 |  |
| 10.5m-12t/eager/export-500k/exec | 2.3284 | 2.5833 | 1.109 | slower |
| 10.5m-12t/streaming/export-500k/exec | 0.409 | 0.4231 | 1.034 |  |
| 10.5m-12t/eager/expr-agg-arg/exec | 28.5089 | 28.7974 | 1.010 |  |
| 10.5m-12t/streaming/expr-agg-arg/exec | 23.5466 | 23.193 | 0.985 |  |
| 10.5m-12t/eager/expr-bind-group/exec | 4.7495 | 4.8261 | 1.016 |  |
| 10.5m-12t/streaming/expr-bind-group/exec | 3.4235 | 3.4278 | 1.001 |  |
| 10.5m-12t/eager/expr-order-key/exec | 10.4957 | 10.1057 | 0.963 |  |
| 10.5m-12t/streaming/expr-order-key/exec | 11.2336 | 11.1112 | 0.989 |  |
| 10.5m-12t/eager/group-avg/exec | 47.5777 | 46.1542 | 0.970 |  |
| 10.5m-12t/streaming/group-avg/exec | 40.0713 | 37.5777 | 0.938 |  |
| 10.5m-12t/eager/knows-reach/exec | 141.699 | 143.563 | 1.013 |  |
| 10.5m-12t/streaming/knows-reach/exec | 139.594 | 140.487 | 1.006 |  |
| 10.5m-12t/eager/lang-filter/exec | 3.1229 | 2.9065 | 0.931 |  |
| 10.5m-12t/streaming/lang-filter/exec | 3.055 | 2.9165 | 0.955 |  |
| 10.5m-12t/eager/minus/exec | 3.2872 | 3.3023 | 1.005 |  |
| 10.5m-12t/streaming/minus/exec | 2.5828 | 2.571 | 0.995 |  |
| 10.5m-12t/eager/not-exists/exec | 9.7005 | 10.1632 | 1.048 |  |
| 10.5m-12t/streaming/not-exists/exec | 8.935 | 9.0994 | 1.018 |  |
| 10.5m-12t/eager/optional-chain/exec | 29.3404 | 45.2343 | 1.542 | slower |
| 10.5m-12t/streaming/optional-chain/exec | 25.564 | 25.4781 | 0.997 |  |
| 10.5m-12t/eager/optional-count/exec | 5.3604 | 5.4636 | 1.019 |  |
| 10.5m-12t/streaming/optional-count/exec | 5.222 | 5.1799 | 0.992 |  |
| 10.5m-12t/eager/order-by-full/exec | 250.395 | 187.553 | 0.749 |  |
| 10.5m-12t/streaming/order-by-full/exec | 268.349 | 191.771 | 0.715 |  |
| 10.5m-12t/eager/path-plus/exec | 0.0138 | 0.0153 | 1.109 | slower, short |
| 10.5m-12t/streaming/path-plus/exec | 0.0145 | 0.0149 | 1.028 | slower, short |
| 10.5m-12t/eager/predicate-counts/exec | 0.0078 | 0.0076 | 0.974 |  |
| 10.5m-12t/streaming/predicate-counts/exec | 0.0146 | 0.0111 | 0.760 |  |
| 10.5m-12t/eager/predicate-subjects/exec | 0.0093 | 0.0091 | 0.978 |  |
| 10.5m-12t/streaming/predicate-subjects/exec | 0.0154 | 0.0145 | 0.942 |  |
| 10.5m-12t/eager/range-topk/exec | 2.4239 | 2.1753 | 0.897 |  |
| 10.5m-12t/streaming/range-topk/exec | 2.3227 | 2.2206 | 0.956 |  |
| 10.5m-12t/eager/regex-iri/exec | 6.5133 | 6.2021 | 0.952 |  |
| 10.5m-12t/streaming/regex-iri/exec | 6.4436 | 6.2201 | 0.965 |  |
| 10.5m-12t/eager/star-join/exec | 13.2103 | 13.7188 | 1.038 |  |
| 10.5m-12t/streaming/star-join/exec | 13.8683 | 14.0274 | 1.011 |  |
| 10.5m-12t/eager/star-lookup/exec | 0.0728 | 0.0733 | 1.007 | slower, short |
| 10.5m-12t/streaming/star-lookup/exec | 0.0839 | 0.0806 | 0.961 |  |
| 10.5m-12t/eager/subquery-agg/exec | 0.8163 | 0.8063 | 0.988 |  |
| 10.5m-12t/streaming/subquery-agg/exec | 0.8099 | 0.8356 | 1.032 |  |
| 10.5m-12t/eager/two-hop-count/exec | 11.1398 | 11.3232 | 1.016 |  |
| 10.5m-12t/streaming/two-hop-count/exec | 11.1219 | 11.2561 | 1.012 |  |
| 10.5m-12t/eager/types-grouped/exec | 0.0122 | 0.0115 | 0.943 |  |
| 10.5m-12t/streaming/types-grouped/exec | 0.0194 | 0.0196 | 1.010 | slower, short |
| 10.5m-12t/eager/values-star/exec | 0.0205 | 0.0206 | 1.005 | slower, short |
| 10.5m-12t/streaming/values-star/exec | 0.028 | 0.0325 | 1.161 | slower, short |

124 metrics compared, 17 flagged, 0 failed checks, 0 missing checks, 0 changed fingerprints.
