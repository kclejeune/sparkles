# executors gate: candidate against baseline

- baseline: host forge, commit 04d49b57, started 2026-11-01T10:02:19Z
- candidate: host forge, commit e52937ca, started 2026-11-02T03:37:44Z
- threshold: 5.0% slower, 0.0% below 0.3 ms
- compared although different: manifest build_config: '{"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{"CARGO_INCREMENTAL":"0"},"mbx_policy":"d8bf0b48d620f6bf62929d3db32bd253653505cdb33a5bd1efed1e79aaca05b4","profile":"release"}' against '{"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{},"mbx_policy":"d8bf0b48d620f6bf62929d3db32bd253653505cdb33a5bd1efed1e79aaca05b4","profile":"release"}'

| metric | baseline | candidate | ratio | flag |
|---|--:|--:|--:|---|
| 1.05m-1t/eager/class-predicate-list/exec | 0.9333 | 0.9288 | 0.995 |  |
| 1.05m-1t/streaming/class-predicate-list/exec | 0.9272 | 0.9166 | 0.989 |  |
| 1.05m-1t/eager/class-predicates/exec | 0.9411 | 0.932 | 0.990 |  |
| 1.05m-1t/streaming/class-predicates/exec | 0.9388 | 0.9294 | 0.990 |  |
| 1.05m-1t/eager/contains/exec | 2.4437 | 2.4102 | 0.986 |  |
| 1.05m-1t/streaming/contains/exec | 2.4666 | 2.4732 | 1.003 |  |
| 1.05m-1t/eager/count-all/exec | 0.0042 | 0.0047 | 1.119 | slower, short |
| 1.05m-1t/streaming/count-all/exec | 0.0054 | 0.0055 | 1.019 | slower, short |
| 1.05m-1t/eager/distinct-join/exec | 3.4724 | 3.4881 | 1.005 |  |
| 1.05m-1t/streaming/distinct-join/exec | 3.6834 | 3.6425 | 0.989 |  |
| 1.05m-1t/eager/distinct-obj/exec | 0.0037 | 0.0037 | 1.000 |  |
| 1.05m-1t/streaming/distinct-obj/exec | 0.0045 | 0.005 | 1.111 | slower, short |
| 1.05m-1t/eager/employee-docs/exec | 0.0332 | 0.0361 | 1.087 | slower, short |
| 1.05m-1t/streaming/employee-docs/exec | 0.037 | 0.0404 | 1.092 | slower, short |
| 1.05m-1t/eager/exists-join/exec | 1.0226 | 1.0253 | 1.003 |  |
| 1.05m-1t/streaming/exists-join/exec | 1.0242 | 1.0387 | 1.014 |  |
| 1.05m-1t/eager/export-500k/exec | 2.2801 | 2.2474 | 0.986 |  |
| 1.05m-1t/streaming/export-500k/exec | 0.4108 | 0.4282 | 1.042 |  |
| 1.05m-1t/eager/expr-agg-arg/exec | 2.6341 | 2.5477 | 0.967 |  |
| 1.05m-1t/streaming/expr-agg-arg/exec | 1.9032 | 1.8986 | 0.998 |  |
| 1.05m-1t/eager/expr-bind-group/exec | 0.5878 | 0.5931 | 1.009 |  |
| 1.05m-1t/streaming/expr-bind-group/exec | 0.3667 | 0.3687 | 1.005 |  |
| 1.05m-1t/eager/expr-order-key/exec | 1.1765 | 1.1837 | 1.006 |  |
| 1.05m-1t/streaming/expr-order-key/exec | 1.1051 | 1.1107 | 1.005 |  |
| 1.05m-1t/eager/group-avg/exec | 4.0801 | 4.0122 | 0.983 |  |
| 1.05m-1t/streaming/group-avg/exec | 3.4655 | 3.4463 | 0.994 |  |
| 1.05m-1t/eager/knows-reach/exec | 11.5414 | 11.5309 | 0.999 |  |
| 1.05m-1t/streaming/knows-reach/exec | 11.3666 | 11.3615 | 1.000 |  |
| 1.05m-1t/eager/lang-filter/exec | 1.6236 | 1.606 | 0.989 |  |
| 1.05m-1t/streaming/lang-filter/exec | 1.6179 | 1.6268 | 1.006 |  |
| 1.05m-1t/eager/minus/exec | 0.3071 | 0.3034 | 0.988 |  |
| 1.05m-1t/streaming/minus/exec | 0.2534 | 0.2586 | 1.021 | slower, short |
| 1.05m-1t/eager/not-exists/exec | 0.7929 | 0.7992 | 1.008 |  |
| 1.05m-1t/streaming/not-exists/exec | 0.7271 | 0.7498 | 1.031 |  |
| 1.05m-1t/eager/optional-chain/exec | 1.9797 | 1.9906 | 1.006 |  |
| 1.05m-1t/streaming/optional-chain/exec | 1.9588 | 1.9686 | 1.005 |  |
| 1.05m-1t/eager/optional-count/exec | 0.4877 | 0.4879 | 1.000 |  |
| 1.05m-1t/streaming/optional-count/exec | 0.5158 | 0.5188 | 1.006 |  |
| 1.05m-1t/eager/order-by-full/exec | 48.4766 | 48.8854 | 1.008 |  |
| 1.05m-1t/streaming/order-by-full/exec | 45.2267 | 45.1315 | 0.998 |  |
| 1.05m-1t/eager/path-plus/exec | 0.0102 | 0.0108 | 1.059 | slower, short |
| 1.05m-1t/streaming/path-plus/exec | 0.0116 | 0.0121 | 1.043 | slower, short |
| 1.05m-1t/eager/predicate-counts/exec | 0.0044 | 0.0046 | 1.045 | slower, short |
| 1.05m-1t/streaming/predicate-counts/exec | 0.0062 | 0.0061 | 0.984 |  |
| 1.05m-1t/eager/predicate-subjects/exec | 0.0062 | 0.0068 | 1.097 | slower, short |
| 1.05m-1t/streaming/predicate-subjects/exec | 0.0082 | 0.0082 | 1.000 |  |
| 1.05m-1t/eager/range-topk/exec | 0.6589 | 0.6636 | 1.007 |  |
| 1.05m-1t/streaming/range-topk/exec | 0.527 | 0.5285 | 1.003 |  |
| 1.05m-1t/eager/regex-iri/exec | 3.5715 | 3.5426 | 0.992 |  |
| 1.05m-1t/streaming/regex-iri/exec | 3.6645 | 3.6781 | 1.004 |  |
| 1.05m-1t/eager/star-join/exec | 1.3522 | 1.3829 | 1.023 |  |
| 1.05m-1t/streaming/star-join/exec | 1.3924 | 1.4227 | 1.022 |  |
| 1.05m-1t/eager/star-lookup/exec | 0.0594 | 0.0608 | 1.024 | slower, short |
| 1.05m-1t/streaming/star-lookup/exec | 0.0622 | 0.0659 | 1.059 | slower, short |
| 1.05m-1t/eager/subquery-agg/exec | 0.1828 | 0.188 | 1.028 | slower, short |
| 1.05m-1t/streaming/subquery-agg/exec | 0.181 | 0.183 | 1.011 | slower, short |
| 1.05m-1t/eager/two-hop-count/exec | 2.625 | 2.62 | 0.998 |  |
| 1.05m-1t/streaming/two-hop-count/exec | 2.6153 | 2.6149 | 1.000 |  |
| 1.05m-1t/eager/types-grouped/exec | 0.007 | 0.0074 | 1.057 | slower, short |
| 1.05m-1t/streaming/types-grouped/exec | 0.0126 | 0.0124 | 0.984 |  |
| 1.05m-1t/eager/values-star/exec | 0.0149 | 0.0148 | 0.993 |  |
| 1.05m-1t/streaming/values-star/exec | 0.019 | 0.0189 | 0.995 |  |
| 1.05m-12t/eager/class-predicate-list/exec | 0.9962 | 0.973 | 0.977 |  |
| 1.05m-12t/streaming/class-predicate-list/exec | 1.0154 | 1.005 | 0.990 |  |
| 1.05m-12t/eager/class-predicates/exec | 1.0007 | 0.9814 | 0.981 |  |
| 1.05m-12t/streaming/class-predicates/exec | 1.0141 | 1.0255 | 1.011 |  |
| 1.05m-12t/eager/contains/exec | 0.7524 | 0.7176 | 0.954 |  |
| 1.05m-12t/streaming/contains/exec | 0.7212 | 0.7499 | 1.040 |  |
| 1.05m-12t/eager/count-all/exec | 0.0072 | 0.0074 | 1.028 | slower, short |
| 1.05m-12t/streaming/count-all/exec | 0.0095 | 0.0105 | 1.105 | slower, short |
| 1.05m-12t/eager/distinct-join/exec | 3.5951 | 3.5292 | 0.982 |  |
| 1.05m-12t/streaming/distinct-join/exec | 3.905 | 3.8761 | 0.993 |  |
| 1.05m-12t/eager/distinct-obj/exec | 0.0061 | 0.006 | 0.984 |  |
| 1.05m-12t/streaming/distinct-obj/exec | 0.008 | 0.0091 | 1.137 | slower, short |
| 1.05m-12t/eager/employee-docs/exec | 0.0472 | 0.0494 | 1.047 | slower, short |
| 1.05m-12t/streaming/employee-docs/exec | 0.0513 | 0.0566 | 1.103 | slower, short |
| 1.05m-12t/eager/exists-join/exec | 1.0884 | 1.0915 | 1.003 |  |
| 1.05m-12t/streaming/exists-join/exec | 1.1116 | 1.1348 | 1.021 |  |
| 1.05m-12t/eager/export-500k/exec | 2.5538 | 2.4226 | 0.949 |  |
| 1.05m-12t/streaming/export-500k/exec | 0.4083 | 0.4075 | 0.998 |  |
| 1.05m-12t/eager/expr-agg-arg/exec | 2.999 | 3.0024 | 1.001 |  |
| 1.05m-12t/streaming/expr-agg-arg/exec | 1.9902 | 2.0102 | 1.010 |  |
| 1.05m-12t/eager/expr-bind-group/exec | 0.7522 | 0.8553 | 1.137 | slower |
| 1.05m-12t/streaming/expr-bind-group/exec | 0.3968 | 0.3928 | 0.990 |  |
| 1.05m-12t/eager/expr-order-key/exec | 1.2819 | 1.2888 | 1.005 |  |
| 1.05m-12t/streaming/expr-order-key/exec | 1.1981 | 1.2076 | 1.008 |  |
| 1.05m-12t/eager/group-avg/exec | 4.3459 | 4.4798 | 1.031 |  |
| 1.05m-12t/streaming/group-avg/exec | 3.6116 | 3.5906 | 0.994 |  |
| 1.05m-12t/eager/knows-reach/exec | 11.8241 | 11.8375 | 1.001 |  |
| 1.05m-12t/streaming/knows-reach/exec | 11.5876 | 11.688 | 1.009 |  |
| 1.05m-12t/eager/lang-filter/exec | 0.6546 | 0.705 | 1.077 | slower |
| 1.05m-12t/streaming/lang-filter/exec | 0.7251 | 0.7477 | 1.031 |  |
| 1.05m-12t/eager/minus/exec | 0.345 | 0.3522 | 1.021 |  |
| 1.05m-12t/streaming/minus/exec | 0.275 | 0.2815 | 1.024 | slower, short |
| 1.05m-12t/eager/not-exists/exec | 0.8778 | 0.8651 | 0.986 |  |
| 1.05m-12t/streaming/not-exists/exec | 0.7831 | 0.7986 | 1.020 |  |
| 1.05m-12t/eager/optional-chain/exec | 2.1 | 2.1357 | 1.017 |  |
| 1.05m-12t/streaming/optional-chain/exec | 2.0636 | 2.0777 | 1.007 |  |
| 1.05m-12t/eager/optional-count/exec | 0.5418 | 0.5425 | 1.001 |  |
| 1.05m-12t/streaming/optional-count/exec | 0.5498 | 0.5556 | 1.011 |  |
| 1.05m-12t/eager/order-by-full/exec | 13.953 | 14.0955 | 1.010 |  |
| 1.05m-12t/streaming/order-by-full/exec | 14.2867 | 15.7521 | 1.103 | slower |
| 1.05m-12t/eager/path-plus/exec | 0.0171 | 0.018 | 1.053 | slower, short |
| 1.05m-12t/streaming/path-plus/exec | 0.0216 | 0.0224 | 1.037 | slower, short |
| 1.05m-12t/eager/predicate-counts/exec | 0.0062 | 0.0068 | 1.097 | slower, short |
| 1.05m-12t/streaming/predicate-counts/exec | 0.0109 | 0.011 | 1.009 | slower, short |
| 1.05m-12t/eager/predicate-subjects/exec | 0.0099 | 0.0095 | 0.960 |  |
| 1.05m-12t/streaming/predicate-subjects/exec | 0.0151 | 0.0154 | 1.020 | slower, short |
| 1.05m-12t/eager/range-topk/exec | 0.7195 | 0.7128 | 0.991 |  |
| 1.05m-12t/streaming/range-topk/exec | 0.5581 | 0.5688 | 1.019 |  |
| 1.05m-12t/eager/regex-iri/exec | 0.9221 | 0.9223 | 1.000 |  |
| 1.05m-12t/streaming/regex-iri/exec | 0.9926 | 0.9973 | 1.005 |  |
| 1.05m-12t/eager/star-join/exec | 1.5658 | 1.5408 | 0.984 |  |
| 1.05m-12t/streaming/star-join/exec | 1.604 | 1.6506 | 1.029 |  |
| 1.05m-12t/eager/star-lookup/exec | 0.0814 | 0.0837 | 1.028 | slower, short |
| 1.05m-12t/streaming/star-lookup/exec | 0.0901 | 0.0886 | 0.983 |  |
| 1.05m-12t/eager/subquery-agg/exec | 0.2251 | 0.2389 | 1.061 | slower, short |
| 1.05m-12t/streaming/subquery-agg/exec | 0.2172 | 0.2225 | 1.024 | slower, short |
| 1.05m-12t/eager/two-hop-count/exec | 1.2353 | 1.2677 | 1.026 |  |
| 1.05m-12t/streaming/two-hop-count/exec | 1.3163 | 1.2492 | 0.949 |  |
| 1.05m-12t/eager/types-grouped/exec | 0.0105 | 0.0115 | 1.095 | slower, short |
| 1.05m-12t/streaming/types-grouped/exec | 0.0184 | 0.0193 | 1.049 | slower, short |
| 1.05m-12t/eager/values-star/exec | 0.0197 | 0.0205 | 1.041 | slower, short |
| 1.05m-12t/streaming/values-star/exec | 0.0247 | 0.0262 | 1.061 | slower, short |
| 10.5m-1t/eager/class-predicate-list/exec | 9.9817 | 9.8319 | 0.985 |  |
| 10.5m-1t/streaming/class-predicate-list/exec | 10.4373 | 10.2733 | 0.984 |  |
| 10.5m-1t/eager/class-predicates/exec | 9.9978 | 9.8483 | 0.985 |  |
| 10.5m-1t/streaming/class-predicates/exec | 10.4525 | 10.2449 | 0.980 |  |
| 10.5m-1t/eager/contains/exec | 23.8246 | 23.4661 | 0.985 |  |
| 10.5m-1t/streaming/contains/exec | 24.0997 | 24.0445 | 0.998 |  |
| 10.5m-1t/eager/count-all/exec | 0.0055 | 0.0058 | 1.055 | slower, short |
| 10.5m-1t/streaming/count-all/exec | 0.0061 | 0.0068 | 1.115 | slower, short |
| 10.5m-1t/eager/distinct-join/exec | 39.0551 | 38.2491 | 0.979 |  |
| 10.5m-1t/streaming/distinct-join/exec | 38.5781 | 37.9224 | 0.983 |  |
| 10.5m-1t/eager/distinct-obj/exec | 0.0043 | 0.0039 | 0.907 |  |
| 10.5m-1t/streaming/distinct-obj/exec | 0.0046 | 0.005 | 1.087 | slower, short |
| 10.5m-1t/eager/employee-docs/exec | 0.0414 | 0.0482 | 1.164 | slower, short |
| 10.5m-1t/streaming/employee-docs/exec | 0.0452 | 0.0535 | 1.184 | slower, short |
| 10.5m-1t/eager/exists-join/exec | 8.5727 | 8.5635 | 0.999 |  |
| 10.5m-1t/streaming/exists-join/exec | 8.5868 | 8.5601 | 0.997 |  |
| 10.5m-1t/eager/export-500k/exec | 2.3581 | 2.2178 | 0.941 |  |
| 10.5m-1t/streaming/export-500k/exec | 0.4378 | 0.4425 | 1.011 |  |
| 10.5m-1t/eager/expr-agg-arg/exec | 29.3539 | 29.4458 | 1.003 |  |
| 10.5m-1t/streaming/expr-agg-arg/exec | 22.898 | 23.0005 | 1.004 |  |
| 10.5m-1t/eager/expr-bind-group/exec | 5.5835 | 5.5701 | 0.998 |  |
| 10.5m-1t/streaming/expr-bind-group/exec | 3.2885 | 3.2995 | 1.003 |  |
| 10.5m-1t/eager/expr-order-key/exec | 11.9447 | 11.8812 | 0.995 |  |
| 10.5m-1t/streaming/expr-order-key/exec | 10.3843 | 10.5345 | 1.014 |  |
| 10.5m-1t/eager/group-avg/exec | 47.1977 | 47.5151 | 1.007 |  |
| 10.5m-1t/streaming/group-avg/exec | 39.198 | 39.3425 | 1.004 |  |
| 10.5m-1t/eager/knows-reach/exec | 139.139 | 141.565 | 1.017 |  |
| 10.5m-1t/streaming/knows-reach/exec | 137.255 | 137.47 | 1.002 |  |
| 10.5m-1t/eager/lang-filter/exec | 15.8707 | 15.6537 | 0.986 |  |
| 10.5m-1t/streaming/lang-filter/exec | 15.8142 | 15.9092 | 1.006 |  |
| 10.5m-1t/eager/minus/exec | 3.2348 | 3.1705 | 0.980 |  |
| 10.5m-1t/streaming/minus/exec | 2.4235 | 2.4382 | 1.006 |  |
| 10.5m-1t/eager/not-exists/exec | 9.511 | 9.7704 | 1.027 |  |
| 10.5m-1t/streaming/not-exists/exec | 8.5796 | 8.8687 | 1.034 |  |
| 10.5m-1t/eager/optional-chain/exec | 27.8718 | 29.844 | 1.071 | slower |
| 10.5m-1t/streaming/optional-chain/exec | 23.0219 | 22.8527 | 0.993 |  |
| 10.5m-1t/eager/optional-count/exec | 5.3247 | 5.3228 | 1.000 |  |
| 10.5m-1t/streaming/optional-count/exec | 4.9931 | 5.0155 | 1.004 |  |
| 10.5m-1t/eager/order-by-full/exec | 676.12 | 677.315 | 1.002 |  |
| 10.5m-1t/streaming/order-by-full/exec | 561.392 | 596.671 | 1.063 | slower |
| 10.5m-1t/eager/path-plus/exec | 0.0079 | 0.0089 | 1.127 | slower, short |
| 10.5m-1t/streaming/path-plus/exec | 0.0084 | 0.0091 | 1.083 | slower, short |
| 10.5m-1t/eager/predicate-counts/exec | 0.0046 | 0.0047 | 1.022 | slower, short |
| 10.5m-1t/streaming/predicate-counts/exec | 0.0064 | 0.0064 | 1.000 |  |
| 10.5m-1t/eager/predicate-subjects/exec | 0.0063 | 0.0063 | 1.000 |  |
| 10.5m-1t/streaming/predicate-subjects/exec | 0.0083 | 0.0085 | 1.024 | slower, short |
| 10.5m-1t/eager/range-topk/exec | 5.8192 | 5.8508 | 1.005 |  |
| 10.5m-1t/streaming/range-topk/exec | 4.9923 | 5.0948 | 1.021 |  |
| 10.5m-1t/eager/regex-iri/exec | 35.9948 | 35.5455 | 0.988 |  |
| 10.5m-1t/streaming/regex-iri/exec | 36.6534 | 36.7402 | 1.002 |  |
| 10.5m-1t/eager/star-join/exec | 13.5875 | 13.8913 | 1.022 |  |
| 10.5m-1t/streaming/star-join/exec | 14.2828 | 14.5118 | 1.016 |  |
| 10.5m-1t/eager/star-lookup/exec | 0.0521 | 0.0556 | 1.067 | slower, short |
| 10.5m-1t/streaming/star-lookup/exec | 0.053 | 0.0605 | 1.142 | slower, short |
| 10.5m-1t/eager/subquery-agg/exec | 1.6405 | 1.6531 | 1.008 |  |
| 10.5m-1t/streaming/subquery-agg/exec | 1.6198 | 1.6389 | 1.012 |  |
| 10.5m-1t/eager/two-hop-count/exec | 27.7318 | 27.6608 | 0.997 |  |
| 10.5m-1t/streaming/two-hop-count/exec | 27.7689 | 27.7318 | 0.999 |  |
| 10.5m-1t/eager/types-grouped/exec | 0.0072 | 0.0074 | 1.028 | slower, short |
| 10.5m-1t/streaming/types-grouped/exec | 0.0128 | 0.0134 | 1.047 | slower, short |
| 10.5m-1t/eager/values-star/exec | 0.0152 | 0.0168 | 1.105 | slower, short |
| 10.5m-1t/streaming/values-star/exec | 0.0204 | 0.0196 | 0.961 |  |
| 10.5m-12t/eager/class-predicate-list/exec | 10.1889 | 10.178 | 0.999 |  |
| 10.5m-12t/streaming/class-predicate-list/exec | 10.706 | 10.4968 | 0.980 |  |
| 10.5m-12t/eager/class-predicates/exec | 10.235 | 10.1159 | 0.988 |  |
| 10.5m-12t/streaming/class-predicates/exec | 10.7961 | 10.5169 | 0.974 |  |
| 10.5m-12t/eager/contains/exec | 3.991 | 4.004 | 1.003 |  |
| 10.5m-12t/streaming/contains/exec | 4.1265 | 4.0798 | 0.989 |  |
| 10.5m-12t/eager/count-all/exec | 0.0081 | 0.008 | 0.988 |  |
| 10.5m-12t/streaming/count-all/exec | 0.0111 | 0.0119 | 1.072 | slower, short |
| 10.5m-12t/eager/distinct-join/exec | 39.181 | 38.5196 | 0.983 |  |
| 10.5m-12t/streaming/distinct-join/exec | 38.9121 | 38.051 | 0.978 |  |
| 10.5m-12t/eager/distinct-obj/exec | 0.0054 | 0.0058 | 1.074 | slower, short |
| 10.5m-12t/streaming/distinct-obj/exec | 0.0083 | 0.0093 | 1.120 | slower, short |
| 10.5m-12t/eager/employee-docs/exec | 0.0579 | 0.0678 | 1.171 | slower, short |
| 10.5m-12t/streaming/employee-docs/exec | 0.0738 | 0.0789 | 1.069 | slower, short |
| 10.5m-12t/eager/exists-join/exec | 8.9835 | 8.852 | 0.985 |  |
| 10.5m-12t/streaming/exists-join/exec | 8.9413 | 8.8247 | 0.987 |  |
| 10.5m-12t/eager/export-500k/exec | 2.364 | 2.4799 | 1.049 |  |
| 10.5m-12t/streaming/export-500k/exec | 0.4144 | 0.4091 | 0.987 |  |
| 10.5m-12t/eager/expr-agg-arg/exec | 28.3998 | 28.0343 | 0.987 |  |
| 10.5m-12t/streaming/expr-agg-arg/exec | 23.1432 | 23.2254 | 1.004 |  |
| 10.5m-12t/eager/expr-bind-group/exec | 4.7325 | 4.8124 | 1.017 |  |
| 10.5m-12t/streaming/expr-bind-group/exec | 3.4127 | 3.4362 | 1.007 |  |
| 10.5m-12t/eager/expr-order-key/exec | 10.2428 | 10.3228 | 1.008 |  |
| 10.5m-12t/streaming/expr-order-key/exec | 10.8405 | 10.7844 | 0.995 |  |
| 10.5m-12t/eager/group-avg/exec | 48.1104 | 47.8198 | 0.994 |  |
| 10.5m-12t/streaming/group-avg/exec | 40.3646 | 40.0891 | 0.993 |  |
| 10.5m-12t/eager/knows-reach/exec | 141.045 | 142.627 | 1.011 |  |
| 10.5m-12t/streaming/knows-reach/exec | 139.427 | 140.373 | 1.007 |  |
| 10.5m-12t/eager/lang-filter/exec | 2.8272 | 2.8064 | 0.993 |  |
| 10.5m-12t/streaming/lang-filter/exec | 2.9245 | 2.8694 | 0.981 |  |
| 10.5m-12t/eager/minus/exec | 3.277 | 3.238 | 0.988 |  |
| 10.5m-12t/streaming/minus/exec | 2.5804 | 2.6027 | 1.009 |  |
| 10.5m-12t/eager/not-exists/exec | 9.6889 | 9.7976 | 1.011 |  |
| 10.5m-12t/streaming/not-exists/exec | 9.0847 | 8.9343 | 0.983 |  |
| 10.5m-12t/eager/optional-chain/exec | 27.4373 | 28.4546 | 1.037 |  |
| 10.5m-12t/streaming/optional-chain/exec | 24.9074 | 25.5174 | 1.024 |  |
| 10.5m-12t/eager/optional-count/exec | 5.3744 | 5.3987 | 1.005 |  |
| 10.5m-12t/streaming/optional-count/exec | 5.2013 | 5.22 | 1.004 |  |
| 10.5m-12t/eager/order-by-full/exec | 181.829 | 239.272 | 1.316 | slower |
| 10.5m-12t/streaming/order-by-full/exec | 158.978 | 266.403 | 1.676 | slower |
| 10.5m-12t/eager/path-plus/exec | 0.0127 | 0.0137 | 1.079 | slower, short |
| 10.5m-12t/streaming/path-plus/exec | 0.0147 | 0.0142 | 0.966 |  |
| 10.5m-12t/eager/predicate-counts/exec | 0.0066 | 0.0068 | 1.030 | slower, short |
| 10.5m-12t/streaming/predicate-counts/exec | 0.0104 | 0.0111 | 1.067 | slower, short |
| 10.5m-12t/eager/predicate-subjects/exec | 0.0096 | 0.0079 | 0.823 |  |
| 10.5m-12t/streaming/predicate-subjects/exec | 0.0153 | 0.0157 | 1.026 | slower, short |
| 10.5m-12t/eager/range-topk/exec | 2.275 | 2.456 | 1.080 | slower |
| 10.5m-12t/streaming/range-topk/exec | 2.2399 | 2.2472 | 1.003 |  |
| 10.5m-12t/eager/regex-iri/exec | 5.9074 | 5.9913 | 1.014 |  |
| 10.5m-12t/streaming/regex-iri/exec | 6.2451 | 6.2395 | 0.999 |  |
| 10.5m-12t/eager/star-join/exec | 12.7757 | 13.0634 | 1.023 |  |
| 10.5m-12t/streaming/star-join/exec | 13.4632 | 13.5013 | 1.003 |  |
| 10.5m-12t/eager/star-lookup/exec | 0.0663 | 0.0725 | 1.094 | slower, short |
| 10.5m-12t/streaming/star-lookup/exec | 0.0726 | 0.082 | 1.129 | slower, short |
| 10.5m-12t/eager/subquery-agg/exec | 0.7134 | 0.7566 | 1.061 | slower |
| 10.5m-12t/streaming/subquery-agg/exec | 0.8065 | 0.83 | 1.029 |  |
| 10.5m-12t/eager/two-hop-count/exec | 11.241 | 11.1937 | 0.996 |  |
| 10.5m-12t/streaming/two-hop-count/exec | 11.2695 | 11.2398 | 0.997 |  |
| 10.5m-12t/eager/types-grouped/exec | 0.0101 | 0.0105 | 1.040 | slower, short |
| 10.5m-12t/streaming/types-grouped/exec | 0.0189 | 0.0203 | 1.074 | slower, short |
| 10.5m-12t/eager/values-star/exec | 0.0191 | 0.0202 | 1.058 | slower, short |
| 10.5m-12t/streaming/values-star/exec | 0.0275 | 0.0266 | 0.967 |  |

248 metrics compared, 70 flagged, 0 failed checks, 0 missing checks, 0 changed fingerprints.
