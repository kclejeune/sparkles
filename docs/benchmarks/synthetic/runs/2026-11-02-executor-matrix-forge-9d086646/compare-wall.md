# executors gate: candidate against baseline

- baseline: host forge, commit e52937ca, started 2026-11-02T03:37:44Z
- candidate: host forge, commit final, started 2026-11-02T18:32:08Z
- threshold: 5.0% slower, floor 5.0, 0.0% below 0.3 ms
- compared although different: setting run: '31 queries, scales 100000 1000000 people, threads 12 1, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server' against '31 queries, scales 100000 1000000 people, threads 12, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server'
- compared although different: manifest build_config: '{"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{},"mbx_policy":"d8bf0b48d620f6bf62929d3db32bd253653505cdb33a5bd1efed1e79aaca05b4","profile":"release"}' against '{"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{},"mbx_policy":"779a99e845a5ac3f1c4d64a3c95bcc74b33f47418e829bf008542ec2dbbcd151","profile":"release"}'

| metric | baseline | candidate | ratio | flag |
|---|--:|--:|--:|---|
| 1.05m-12t/streaming/distinct-join/wall | 5.6865 | 5.613 | 0.987 |  |
| 1.05m-12t/eager/export-500k/wall | 220.028 | 249.233 | 1.133 | slower |
| 1.05m-12t/streaming/export-500k/wall | 295.875 | 279.998 | 0.946 |  |
| 1.05m-12t/eager/group-avg/wall | 5.3145 | 5.1765 | 0.974 |  |
| 1.05m-12t/eager/knows-reach/wall | 12.9415 | 12.815 | 0.990 |  |
| 1.05m-12t/streaming/knows-reach/wall | 12.733 | 12.6405 | 0.993 |  |
| 1.05m-12t/eager/optional-chain/wall | 13.7395 | 14.663 | 1.067 | slower |
| 1.05m-12t/streaming/optional-chain/wall | 18.9335 | 17.795 | 0.940 |  |
| 1.05m-12t/eager/order-by-full/wall | 46.0465 | 48.6285 | 1.056 | slower |
| 1.05m-12t/streaming/order-by-full/wall | 58.524 | 54.14 | 0.925 |  |
| 1.05m-12t/streaming/star-join/wall | 6.753 | 5.7615 | 0.853 |  |
| 10.5m-12t/eager/class-predicate-list/wall | 11.2745 | 11.068 | 0.982 |  |
| 10.5m-12t/streaming/class-predicate-list/wall | 11.5755 | 11.5815 | 1.001 |  |
| 10.5m-12t/eager/class-predicates/wall | 11.1855 | 11.1385 | 0.996 |  |
| 10.5m-12t/streaming/class-predicates/wall | 11.6275 | 11.6095 | 0.998 |  |
| 10.5m-12t/streaming/contains/wall | 5.0535 | 5.119 | 1.013 |  |
| 10.5m-12t/eager/distinct-join/wall | 43.6555 | 44.1925 | 1.012 |  |
| 10.5m-12t/streaming/distinct-join/wall | 44.433 | 44.8595 | 1.010 |  |
| 10.5m-12t/eager/exists-join/wall | 9.789 | 10.079 | 1.030 |  |
| 10.5m-12t/streaming/exists-join/wall | 9.988 | 10.088 | 1.010 |  |
| 10.5m-12t/eager/export-500k/wall | 228.08 | 253.042 | 1.109 | slower |
| 10.5m-12t/streaming/export-500k/wall | 292.974 | 271.828 | 0.928 |  |
| 10.5m-12t/eager/expr-agg-arg/wall | 29.171 | 29.6855 | 1.018 |  |
| 10.5m-12t/streaming/expr-agg-arg/wall | 24.481 | 24.712 | 1.009 |  |
| 10.5m-12t/eager/expr-bind-group/wall | 5.6395 | 5.567 | 0.987 |  |
| 10.5m-12t/eager/expr-order-key/wall | 12.107 | 12.2665 | 1.013 |  |
| 10.5m-12t/streaming/expr-order-key/wall | 11.844 | 12.3045 | 1.039 |  |
| 10.5m-12t/eager/group-avg/wall | 49.3805 | 49.0725 | 0.994 |  |
| 10.5m-12t/streaming/group-avg/wall | 41.511 | 41.388 | 0.997 |  |
| 10.5m-12t/eager/knows-reach/wall | 144.315 | 143.02 | 0.991 |  |
| 10.5m-12t/streaming/knows-reach/wall | 141.679 | 140.894 | 0.994 |  |
| 10.5m-12t/eager/not-exists/wall | 10.8415 | 10.721 | 0.989 |  |
| 10.5m-12t/streaming/not-exists/wall | 10.0015 | 9.973 | 0.997 |  |
| 10.5m-12t/eager/optional-chain/wall | 130.174 | 141.6 | 1.088 | slower |
| 10.5m-12t/streaming/optional-chain/wall | 171.385 | 161.96 | 0.945 |  |
| 10.5m-12t/eager/optional-count/wall | 6.247 | 6.229 | 0.997 |  |
| 10.5m-12t/streaming/optional-count/wall | 6.236 | 6.242 | 1.001 |  |
| 10.5m-12t/eager/order-by-full/wall | 577.207 | 595.639 | 1.032 |  |
| 10.5m-12t/streaming/order-by-full/wall | 668.779 | 649.162 | 0.971 |  |
| 10.5m-12t/eager/regex-iri/wall | 6.7925 | 7.3205 | 1.078 | slower |
| 10.5m-12t/streaming/regex-iri/wall | 7.3595 | 7.453 | 1.013 |  |
| 10.5m-12t/eager/star-join/wall | 32.1245 | 33.165 | 1.032 |  |
| 10.5m-12t/streaming/star-join/wall | 43.77 | 42.378 | 0.968 |  |
| 10.5m-12t/eager/two-hop-count/wall | 12.369 | 12.318 | 0.996 |  |
| 10.5m-12t/streaming/two-hop-count/wall | 12.401 | 12.2725 | 0.990 |  |

