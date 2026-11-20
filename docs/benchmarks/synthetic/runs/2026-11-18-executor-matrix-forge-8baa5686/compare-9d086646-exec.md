# executors gate: candidate against baseline

- baseline: host forge, commit final, started 2026-11-02T18:32:08Z
- candidate: host forge, commit 8baa5686, started 2026-11-18T09:27:30Z
- threshold: 5.0% slower, 0.0% below 0.3 ms
- compared although different: setting run: '31 queries, scales 100000 1000000 people, threads 12, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server' against '31 queries, scales 100000 1000000 people, threads 12 1, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server'
- compared although different: manifest build_config: '{"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{},"mbx_policy":"779a99e845a5ac3f1c4d64a3c95bcc74b33f47418e829bf008542ec2dbbcd151","profile":"release"}' against '{"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1},"sparkles-owl":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{"CARGO_INCREMENTAL":"0"},"mbx_policy":"779a99e845a5ac3f1c4d64a3c95bcc74b33f47418e829bf008542ec2dbbcd151","profile":"release"}'

| metric | baseline | candidate | ratio | flag |
|---|--:|--:|--:|---|
| 1.05m-12t/eager/class-predicate-list/exec | 0.9658 | 0.9861 | 1.021 |  |
| 1.05m-12t/streaming/class-predicate-list/exec | 0.9937 | 1.0122 | 1.019 |  |
| 1.05m-12t/eager/class-predicates/exec | 0.9727 | 0.9958 | 1.024 |  |
| 1.05m-12t/streaming/class-predicates/exec | 1.009 | 1.0197 | 1.011 |  |
| 1.05m-12t/eager/contains/exec | 0.7178 | 0.7585 | 1.057 | slower |
| 1.05m-12t/streaming/contains/exec | 0.7083 | 0.7729 | 1.091 | slower |
| 1.05m-12t/eager/count-all/exec | 0.008 | 0.008 | 1.000 |  |
| 1.05m-12t/streaming/count-all/exec | 0.0103 | 0.0103 | 1.000 |  |
| 1.05m-12t/eager/distinct-join/exec | 3.4968 | 3.7598 | 1.075 | slower |
| 1.05m-12t/streaming/distinct-join/exec | 4.0037 | 4.015 | 1.003 |  |
| 1.05m-12t/eager/distinct-obj/exec | 0.0068 | 0.0068 | 1.000 |  |
| 1.05m-12t/streaming/distinct-obj/exec | 0.0094 | 0.0087 | 0.926 |  |
| 1.05m-12t/eager/employee-docs/exec | 0.0503 | 0.0505 | 1.004 | slower, short |
| 1.05m-12t/streaming/employee-docs/exec | 0.0544 | 0.0542 | 0.996 |  |
| 1.05m-12t/eager/exists-join/exec | 1.1039 | 1.1391 | 1.032 |  |
| 1.05m-12t/streaming/exists-join/exec | 1.141 | 1.1496 | 1.008 |  |
| 1.05m-12t/eager/export-500k/exec | 2.3697 | 2.3889 | 1.008 |  |
| 1.05m-12t/streaming/export-500k/exec | 0.4078 | 0.3735 | 0.916 |  |
| 1.05m-12t/eager/expr-agg-arg/exec | 2.9354 | 2.9012 | 0.988 |  |
| 1.05m-12t/streaming/expr-agg-arg/exec | 2.0795 | 2.0462 | 0.984 |  |
| 1.05m-12t/eager/expr-bind-group/exec | 0.7893 | 0.7623 | 0.966 |  |
| 1.05m-12t/streaming/expr-bind-group/exec | 0.401 | 0.3979 | 0.992 |  |
| 1.05m-12t/eager/expr-order-key/exec | 1.2469 | 1.2422 | 0.996 |  |
| 1.05m-12t/streaming/expr-order-key/exec | 1.2613 | 1.2247 | 0.971 |  |
| 1.05m-12t/eager/group-avg/exec | 4.3705 | 4.0346 | 0.923 |  |
| 1.05m-12t/streaming/group-avg/exec | 3.5623 | 3.3616 | 0.944 |  |
| 1.05m-12t/eager/knows-reach/exec | 11.7889 | 11.8109 | 1.002 |  |
| 1.05m-12t/streaming/knows-reach/exec | 11.5946 | 11.6777 | 1.007 |  |
| 1.05m-12t/eager/lang-filter/exec | 0.7751 | 0.7779 | 1.004 |  |
| 1.05m-12t/streaming/lang-filter/exec | 0.7599 | 0.8097 | 1.066 | slower |
| 1.05m-12t/eager/minus/exec | 0.3484 | 0.4813 | 1.381 | slower |
| 1.05m-12t/streaming/minus/exec | 0.2979 | 0.2805 | 0.942 |  |
| 1.05m-12t/eager/not-exists/exec | 0.8784 | 0.8673 | 0.987 |  |
| 1.05m-12t/streaming/not-exists/exec | 0.8179 | 0.7928 | 0.969 |  |
| 1.05m-12t/eager/optional-chain/exec | 2.1473 | 2.1128 | 0.984 |  |
| 1.05m-12t/streaming/optional-chain/exec | 2.1129 | 2.2341 | 1.057 | slower |
| 1.05m-12t/eager/optional-count/exec | 0.5549 | 0.5478 | 0.987 |  |
| 1.05m-12t/streaming/optional-count/exec | 0.5445 | 0.5751 | 1.056 | slower |
| 1.05m-12t/eager/order-by-full/exec | 14.1676 | 8.7682 | 0.619 |  |
| 1.05m-12t/streaming/order-by-full/exec | 14.482 | 9.0326 | 0.624 |  |
| 1.05m-12t/eager/path-plus/exec | 0.0182 | 0.0175 | 0.962 |  |
| 1.05m-12t/streaming/path-plus/exec | 0.0201 | 0.0201 | 1.000 |  |
| 1.05m-12t/eager/predicate-counts/exec | 0.0061 | 0.0074 | 1.213 | slower, short |
| 1.05m-12t/streaming/predicate-counts/exec | 0.0104 | 0.0108 | 1.038 | slower, short |
| 1.05m-12t/eager/predicate-subjects/exec | 0.0106 | 0.0083 | 0.783 |  |
| 1.05m-12t/streaming/predicate-subjects/exec | 0.0148 | 0.0148 | 1.000 |  |
| 1.05m-12t/eager/range-topk/exec | 0.5682 | 0.582 | 1.024 |  |
| 1.05m-12t/streaming/range-topk/exec | 0.5725 | 0.5746 | 1.004 |  |
| 1.05m-12t/eager/regex-iri/exec | 1.0356 | 1.0236 | 0.988 |  |
| 1.05m-12t/streaming/regex-iri/exec | 1.0471 | 1.017 | 0.971 |  |
| 1.05m-12t/eager/star-join/exec | 1.6445 | 1.5913 | 0.968 |  |
| 1.05m-12t/streaming/star-join/exec | 1.6748 | 1.6511 | 0.986 |  |
| 1.05m-12t/eager/star-lookup/exec | 0.0841 | 0.0831 | 0.988 |  |
| 1.05m-12t/streaming/star-lookup/exec | 0.0888 | 0.0933 | 1.051 | slower, short |
| 1.05m-12t/eager/subquery-agg/exec | 0.2207 | 0.2184 | 0.990 |  |
| 1.05m-12t/streaming/subquery-agg/exec | 0.1884 | 0.2094 | 1.111 | slower, short |
| 1.05m-12t/eager/two-hop-count/exec | 1.2314 | 1.2316 | 1.000 |  |
| 1.05m-12t/streaming/two-hop-count/exec | 1.2518 | 1.2287 | 0.982 |  |
| 1.05m-12t/eager/types-grouped/exec | 0.0122 | 0.0122 | 1.000 |  |
| 1.05m-12t/streaming/types-grouped/exec | 0.0224 | 0.0209 | 0.933 |  |
| 1.05m-12t/eager/values-star/exec | 0.0203 | 0.021 | 1.034 | slower, short |
| 1.05m-12t/streaming/values-star/exec | 0.0323 | 0.0313 | 0.969 |  |
| 10.5m-12t/eager/class-predicate-list/exec | 9.9453 | 10.4838 | 1.054 | slower |
| 10.5m-12t/streaming/class-predicate-list/exec | 10.5076 | 10.6001 | 1.009 |  |
| 10.5m-12t/eager/class-predicates/exec | 10.0375 | 10.2841 | 1.025 |  |
| 10.5m-12t/streaming/class-predicates/exec | 10.4419 | 10.7224 | 1.027 |  |
| 10.5m-12t/eager/contains/exec | 4.1414 | 4.2557 | 1.028 |  |
| 10.5m-12t/streaming/contains/exec | 4.1481 | 4.2746 | 1.030 |  |
| 10.5m-12t/eager/count-all/exec | 0.0094 | 0.0086 | 0.915 |  |
| 10.5m-12t/streaming/count-all/exec | 0.0126 | 0.0114 | 0.905 |  |
| 10.5m-12t/eager/distinct-join/exec | 38.5522 | 39.2794 | 1.019 |  |
| 10.5m-12t/streaming/distinct-join/exec | 38.8756 | 39.0919 | 1.006 |  |
| 10.5m-12t/eager/distinct-obj/exec | 0.0065 | 0.0067 | 1.031 | slower, short |
| 10.5m-12t/streaming/distinct-obj/exec | 0.0108 | 0.0095 | 0.880 |  |
| 10.5m-12t/eager/employee-docs/exec | 0.0681 | 0.0686 | 1.007 | slower, short |
| 10.5m-12t/streaming/employee-docs/exec | 0.0771 | 0.0766 | 0.994 |  |
| 10.5m-12t/eager/exists-join/exec | 8.9121 | 8.9501 | 1.004 |  |
| 10.5m-12t/streaming/exists-join/exec | 8.9344 | 8.9148 | 0.998 |  |
| 10.5m-12t/eager/export-500k/exec | 2.3284 | 2.198 | 0.944 |  |
| 10.5m-12t/streaming/export-500k/exec | 0.409 | 0.3828 | 0.936 |  |
| 10.5m-12t/eager/expr-agg-arg/exec | 28.5089 | 27.6744 | 0.971 |  |
| 10.5m-12t/streaming/expr-agg-arg/exec | 23.5466 | 23.4144 | 0.994 |  |
| 10.5m-12t/eager/expr-bind-group/exec | 4.7495 | 4.7512 | 1.000 |  |
| 10.5m-12t/streaming/expr-bind-group/exec | 3.4235 | 3.413 | 0.997 |  |
| 10.5m-12t/eager/expr-order-key/exec | 10.4957 | 10.1028 | 0.963 |  |
| 10.5m-12t/streaming/expr-order-key/exec | 11.2336 | 10.9592 | 0.976 |  |
| 10.5m-12t/eager/group-avg/exec | 47.5777 | 45.4409 | 0.955 |  |
| 10.5m-12t/streaming/group-avg/exec | 40.0713 | 37.7191 | 0.941 |  |
| 10.5m-12t/eager/knows-reach/exec | 141.699 | 141.111 | 0.996 |  |
| 10.5m-12t/streaming/knows-reach/exec | 139.594 | 139.26 | 0.998 |  |
| 10.5m-12t/eager/lang-filter/exec | 3.1229 | 3.1237 | 1.000 |  |
| 10.5m-12t/streaming/lang-filter/exec | 3.055 | 3.0565 | 1.000 |  |
| 10.5m-12t/eager/minus/exec | 3.2872 | 4.3026 | 1.309 | slower |
| 10.5m-12t/streaming/minus/exec | 2.5828 | 2.5863 | 1.001 |  |
| 10.5m-12t/eager/not-exists/exec | 9.7005 | 9.755 | 1.006 |  |
| 10.5m-12t/streaming/not-exists/exec | 8.935 | 8.9493 | 1.002 |  |
| 10.5m-12t/eager/optional-chain/exec | 29.3404 | 25.1457 | 0.857 |  |
| 10.5m-12t/streaming/optional-chain/exec | 25.564 | 26.5077 | 1.037 |  |
| 10.5m-12t/eager/optional-count/exec | 5.3604 | 5.4023 | 1.008 |  |
| 10.5m-12t/streaming/optional-count/exec | 5.222 | 5.4524 | 1.044 |  |
| 10.5m-12t/eager/order-by-full/exec | 250.395 | 108.713 | 0.434 |  |
| 10.5m-12t/streaming/order-by-full/exec | 268.349 | 107.363 | 0.400 |  |
| 10.5m-12t/eager/path-plus/exec | 0.0138 | 0.0125 | 0.906 |  |
| 10.5m-12t/streaming/path-plus/exec | 0.0145 | 0.0153 | 1.055 | slower, short |
| 10.5m-12t/eager/predicate-counts/exec | 0.0078 | 0.0067 | 0.859 |  |
| 10.5m-12t/streaming/predicate-counts/exec | 0.0146 | 0.0109 | 0.747 |  |
| 10.5m-12t/eager/predicate-subjects/exec | 0.0093 | 0.0091 | 0.978 |  |
| 10.5m-12t/streaming/predicate-subjects/exec | 0.0154 | 0.0167 | 1.084 | slower, short |
| 10.5m-12t/eager/range-topk/exec | 2.4239 | 2.1106 | 0.871 |  |
| 10.5m-12t/streaming/range-topk/exec | 2.3227 | 2.0712 | 0.892 |  |
| 10.5m-12t/eager/regex-iri/exec | 6.5133 | 6.4991 | 0.998 |  |
| 10.5m-12t/streaming/regex-iri/exec | 6.4436 | 6.503 | 1.009 |  |
| 10.5m-12t/eager/star-join/exec | 13.2103 | 13.3085 | 1.007 |  |
| 10.5m-12t/streaming/star-join/exec | 13.8683 | 13.9195 | 1.004 |  |
| 10.5m-12t/eager/star-lookup/exec | 0.0728 | 0.0734 | 1.008 | slower, short |
| 10.5m-12t/streaming/star-lookup/exec | 0.0839 | 0.0842 | 1.004 | slower, short |
| 10.5m-12t/eager/subquery-agg/exec | 0.8163 | 0.8224 | 1.007 |  |
| 10.5m-12t/streaming/subquery-agg/exec | 0.8099 | 0.7732 | 0.955 |  |
| 10.5m-12t/eager/two-hop-count/exec | 11.1398 | 11.3129 | 1.016 |  |
| 10.5m-12t/streaming/two-hop-count/exec | 11.1219 | 11.1765 | 1.005 |  |
| 10.5m-12t/eager/types-grouped/exec | 0.0122 | 0.0122 | 1.000 |  |
| 10.5m-12t/streaming/types-grouped/exec | 0.0194 | 0.0201 | 1.036 | slower, short |
| 10.5m-12t/eager/values-star/exec | 0.0205 | 0.0207 | 1.010 | slower, short |
| 10.5m-12t/streaming/values-star/exec | 0.028 | 0.0275 | 0.982 |  |

124 metrics compared, 23 flagged, 0 failed checks, 0 missing checks, 0 changed fingerprints.
