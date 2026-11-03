# executors gate: candidate against baseline

- baseline: host forge, commit e52937ca, started 2026-11-02T03:37:44Z
- candidate: host forge, commit final, started 2026-11-02T18:32:08Z
- threshold: 5.0% slower, 0.0% below 0.3 ms
- compared although different: setting run: '31 queries, scales 100000 1000000 people, threads 12 1, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server' against '31 queries, scales 100000 1000000 people, threads 12, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server'
- compared although different: manifest build_config: '{"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{},"mbx_policy":"d8bf0b48d620f6bf62929d3db32bd253653505cdb33a5bd1efed1e79aaca05b4","profile":"release"}' against '{"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{},"mbx_policy":"779a99e845a5ac3f1c4d64a3c95bcc74b33f47418e829bf008542ec2dbbcd151","profile":"release"}'

| metric | baseline | candidate | ratio | flag |
|---|--:|--:|--:|---|
| 1.05m-12t/eager/class-predicate-list/exec | 0.973 | 0.9658 | 0.993 |  |
| 1.05m-12t/streaming/class-predicate-list/exec | 1.005 | 0.9937 | 0.989 |  |
| 1.05m-12t/eager/class-predicates/exec | 0.9814 | 0.9727 | 0.991 |  |
| 1.05m-12t/streaming/class-predicates/exec | 1.0255 | 1.009 | 0.984 |  |
| 1.05m-12t/eager/contains/exec | 0.7176 | 0.7178 | 1.000 |  |
| 1.05m-12t/streaming/contains/exec | 0.7499 | 0.7083 | 0.945 |  |
| 1.05m-12t/eager/count-all/exec | 0.0074 | 0.008 | 1.081 | slower, short |
| 1.05m-12t/streaming/count-all/exec | 0.0105 | 0.0103 | 0.981 |  |
| 1.05m-12t/eager/distinct-join/exec | 3.5292 | 3.4968 | 0.991 |  |
| 1.05m-12t/streaming/distinct-join/exec | 3.8761 | 4.0037 | 1.033 |  |
| 1.05m-12t/eager/distinct-obj/exec | 0.006 | 0.0068 | 1.133 | slower, short |
| 1.05m-12t/streaming/distinct-obj/exec | 0.0091 | 0.0094 | 1.033 | slower, short |
| 1.05m-12t/eager/employee-docs/exec | 0.0494 | 0.0503 | 1.018 | slower, short |
| 1.05m-12t/streaming/employee-docs/exec | 0.0566 | 0.0544 | 0.961 |  |
| 1.05m-12t/eager/exists-join/exec | 1.0915 | 1.1039 | 1.011 |  |
| 1.05m-12t/streaming/exists-join/exec | 1.1348 | 1.141 | 1.005 |  |
| 1.05m-12t/eager/export-500k/exec | 2.4226 | 2.3697 | 0.978 |  |
| 1.05m-12t/streaming/export-500k/exec | 0.4075 | 0.4078 | 1.001 |  |
| 1.05m-12t/eager/expr-agg-arg/exec | 3.0024 | 2.9354 | 0.978 |  |
| 1.05m-12t/streaming/expr-agg-arg/exec | 2.0102 | 2.0795 | 1.034 |  |
| 1.05m-12t/eager/expr-bind-group/exec | 0.8553 | 0.7893 | 0.923 |  |
| 1.05m-12t/streaming/expr-bind-group/exec | 0.3928 | 0.401 | 1.021 |  |
| 1.05m-12t/eager/expr-order-key/exec | 1.2888 | 1.2469 | 0.967 |  |
| 1.05m-12t/streaming/expr-order-key/exec | 1.2076 | 1.2613 | 1.044 |  |
| 1.05m-12t/eager/group-avg/exec | 4.4798 | 4.3705 | 0.976 |  |
| 1.05m-12t/streaming/group-avg/exec | 3.5906 | 3.5623 | 0.992 |  |
| 1.05m-12t/eager/knows-reach/exec | 11.8375 | 11.7889 | 0.996 |  |
| 1.05m-12t/streaming/knows-reach/exec | 11.688 | 11.5946 | 0.992 |  |
| 1.05m-12t/eager/lang-filter/exec | 0.705 | 0.7751 | 1.099 | slower |
| 1.05m-12t/streaming/lang-filter/exec | 0.7477 | 0.7599 | 1.016 |  |
| 1.05m-12t/eager/minus/exec | 0.3522 | 0.3484 | 0.989 |  |
| 1.05m-12t/streaming/minus/exec | 0.2815 | 0.2979 | 1.058 | slower, short |
| 1.05m-12t/eager/not-exists/exec | 0.8651 | 0.8784 | 1.015 |  |
| 1.05m-12t/streaming/not-exists/exec | 0.7986 | 0.8179 | 1.024 |  |
| 1.05m-12t/eager/optional-chain/exec | 2.1357 | 2.1473 | 1.005 |  |
| 1.05m-12t/streaming/optional-chain/exec | 2.0777 | 2.1129 | 1.017 |  |
| 1.05m-12t/eager/optional-count/exec | 0.5425 | 0.5549 | 1.023 |  |
| 1.05m-12t/streaming/optional-count/exec | 0.5556 | 0.5445 | 0.980 |  |
| 1.05m-12t/eager/order-by-full/exec | 14.0955 | 14.1676 | 1.005 |  |
| 1.05m-12t/streaming/order-by-full/exec | 15.7521 | 14.482 | 0.919 |  |
| 1.05m-12t/eager/path-plus/exec | 0.018 | 0.0182 | 1.011 | slower, short |
| 1.05m-12t/streaming/path-plus/exec | 0.0224 | 0.0201 | 0.897 |  |
| 1.05m-12t/eager/predicate-counts/exec | 0.0068 | 0.0061 | 0.897 |  |
| 1.05m-12t/streaming/predicate-counts/exec | 0.011 | 0.0104 | 0.945 |  |
| 1.05m-12t/eager/predicate-subjects/exec | 0.0095 | 0.0106 | 1.116 | slower, short |
| 1.05m-12t/streaming/predicate-subjects/exec | 0.0154 | 0.0148 | 0.961 |  |
| 1.05m-12t/eager/range-topk/exec | 0.7128 | 0.5682 | 0.797 |  |
| 1.05m-12t/streaming/range-topk/exec | 0.5688 | 0.5725 | 1.007 |  |
| 1.05m-12t/eager/regex-iri/exec | 0.9223 | 1.0356 | 1.123 | slower |
| 1.05m-12t/streaming/regex-iri/exec | 0.9973 | 1.0471 | 1.050 |  |
| 1.05m-12t/eager/star-join/exec | 1.5408 | 1.6445 | 1.067 | slower |
| 1.05m-12t/streaming/star-join/exec | 1.6506 | 1.6748 | 1.015 |  |
| 1.05m-12t/eager/star-lookup/exec | 0.0837 | 0.0841 | 1.005 | slower, short |
| 1.05m-12t/streaming/star-lookup/exec | 0.0886 | 0.0888 | 1.002 | slower, short |
| 1.05m-12t/eager/subquery-agg/exec | 0.2389 | 0.2207 | 0.924 |  |
| 1.05m-12t/streaming/subquery-agg/exec | 0.2225 | 0.1884 | 0.847 |  |
| 1.05m-12t/eager/two-hop-count/exec | 1.2677 | 1.2314 | 0.971 |  |
| 1.05m-12t/streaming/two-hop-count/exec | 1.2492 | 1.2518 | 1.002 |  |
| 1.05m-12t/eager/types-grouped/exec | 0.0115 | 0.0122 | 1.061 | slower, short |
| 1.05m-12t/streaming/types-grouped/exec | 0.0193 | 0.0224 | 1.161 | slower, short |
| 1.05m-12t/eager/values-star/exec | 0.0205 | 0.0203 | 0.990 |  |
| 1.05m-12t/streaming/values-star/exec | 0.0262 | 0.0323 | 1.233 | slower, short |
| 10.5m-12t/eager/class-predicate-list/exec | 10.178 | 9.9453 | 0.977 |  |
| 10.5m-12t/streaming/class-predicate-list/exec | 10.4968 | 10.5076 | 1.001 |  |
| 10.5m-12t/eager/class-predicates/exec | 10.1159 | 10.0375 | 0.992 |  |
| 10.5m-12t/streaming/class-predicates/exec | 10.5169 | 10.4419 | 0.993 |  |
| 10.5m-12t/eager/contains/exec | 4.004 | 4.1414 | 1.034 |  |
| 10.5m-12t/streaming/contains/exec | 4.0798 | 4.1481 | 1.017 |  |
| 10.5m-12t/eager/count-all/exec | 0.008 | 0.0094 | 1.175 | slower, short |
| 10.5m-12t/streaming/count-all/exec | 0.0119 | 0.0126 | 1.059 | slower, short |
| 10.5m-12t/eager/distinct-join/exec | 38.5196 | 38.5522 | 1.001 |  |
| 10.5m-12t/streaming/distinct-join/exec | 38.051 | 38.8756 | 1.022 |  |
| 10.5m-12t/eager/distinct-obj/exec | 0.0058 | 0.0065 | 1.121 | slower, short |
| 10.5m-12t/streaming/distinct-obj/exec | 0.0093 | 0.0108 | 1.161 | slower, short |
| 10.5m-12t/eager/employee-docs/exec | 0.0678 | 0.0681 | 1.004 | slower, short |
| 10.5m-12t/streaming/employee-docs/exec | 0.0789 | 0.0771 | 0.977 |  |
| 10.5m-12t/eager/exists-join/exec | 8.852 | 8.9121 | 1.007 |  |
| 10.5m-12t/streaming/exists-join/exec | 8.8247 | 8.9344 | 1.012 |  |
| 10.5m-12t/eager/export-500k/exec | 2.4799 | 2.3284 | 0.939 |  |
| 10.5m-12t/streaming/export-500k/exec | 0.4091 | 0.409 | 1.000 |  |
| 10.5m-12t/eager/expr-agg-arg/exec | 28.0343 | 28.5089 | 1.017 |  |
| 10.5m-12t/streaming/expr-agg-arg/exec | 23.2254 | 23.5466 | 1.014 |  |
| 10.5m-12t/eager/expr-bind-group/exec | 4.8124 | 4.7495 | 0.987 |  |
| 10.5m-12t/streaming/expr-bind-group/exec | 3.4362 | 3.4235 | 0.996 |  |
| 10.5m-12t/eager/expr-order-key/exec | 10.3228 | 10.4957 | 1.017 |  |
| 10.5m-12t/streaming/expr-order-key/exec | 10.7844 | 11.2336 | 1.042 |  |
| 10.5m-12t/eager/group-avg/exec | 47.8198 | 47.5777 | 0.995 |  |
| 10.5m-12t/streaming/group-avg/exec | 40.0891 | 40.0713 | 1.000 |  |
| 10.5m-12t/eager/knows-reach/exec | 142.627 | 141.699 | 0.993 |  |
| 10.5m-12t/streaming/knows-reach/exec | 140.373 | 139.594 | 0.994 |  |
| 10.5m-12t/eager/lang-filter/exec | 2.8064 | 3.1229 | 1.113 | slower |
| 10.5m-12t/streaming/lang-filter/exec | 2.8694 | 3.055 | 1.065 | slower |
| 10.5m-12t/eager/minus/exec | 3.238 | 3.2872 | 1.015 |  |
| 10.5m-12t/streaming/minus/exec | 2.6027 | 2.5828 | 0.992 |  |
| 10.5m-12t/eager/not-exists/exec | 9.7976 | 9.7005 | 0.990 |  |
| 10.5m-12t/streaming/not-exists/exec | 8.9343 | 8.935 | 1.000 |  |
| 10.5m-12t/eager/optional-chain/exec | 28.4546 | 29.3404 | 1.031 |  |
| 10.5m-12t/streaming/optional-chain/exec | 25.5174 | 25.564 | 1.002 |  |
| 10.5m-12t/eager/optional-count/exec | 5.3987 | 5.3604 | 0.993 |  |
| 10.5m-12t/streaming/optional-count/exec | 5.22 | 5.222 | 1.000 |  |
| 10.5m-12t/eager/order-by-full/exec | 239.272 | 250.395 | 1.046 |  |
| 10.5m-12t/streaming/order-by-full/exec | 266.403 | 268.349 | 1.007 |  |
| 10.5m-12t/eager/path-plus/exec | 0.0137 | 0.0138 | 1.007 | slower, short |
| 10.5m-12t/streaming/path-plus/exec | 0.0142 | 0.0145 | 1.021 | slower, short |
| 10.5m-12t/eager/predicate-counts/exec | 0.0068 | 0.0078 | 1.147 | slower, short |
| 10.5m-12t/streaming/predicate-counts/exec | 0.0111 | 0.0146 | 1.315 | slower, short |
| 10.5m-12t/eager/predicate-subjects/exec | 0.0079 | 0.0093 | 1.177 | slower, short |
| 10.5m-12t/streaming/predicate-subjects/exec | 0.0157 | 0.0154 | 0.981 |  |
| 10.5m-12t/eager/range-topk/exec | 2.456 | 2.4239 | 0.987 |  |
| 10.5m-12t/streaming/range-topk/exec | 2.2472 | 2.3227 | 1.034 |  |
| 10.5m-12t/eager/regex-iri/exec | 5.9913 | 6.5133 | 1.087 | slower |
| 10.5m-12t/streaming/regex-iri/exec | 6.2395 | 6.4436 | 1.033 |  |
| 10.5m-12t/eager/star-join/exec | 13.0634 | 13.2103 | 1.011 |  |
| 10.5m-12t/streaming/star-join/exec | 13.5013 | 13.8683 | 1.027 |  |
| 10.5m-12t/eager/star-lookup/exec | 0.0725 | 0.0728 | 1.004 | slower, short |
| 10.5m-12t/streaming/star-lookup/exec | 0.082 | 0.0839 | 1.023 | slower, short |
| 10.5m-12t/eager/subquery-agg/exec | 0.7566 | 0.8163 | 1.079 | slower |
| 10.5m-12t/streaming/subquery-agg/exec | 0.83 | 0.8099 | 0.976 |  |
| 10.5m-12t/eager/two-hop-count/exec | 11.1937 | 11.1398 | 0.995 |  |
| 10.5m-12t/streaming/two-hop-count/exec | 11.2398 | 11.1219 | 0.990 |  |
| 10.5m-12t/eager/types-grouped/exec | 0.0105 | 0.0122 | 1.162 | slower, short |
| 10.5m-12t/streaming/types-grouped/exec | 0.0203 | 0.0194 | 0.956 |  |
| 10.5m-12t/eager/values-star/exec | 0.0202 | 0.0205 | 1.015 | slower, short |
| 10.5m-12t/streaming/values-star/exec | 0.0266 | 0.028 | 1.053 | slower, short |