Checks of the baseline that the candidate lacks: 1.05m-1t/class-predicate-list/answers, 1.05m-1t/class-predicate-list/timed-answers, 1.05m-1t/class-predicates/answers, 1.05m-1t/class-predicates/timed-answers, 1.05m-1t/contains/answers, 1.05m-1t/contains/timed-answers, 1.05m-1t/count-all/answers, 1.05m-1t/count-all/timed-answers, 1.05m-1t/distinct-join/answers, 1.05m-1t/distinct-join/timed-answers, 1.05m-1t/distinct-obj/answers, 1.05m-1t/distinct-obj/timed-answers, 1.05m-1t/employee-docs/answers, 1.05m-1t/employee-docs/timed-answers, 1.05m-1t/exists-join/answers, 1.05m-1t/exists-join/timed-answers, 1.05m-1t/export-500k/answers, 1.05m-1t/export-500k/timed-answers, 1.05m-1t/expr-agg-arg/answers, 1.05m-1t/expr-agg-arg/timed-answers, 1.05m-1t/expr-bind-group/answers, 1.05m-1t/expr-bind-group/timed-answers, 1.05m-1t/expr-order-key/answers, 1.05m-1t/expr-order-key/timed-answers, 1.05m-1t/group-avg/answers, 1.05m-1t/group-avg/timed-answers, 1.05m-1t/knows-reach/answers, 1.05m-1t/knows-reach/timed-answers, 1.05m-1t/lang-filter/answers, 1.05m-1t/lang-filter/timed-answers, 1.05m-1t/minus/answers, 1.05m-1t/minus/timed-answers, 1.05m-1t/not-exists/answers, 1.05m-1t/not-exists/timed-answers, 1.05m-1t/optional-chain/answers, 1.05m-1t/optional-chain/timed-answers, 1.05m-1t/optional-count/answers, 1.05m-1t/optional-count/timed-answers, 1.05m-1t/order-by-full/answers, 1.05m-1t/order-by-full/timed-answers, 1.05m-1t/path-plus/answers, 1.05m-1t/path-plus/timed-answers, 1.05m-1t/predicate-counts/answers, 1.05m-1t/predicate-counts/timed-answers, 1.05m-1t/predicate-subjects/answers, 1.05m-1t/predicate-subjects/timed-answers, 1.05m-1t/range-topk/answers, 1.05m-1t/range-topk/timed-answers, 1.05m-1t/regex-iri/answers, 1.05m-1t/regex-iri/timed-answers, 1.05m-1t/star-join/answers, 1.05m-1t/star-join/timed-answers, 1.05m-1t/star-lookup/answers, 1.05m-1t/star-lookup/timed-answers, 1.05m-1t/subquery-agg/answers, 1.05m-1t/subquery-agg/timed-answers, 1.05m-1t/two-hop-count/answers, 1.05m-1t/two-hop-count/timed-answers, 1.05m-1t/types-grouped/answers, 1.05m-1t/types-grouped/timed-answers, 1.05m-1t/values-star/answers, 1.05m-1t/values-star/timed-answers, 10.5m-1t/class-predicate-list/answers, 10.5m-1t/class-predicate-list/timed-answers, 10.5m-1t/class-predicates/answers, 10.5m-1t/class-predicates/timed-answers, 10.5m-1t/contains/answers, 10.5m-1t/contains/timed-answers, 10.5m-1t/count-all/answers, 10.5m-1t/count-all/timed-answers, 10.5m-1t/distinct-join/answers, 10.5m-1t/distinct-join/timed-answers, 10.5m-1t/distinct-obj/answers, 10.5m-1t/distinct-obj/timed-answers, 10.5m-1t/employee-docs/answers, 10.5m-1t/employee-docs/timed-answers, 10.5m-1t/exists-join/answers, 10.5m-1t/exists-join/timed-answers, 10.5m-1t/export-500k/answers, 10.5m-1t/export-500k/timed-answers, 10.5m-1t/expr-agg-arg/answers, 10.5m-1t/expr-agg-arg/timed-answers, 10.5m-1t/expr-bind-group/answers, 10.5m-1t/expr-bind-group/timed-answers, 10.5m-1t/expr-order-key/answers, 10.5m-1t/expr-order-key/timed-answers, 10.5m-1t/group-avg/answers, 10.5m-1t/group-avg/timed-answers, 10.5m-1t/knows-reach/answers, 10.5m-1t/knows-reach/timed-answers, 10.5m-1t/lang-filter/answers, 10.5m-1t/lang-filter/timed-answers, 10.5m-1t/minus/answers, 10.5m-1t/minus/timed-answers, 10.5m-1t/not-exists/answers, 10.5m-1t/not-exists/timed-answers, 10.5m-1t/optional-chain/answers, 10.5m-1t/optional-chain/timed-answers, 10.5m-1t/optional-count/answers, 10.5m-1t/optional-count/timed-answers, 10.5m-1t/order-by-full/answers, 10.5m-1t/order-by-full/timed-answers, 10.5m-1t/path-plus/answers, 10.5m-1t/path-plus/timed-answers, 10.5m-1t/predicate-counts/answers, 10.5m-1t/predicate-counts/timed-answers, 10.5m-1t/predicate-subjects/answers, 10.5m-1t/predicate-subjects/timed-answers, 10.5m-1t/range-topk/answers, 10.5m-1t/range-topk/timed-answers, 10.5m-1t/regex-iri/answers, 10.5m-1t/regex-iri/timed-answers, 10.5m-1t/star-join/answers, 10.5m-1t/star-join/timed-answers, 10.5m-1t/star-lookup/answers, 10.5m-1t/star-lookup/timed-answers, 10.5m-1t/subquery-agg/answers, 10.5m-1t/subquery-agg/timed-answers, 10.5m-1t/two-hop-count/answers, 10.5m-1t/two-hop-count/timed-answers, 10.5m-1t/types-grouped/answers, 10.5m-1t/types-grouped/timed-answers, 10.5m-1t/values-star/answers, 10.5m-1t/values-star/timed-answers.
45 metrics compared, 6 flagged, 0 failed checks, 124 missing checks, 0 changed fingerprints.
