# executors gate: candidate against baseline

- baseline: host forge, commit 04d49b57, started 2026-11-01T10:02:19Z
- candidate: host forge, commit e52937ca, started 2026-11-02T03:37:44Z
- threshold: 5.0% slower, floor 5.0, 0.0% below 0.3 ms
- compared although different: manifest build_config: '{"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{"CARGO_INCREMENTAL":"0"},"mbx_policy":"d8bf0b48d620f6bf62929d3db32bd253653505cdb33a5bd1efed1e79aaca05b4","profile":"release"}' against '{"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{},"mbx_policy":"d8bf0b48d620f6bf62929d3db32bd253653505cdb33a5bd1efed1e79aaca05b4","profile":"release"}'

| metric | baseline | candidate | ratio | flag |
|---|--:|--:|--:|---|
| 1.05m-1t/eager/distinct-join/wall | 5.051 | 5.051 | 1.000 |  |
| 1.05m-1t/streaming/distinct-join/wall | 5.58 | 5.5415 | 0.993 |  |
| 1.05m-1t/eager/export-500k/wall | 264.968 | 234.044 | 0.883 |  |
| 1.05m-1t/streaming/export-500k/wall | 370.17 | 364.969 | 0.986 |  |
| 1.05m-1t/eager/group-avg/wall | 5.8505 | 5.4575 | 0.933 |  |
| 1.05m-1t/eager/knows-reach/wall | 12.761 | 12.7595 | 1.000 |  |
| 1.05m-1t/streaming/knows-reach/wall | 12.598 | 12.571 | 0.998 |  |
| 1.05m-1t/eager/optional-chain/wall | 15.1335 | 14.1295 | 0.934 |  |
| 1.05m-1t/streaming/optional-chain/wall | 22.04 | 22.043 | 1.000 |  |
| 1.05m-1t/eager/order-by-full/wall | 85.541 | 84.0195 | 0.982 |  |
| 1.05m-1t/streaming/order-by-full/wall | 101.487 | 101.418 | 0.999 |  |
| 1.05m-1t/streaming/star-join/wall | 6.4075 | 6.4545 | 1.007 |  |
| 1.05m-12t/streaming/distinct-join/wall | 5.649 | 5.6865 | 1.007 |  |
| 1.05m-12t/eager/export-500k/wall | 246.856 | 220.028 | 0.891 |  |
| 1.05m-12t/streaming/export-500k/wall | 298.73 | 295.875 | 0.990 |  |
| 1.05m-12t/eager/group-avg/wall | 5.188 | 5.3145 | 1.024 |  |
| 1.05m-12t/eager/knows-reach/wall | 12.911 | 12.9415 | 1.002 |  |
| 1.05m-12t/streaming/knows-reach/wall | 12.703 | 12.733 | 1.002 |  |
| 1.05m-12t/eager/optional-chain/wall | 14.79 | 13.7395 | 0.929 |  |
| 1.05m-12t/streaming/optional-chain/wall | 18.7875 | 18.9335 | 1.008 |  |
| 1.05m-12t/eager/order-by-full/wall | 48.822 | 46.0465 | 0.943 |  |
| 1.05m-12t/streaming/order-by-full/wall | 57.0925 | 58.524 | 1.025 |  |
| 1.05m-12t/streaming/star-join/wall | 6.658 | 6.753 | 1.014 |  |
| 10.5m-1t/eager/class-predicate-list/wall | 11.283 | 11.1405 | 0.987 |  |
| 10.5m-1t/streaming/class-predicate-list/wall | 11.7445 | 11.5865 | 0.987 |  |
| 10.5m-1t/eager/class-predicates/wall | 11.3475 | 11.153 | 0.983 |  |
| 10.5m-1t/streaming/class-predicates/wall | 11.791 | 11.666 | 0.989 |  |
| 10.5m-1t/eager/contains/wall | 25.028 | 24.5565 | 0.981 |  |
| 10.5m-1t/streaming/contains/wall | 25.3165 | 25.316 | 1.000 |  |
| 10.5m-1t/eager/distinct-join/wall | 44.484 | 43.4435 | 0.977 |  |
| 10.5m-1t/streaming/distinct-join/wall | 46.126 | 45.7575 | 0.992 |  |
| 10.5m-1t/eager/exists-join/wall | 9.921 | 9.9195 | 1.000 |  |
| 10.5m-1t/streaming/exists-join/wall | 9.9235 | 9.9535 | 1.003 |  |
| 10.5m-1t/eager/export-500k/wall | 271.946 | 241.608 | 0.888 |  |
| 10.5m-1t/streaming/export-500k/wall | 369.978 | 365.974 | 0.989 |  |
| 10.5m-1t/eager/expr-agg-arg/wall | 30.7045 | 30.915 | 1.007 |  |
| 10.5m-1t/streaming/expr-agg-arg/wall | 24.0085 | 24.3075 | 1.012 |  |
| 10.5m-1t/eager/expr-bind-group/wall | 6.8265 | 6.847 | 1.003 |  |
| 10.5m-1t/eager/expr-order-key/wall | 13.937 | 13.9325 | 1.000 |  |
| 10.5m-1t/streaming/expr-order-key/wall | 11.621 | 11.8415 | 1.019 |  |
| 10.5m-1t/eager/group-avg/wall | 48.7525 | 49.1275 | 1.008 |  |
| 10.5m-1t/streaming/group-avg/wall | 40.7245 | 40.9035 | 1.004 |  |
| 10.5m-1t/eager/knows-reach/wall | 140.482 | 142.966 | 1.018 |  |
| 10.5m-1t/streaming/knows-reach/wall | 138.602 | 138.848 | 1.002 |  |
| 10.5m-1t/eager/lang-filter/wall | 17.095 | 16.9 | 0.989 |  |
| 10.5m-1t/streaming/lang-filter/wall | 17.051 | 17.1765 | 1.007 |  |
| 10.5m-1t/eager/not-exists/wall | 10.7445 | 10.991 | 1.023 |  |
| 10.5m-1t/streaming/not-exists/wall | 9.911 | 10.0605 | 1.015 |  |
| 10.5m-1t/eager/optional-chain/wall | 145.118 | 135.058 | 0.931 |  |
| 10.5m-1t/streaming/optional-chain/wall | 218.977 | 217.966 | 0.995 |  |
| 10.5m-1t/eager/optional-count/wall | 6.5785 | 6.762 | 1.028 |  |
| 10.5m-1t/streaming/optional-count/wall | 6.267 | 6.2835 | 1.003 |  |
| 10.5m-1t/eager/order-by-full/wall | 1036.16 | 1010.51 | 0.975 |  |
| 10.5m-1t/streaming/order-by-full/wall | 1115.97 | 1144.98 | 1.026 |  |
| 10.5m-1t/eager/range-topk/wall | 7.174 | 7.1735 | 1.000 |  |
| 10.5m-1t/streaming/range-topk/wall | 6.369 | 6.4765 | 1.017 |  |
| 10.5m-1t/eager/regex-iri/wall | 37.2385 | 36.6885 | 0.985 |  |
| 10.5m-1t/streaming/regex-iri/wall | 37.9635 | 38.0555 | 1.002 |  |
| 10.5m-1t/eager/star-join/wall | 36.09 | 35.0475 | 0.971 |  |
| 10.5m-1t/streaming/star-join/wall | 52.0745 | 52.9695 | 1.017 |  |
| 10.5m-1t/eager/two-hop-count/wall | 29.0045 | 28.959 | 0.998 |  |
| 10.5m-1t/streaming/two-hop-count/wall | 29.1335 | 29.0685 | 0.998 |  |
| 10.5m-12t/eager/class-predicate-list/wall | 11.2585 | 11.2745 | 1.001 |  |
| 10.5m-12t/streaming/class-predicate-list/wall | 11.7905 | 11.5755 | 0.982 |  |
| 10.5m-12t/eager/class-predicates/wall | 11.277 | 11.1855 | 0.992 |  |
| 10.5m-12t/streaming/class-predicates/wall | 11.9505 | 11.6275 | 0.973 |  |
| 10.5m-12t/streaming/contains/wall | 5.1565 | 5.0535 | 0.980 |  |
| 10.5m-12t/eager/distinct-join/wall | 44.566 | 43.6555 | 0.980 |  |
| 10.5m-12t/streaming/distinct-join/wall | 45.1225 | 44.433 | 0.985 |  |
| 10.5m-12t/eager/exists-join/wall | 9.947 | 9.789 | 0.984 |  |
| 10.5m-12t/streaming/exists-join/wall | 10.058 | 9.988 | 0.993 |  |
| 10.5m-12t/eager/export-500k/wall | 254.48 | 228.08 | 0.896 |  |
| 10.5m-12t/streaming/export-500k/wall | 294.608 | 292.974 | 0.994 |  |
| 10.5m-12t/eager/expr-agg-arg/wall | 29.543 | 29.171 | 0.987 |  |
| 10.5m-12t/streaming/expr-agg-arg/wall | 24.39 | 24.481 | 1.004 |  |
| 10.5m-12t/eager/expr-bind-group/wall | 5.5355 | 5.6395 | 1.019 |  |
| 10.5m-12t/eager/expr-order-key/wall | 12.1085 | 12.107 | 1.000 |  |
| 10.5m-12t/streaming/expr-order-key/wall | 11.8505 | 11.844 | 0.999 |  |
| 10.5m-12t/eager/group-avg/wall | 49.62 | 49.3805 | 0.995 |  |
| 10.5m-12t/streaming/group-avg/wall | 41.776 | 41.511 | 0.994 |  |
| 10.5m-12t/eager/knows-reach/wall | 142.282 | 144.315 | 1.014 |  |
| 10.5m-12t/streaming/knows-reach/wall | 140.788 | 141.679 | 1.006 |  |
| 10.5m-12t/eager/not-exists/wall | 10.6425 | 10.8415 | 1.019 |  |
| 10.5m-12t/streaming/not-exists/wall | 10.152 | 10.0015 | 0.985 |  |
| 10.5m-12t/eager/optional-chain/wall | 140.376 | 130.174 | 0.927 |  |
| 10.5m-12t/streaming/optional-chain/wall | 169.669 | 171.385 | 1.010 |  |
| 10.5m-12t/eager/optional-count/wall | 6.2225 | 6.247 | 1.004 |  |
| 10.5m-12t/streaming/optional-count/wall | 6.2455 | 6.236 | 0.998 |  |
| 10.5m-12t/eager/order-by-full/wall | 518.274 | 577.207 | 1.114 | slower |
| 10.5m-12t/streaming/order-by-full/wall | 557.496 | 668.779 | 1.200 | slower |
| 10.5m-12t/eager/regex-iri/wall | 6.743 | 6.7925 | 1.007 |  |
| 10.5m-12t/streaming/regex-iri/wall | 7.3095 | 7.3595 | 1.007 |  |
| 10.5m-12t/eager/star-join/wall | 33.5655 | 32.1245 | 0.957 |  |
| 10.5m-12t/streaming/star-join/wall | 43.4955 | 43.77 | 1.006 |  |
| 10.5m-12t/eager/two-hop-count/wall | 12.367 | 12.369 | 1.000 |  |
| 10.5m-12t/streaming/two-hop-count/wall | 12.451 | 12.401 | 0.996 |  |

96 metrics compared, 2 flagged, 0 failed checks, 0 missing checks, 0 changed fingerprints.