Checks of the baseline that the candidate lacks: 1.05m-1t/class-predicate-list/answers, 1.05m-1t/class-predicate-list/timed-answers, 1.05m-1t/class-predicates/answers, 1.05m-1t/class-predicates/timed-answers, 1.05m-1t/contains/answers, 1.05m-1t/contains/timed-answers, 1.05m-1t/count-all/answers, 1.05m-1t/count-all/timed-answers, 1.05m-1t/distinct-join/answers, 1.05m-1t/distinct-join/timed-answers, 1.05m-1t/distinct-obj/answers, 1.05m-1t/distinct-obj/timed-answers, 1.05m-1t/employee-docs/answers, 1.05m-1t/employee-docs/timed-answers, 1.05m-1t/exists-join/answers, 1.05m-1t/exists-join/timed-answers, 1.05m-1t/export-500k/answers, 1.05m-1t/export-500k/timed-answers, 1.05m-1t/expr-agg-arg/answers, 1.05m-1t/expr-agg-arg/timed-answers, 1.05m-1t/expr-bind-group/answers, 1.05m-1t/expr-bind-group/timed-answers, 1.05m-1t/expr-order-key/answers, 1.05m-1t/expr-order-key/timed-answers, 1.05m-1t/group-avg/answers, 1.05m-1t/group-avg/timed-answers, 1.05m-1t/knows-reach/answers, 1.05m-1t/knows-reach/timed-answers, 1.05m-1t/lang-filter/answers, 1.05m-1t/lang-filter/timed-answers, 1.05m-1t/minus/answers, 1.05m-1t/minus/timed-answers, 1.05m-1t/not-exists/answers, 1.05m-1t/not-exists/timed-answers, 1.05m-1t/optional-chain/answers, 1.05m-1t/optional-chain/timed-answers, 1.05m-1t/optional-count/answers, 1.05m-1t/optional-count/timed-answers, 1.05m-1t/order-by-full/answers, 1.05m-1t/order-by-full/timed-answers, 1.05m-1t/path-plus/answers, 1.05m-1t/path-plus/timed-answers, 1.05m-1t/predicate-counts/answers, 1.05m-1t/predicate-counts/timed-answers, 1.05m-1t/predicate-subjects/answers, 1.05m-1t/predicate-subjects/timed-answers, 1.05m-1t/range-topk/answers, 1.05m-1t/range-topk/timed-answers, 1.05m-1t/regex-iri/answers, 1.05m-1t/regex-iri/timed-answers, 1.05m-1t/star-join/answers, 1.05m-1t/star-join/timed-answers, 1.05m-1t/star-lookup/answers, 1.05m-1t/star-lookup/timed-answers, 1.05m-1t/subquery-agg/answers, 1.05m-1t/subquery-agg/timed-answers, 1.05m-1t/two-hop-count/answers, 1.05m-1t/two-hop-count/timed-answers, 1.05m-1t/types-grouped/answers, 1.05m-1t/types-grouped/timed-answers, 1.05m-1t/values-star/answers, 1.05m-1t/values-star/timed-answers, 10.5m-1t/class-predicate-list/answers, 10.5m-1t/class-predicate-list/timed-answers, 10.5m-1t/class-predicates/answers, 10.5m-1t/class-predicates/timed-answers, 10.5m-1t/contains/answers, 10.5m-1t/contains/timed-answers, 10.5m-1t/count-all/answers, 10.5m-1t/count-all/timed-answers, 10.5m-1t/distinct-join/answers, 10.5m-1t/distinct-join/timed-answers, 10.5m-1t/distinct-obj/answers, 10.5m-1t/distinct-obj/timed-answers, 10.5m-1t/employee-docs/answers, 10.5m-1t/employee-docs/timed-answers, 10.5m-1t/exists-join/answers, 10.5m-1t/exists-join/timed-answers, 10.5m-1t/export-500k/answers, 10.5m-1t/export-500k/timed-answers, 10.5m-1t/expr-agg-arg/answers, 10.5m-1t/expr-agg-arg/timed-answers, 10.5m-1t/expr-bind-group/answers, 10.5m-1t/expr-bind-group/timed-answers, 10.5m-1t/expr-order-key/answers, 10.5m-1t/expr-order-key/timed-answers, 10.5m-1t/group-avg/answers, 10.5m-1t/group-avg/timed-answers, 10.5m-1t/knows-reach/answers, 10.5m-1t/knows-reach/timed-answers, 10.5m-1t/lang-filter/answers, 10.5m-1t/lang-filter/timed-answers, 10.5m-1t/minus/answers, 10.5m-1t/minus/timed-answers, 10.5m-1t/not-exists/answers, 10.5m-1t/not-exists/timed-answers, 10.5m-1t/optional-chain/answers, 10.5m-1t/optional-chain/timed-answers, 10.5m-1t/optional-count/answers, 10.5m-1t/optional-count/timed-answers, 10.5m-1t/order-by-full/answers, 10.5m-1t/order-by-full/timed-answers, 10.5m-1t/path-plus/answers, 10.5m-1t/path-plus/timed-answers, 10.5m-1t/predicate-counts/answers, 10.5m-1t/predicate-counts/timed-answers, 10.5m-1t/predicate-subjects/answers, 10.5m-1t/predicate-subjects/timed-answers, 10.5m-1t/range-topk/answers, 10.5m-1t/range-topk/timed-answers, 10.5m-1t/regex-iri/answers, 10.5m-1t/regex-iri/timed-answers, 10.5m-1t/star-join/answers, 10.5m-1t/star-join/timed-answers, 10.5m-1t/star-lookup/answers, 10.5m-1t/star-lookup/timed-answers, 10.5m-1t/subquery-agg/answers, 10.5m-1t/subquery-agg/timed-answers, 10.5m-1t/two-hop-count/answers, 10.5m-1t/two-hop-count/timed-answers, 10.5m-1t/types-grouped/answers, 10.5m-1t/types-grouped/timed-answers, 10.5m-1t/values-star/answers, 10.5m-1t/values-star/timed-answers.
124 metrics compared, 34 flagged, 0 failed checks, 124 missing checks, 0 changed fingerprints.
