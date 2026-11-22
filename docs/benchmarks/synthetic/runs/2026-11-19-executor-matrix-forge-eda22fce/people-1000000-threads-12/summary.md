# Interleaved A/B on Namespace

- binary: /home/kclejeune/lanes/forge-backlog/bin/sparkles-eda22fce-matrix
- binary sha256: 915ba62e2bade5551f25d5b0c7acca90aa8578e21f180dfb7ccdbf6290fc023c
- binary commit: eda22fce
- version: sparkles 0.1.0
- compilers: GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- build config: {"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1},"sparkles-owl":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{"CARGO_INCREMENTAL":"0"},"mbx_policy":"779a99e845a5ac3f1c4d64a3c95bcc74b33f47418e829bf008542ec2dbbcd151","profile":"release"}
- runtime env: clean
- scripts commit: unknown
- scripts dirty: 0
- host: forge
- started: 2026-11-19T02:31:44Z
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=12, server CPU 0-11, client CPU 12, spinners off, execution eager,streaming
- A: /home/kclejeune/lanes/forge-backlog/bin/sparkles-eda22fce-matrix
- B: /home/kclejeune/lanes/forge-backlog/bin/sparkles-eda22fce-matrix
- data sha256: 10daf6486c20f8dd4579d819ea73bb5e7836e054cd6d2d1b73f99f35233635f8
- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

A and B returned the same sorted TSV answers to all 31 queries.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 10.187 | 10.747 | 1.055 | +5.5% | 1.040 1.046 | 5.4% | 3.1% |
| class-predicates | 10.153 | 10.694 | 1.053 | +5.3% | 1.057 1.054 | 0.9% | 3.0% |
| contains | 4.273 | 4.251 | 0.995 | -0.5% | 0.991 0.992 | 2.3% | 2.5% |
| count-all | 0.009 | 0.012 | 1.322 | +32.2% | 1.246 1.665 | 9.4% | 29.0% |
| distinct-join | 39.425 | 39.107 | 0.992 | -0.8% | 0.990 0.992 | 1.7% | 1.1% |
| distinct-obj | 0.006 | 0.009 | 1.493 | +49.3% | 1.539 1.477 | 6.3% | 14.3% |
| employee-docs | 0.070 | 0.078 | 1.109 | +10.9% | 1.068 1.142 | 5.1% | 21.2% |
| exists-join | 8.941 | 8.953 | 1.001 | +0.1% | 0.981 1.005 | 4.8% | 3.4% |
| export-500k | 2.331 | 0.386 | 0.165 | -83.5% | 0.176 0.160 | 11.9% | 2.6% |
| expr-agg-arg | 27.528 | 23.439 | 0.851 | -14.9% | 0.851 0.852 | 1.5% | 1.2% |
| expr-bind-group | 4.857 | 3.451 | 0.711 | -28.9% | 0.700 0.718 | 4.7% | 8.2% |
| expr-order-key | 10.167 | 11.042 | 1.086 | +8.6% | 1.080 1.103 | 1.8% | 1.5% |
| group-avg | 45.628 | 37.877 | 0.830 | -17.0% | 0.827 0.833 | 2.2% | 0.8% |
| knows-reach | 141.198 | 139.670 | 0.989 | -1.1% | 0.980 0.994 | 1.0% | 1.9% |
| lang-filter | 3.125 | 3.109 | 0.995 | -0.5% | 1.009 0.965 | 3.0% | 5.4% |
| minus | 4.334 | 2.581 | 0.596 | -40.4% | 0.592 0.609 | 4.1% | 4.2% |
| not-exists | 9.653 | 8.951 | 0.927 | -7.3% | 0.904 0.940 | 7.3% | 3.4% |
| optional-chain | 26.849 | 26.847 | 1.000 | -0.0% | 0.988 1.046 | 11.1% | 3.2% |
| optional-count | 5.403 | 5.484 | 1.015 | +1.5% | 1.019 1.015 | 4.9% | 2.1% |
| order-by-full | 121.730 | 125.833 | 1.034 | +3.4% | 1.149 1.029 | 8.5% | 45.7% |
| path-plus | 0.014 | 0.014 | 1.032 | +3.2% | 1.032 1.004 | 10.2% | 7.1% |
| predicate-counts | 0.007 | 0.011 | 1.603 | +60.3% | 1.667 1.481 | 3.4% | 32.5% |
| predicate-subjects | 0.009 | 0.016 | 1.746 | +74.6% | 2.021 1.696 | 34.5% | 25.3% |
| range-topk | 2.068 | 2.010 | 0.972 | -2.8% | 0.966 0.971 | 16.4% | 18.2% |
| regex-iri | 6.316 | 6.329 | 1.002 | +0.2% | 1.003 1.002 | 2.4% | 1.2% |
| star-join | 13.274 | 13.704 | 1.032 | +3.2% | 1.049 1.021 | 2.6% | 3.3% |
| star-lookup | 0.073 | 0.084 | 1.156 | +15.6% | 1.164 1.049 | 3.7% | 15.2% |
| subquery-agg | 0.807 | 0.846 | 1.048 | +4.8% | 1.144 1.000 | 11.8% | 9.2% |
| two-hop-count | 11.370 | 11.167 | 0.982 | -1.8% | 0.982 0.981 | 6.6% | 3.3% |
| types-grouped | 0.012 | 0.019 | 1.631 | +63.1% | 1.679 1.548 | 31.5% | 66.6% |
| values-star | 0.022 | 0.029 | 1.321 | +32.1% | 1.223 1.634 | 5.1% | 53.5% |
| **geometric mean** | | | 0.997 | -0.3% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 11.315 | 11.828 | 1.045 | +4.5% | 1.038 1.043 | 3.6% | 3.0% |
| class-predicates | 11.319 | 11.831 | 1.045 | +4.5% | 1.048 1.042 | 1.0% | 0.9% |
| contains | 5.089 | 5.250 | 1.032 | +3.2% | 1.033 1.031 | 0.7% | 3.3% |
| count-all | 0.514 | 0.782 | 1.520 | +52.0% | 1.389 1.733 | 9.2% | 33.6% |
| distinct-join | 44.830 | 45.326 | 1.011 | +1.1% | 1.027 1.003 | 1.6% | 3.1% |
| distinct-obj | 0.497 | 0.835 | 1.678 | +67.8% | 1.820 1.596 | 12.2% | 8.9% |
| employee-docs | 0.714 | 1.033 | 1.447 | +44.7% | 1.423 1.445 | 6.3% | 13.3% |
| exists-join | 10.024 | 10.104 | 1.008 | +0.8% | 0.998 1.027 | 4.9% | 2.5% |
| export-500k | 223.231 | 267.032 | 1.196 | +19.6% | 1.199 1.191 | 1.3% | 0.6% |
| expr-agg-arg | 28.645 | 24.643 | 0.860 | -14.0% | 0.861 0.858 | 1.4% | 1.6% |
| expr-bind-group | 5.646 | 4.522 | 0.801 | -19.9% | 0.789 0.806 | 5.0% | 4.5% |
| expr-order-key | 11.888 | 12.237 | 1.029 | +2.9% | 1.025 1.041 | 2.5% | 1.1% |
| group-avg | 47.074 | 39.129 | 0.831 | -16.9% | 0.828 0.834 | 2.3% | 0.7% |
| knows-reach | 142.483 | 140.913 | 0.989 | -1.1% | 0.979 0.993 | 1.0% | 1.9% |
| lang-filter | 3.914 | 4.048 | 1.034 | +3.4% | 1.051 1.004 | 2.2% | 5.3% |
| minus | 5.180 | 3.545 | 0.684 | -31.6% | 0.675 0.692 | 2.7% | 7.6% |
| not-exists | 10.742 | 9.971 | 0.928 | -7.2% | 0.898 0.962 | 7.8% | 4.9% |
| optional-chain | 133.476 | 162.232 | 1.215 | +21.5% | 1.215 1.215 | 1.3% | 2.0% |
| optional-count | 6.271 | 6.456 | 1.030 | +3.0% | 1.025 1.024 | 4.2% | 1.6% |
| order-by-full | 441.635 | 492.708 | 1.116 | +11.6% | 1.150 1.103 | 2.3% | 12.9% |
| path-plus | 0.516 | 0.754 | 1.460 | +46.0% | 1.499 1.435 | 17.1% | 17.4% |
| predicate-counts | 0.524 | 0.827 | 1.576 | +57.6% | 1.751 1.517 | 20.2% | 34.0% |
| predicate-subjects | 0.460 | 0.802 | 1.745 | +74.5% | 1.886 1.647 | 22.8% | 7.7% |
| range-topk | 2.962 | 3.099 | 1.046 | +4.6% | 1.054 1.027 | 13.1% | 11.2% |
| regex-iri | 7.133 | 7.343 | 1.030 | +3.0% | 1.031 1.026 | 1.7% | 1.5% |
| star-join | 33.495 | 42.390 | 1.266 | +26.6% | 1.276 1.247 | 1.5% | 2.8% |
| star-lookup | 0.799 | 1.049 | 1.313 | +31.3% | 1.393 1.192 | 10.2% | 31.8% |
| subquery-agg | 1.548 | 1.863 | 1.203 | +20.3% | 1.282 1.165 | 6.8% | 19.5% |
| two-hop-count | 12.529 | 12.311 | 0.983 | -1.7% | 0.986 0.983 | 6.3% | 2.8% |
| types-grouped | 0.552 | 0.853 | 1.545 | +54.5% | 1.679 1.313 | 20.8% | 27.2% |
| values-star | 0.687 | 0.914 | 1.330 | +33.0% | 1.311 1.365 | 1.2% | 13.5% |
| **geometric mean** | | | 1.133 | +13.3% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
