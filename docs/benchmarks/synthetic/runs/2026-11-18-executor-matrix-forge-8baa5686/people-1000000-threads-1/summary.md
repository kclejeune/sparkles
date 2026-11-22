# Interleaved A/B on Namespace

- binary: /home/kclejeune/lanes/forge-backlog/bin/sparkles-8baa5686-matrix
- binary sha256: 20a381e40aa22acf2b099436054025829a753a69ca618156342d8af9b1f59d03
- binary commit: 8baa5686
- version: sparkles 0.1.0
- compilers: GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- build config: {"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1},"sparkles-owl":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{"CARGO_INCREMENTAL":"0"},"mbx_policy":"779a99e845a5ac3f1c4d64a3c95bcc74b33f47418e829bf008542ec2dbbcd151","profile":"release"}
- runtime env: clean
- scripts commit: unknown
- scripts dirty: 0
- host: forge
- started: 2026-11-18T09:27:30Z
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners on, execution eager,streaming
- A: /home/kclejeune/lanes/forge-backlog/bin/sparkles-8baa5686-matrix
- B: /home/kclejeune/lanes/forge-backlog/bin/sparkles-8baa5686-matrix
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
| class-predicate-list | 10.103 | 10.344 | 1.024 | +2.4% | 1.026 1.018 | 1.0% | 0.3% |
| class-predicates | 10.074 | 10.335 | 1.026 | +2.6% | 1.028 1.024 | 1.0% | 0.7% |
| contains | 24.649 | 24.693 | 1.002 | +0.2% | 1.000 1.019 | 0.3% | 2.5% |
| count-all | 0.006 | 0.008 | 1.215 | +21.5% | 1.131 1.269 | 11.6% | 15.3% |
| distinct-join | 39.158 | 38.214 | 0.976 | -2.4% | 0.981 0.975 | 0.4% | 1.7% |
| distinct-obj | 0.004 | 0.005 | 1.273 | +27.3% | 1.242 1.288 | 12.8% | 8.6% |
| employee-docs | 0.050 | 0.054 | 1.075 | +7.5% | 1.070 1.088 | 2.9% | 5.0% |
| exists-join | 8.601 | 8.629 | 1.003 | +0.3% | 1.005 1.008 | 1.5% | 1.1% |
| export-500k | 2.321 | 0.400 | 0.172 | -82.8% | 0.171 0.179 | 12.9% | 1.9% |
| expr-agg-arg | 28.530 | 23.127 | 0.811 | -18.9% | 0.807 0.815 | 3.5% | 1.0% |
| expr-bind-group | 5.765 | 3.295 | 0.572 | -42.8% | 0.579 0.570 | 2.4% | 0.7% |
| expr-order-key | 11.056 | 10.651 | 0.963 | -3.7% | 0.963 0.965 | 0.8% | 1.5% |
| group-avg | 45.206 | 37.228 | 0.824 | -17.6% | 0.823 0.823 | 1.6% | 0.7% |
| knows-reach | 140.011 | 137.307 | 0.981 | -1.9% | 0.981 0.982 | 1.1% | 1.1% |
| lang-filter | 18.041 | 18.074 | 1.002 | +0.2% | 1.005 0.999 | 0.9% | 1.2% |
| minus | 4.300 | 2.467 | 0.574 | -42.6% | 0.578 0.571 | 2.7% | 0.9% |
| not-exists | 9.477 | 8.737 | 0.922 | -7.8% | 0.923 0.944 | 0.9% | 4.0% |
| optional-chain | 26.970 | 25.312 | 0.939 | -6.1% | 0.949 0.939 | 2.9% | 2.9% |
| optional-count | 5.305 | 5.279 | 0.995 | -0.5% | 0.996 0.994 | 2.0% | 0.4% |
| order-by-full | 517.200 | 519.819 | 1.005 | +0.5% | 0.984 1.007 | 4.4% | 0.8% |
| path-plus | 0.009 | 0.009 | 1.058 | +5.8% | 0.998 1.081 | 14.8% | 9.7% |
| predicate-counts | 0.005 | 0.007 | 1.508 | +50.8% | 1.506 1.459 | 9.7% | 25.3% |
| predicate-subjects | 0.007 | 0.009 | 1.298 | +29.8% | 1.309 1.312 | 4.6% | 11.4% |
| range-topk | 5.172 | 5.092 | 0.985 | -1.5% | 0.988 0.983 | 0.4% | 0.8% |
| regex-iri | 38.164 | 38.157 | 1.000 | -0.0% | 0.998 1.001 | 0.4% | 1.2% |
| star-join | 14.148 | 14.843 | 1.049 | +4.9% | 1.052 1.045 | 1.8% | 1.0% |
| star-lookup | 0.058 | 0.058 | 1.010 | +1.0% | 1.025 0.997 | 7.0% | 4.5% |
| subquery-agg | 1.694 | 1.626 | 0.960 | -4.0% | 0.964 0.962 | 0.9% | 1.6% |
| two-hop-count | 27.585 | 27.568 | 0.999 | -0.1% | 0.996 1.004 | 0.7% | 0.6% |
| types-grouped | 0.008 | 0.013 | 1.544 | +54.4% | 1.595 1.479 | 9.1% | 9.0% |
| values-star | 0.016 | 0.020 | 1.222 | +22.2% | 1.236 1.197 | 5.6% | 4.3% |
| **geometric mean** | | | 0.950 | -5.0% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 11.451 | 11.693 | 1.021 | +2.1% | 1.030 1.011 | 2.3% | 1.0% |
| class-predicates | 11.452 | 11.701 | 1.022 | +2.2% | 1.023 1.021 | 2.4% | 1.4% |
| contains | 25.886 | 25.951 | 1.002 | +0.2% | 1.001 1.013 | 0.2% | 2.4% |
| count-all | 1.898 | 1.970 | 1.038 | +3.8% | 1.037 1.031 | 18.1% | 24.2% |
| distinct-join | 44.424 | 45.942 | 1.034 | +3.4% | 1.036 1.036 | 0.4% | 2.2% |
| distinct-obj | 1.274 | 1.972 | 1.548 | +54.8% | 1.600 1.020 | 59.0% | 0.1% |
| employee-docs | 1.981 | 1.984 | 1.002 | +0.2% | 1.002 0.980 | 2.9% | 0.1% |
| exists-join | 9.983 | 10.030 | 1.005 | +0.5% | 1.004 1.011 | 1.4% | 1.0% |
| export-500k | 239.001 | 356.016 | 1.490 | +49.0% | 1.490 1.489 | 0.4% | 0.4% |
| expr-agg-arg | 29.938 | 24.521 | 0.819 | -18.1% | 0.821 0.822 | 3.4% | 1.0% |
| expr-bind-group | 6.979 | 4.577 | 0.656 | -34.4% | 0.671 0.652 | 4.2% | 4.2% |
| expr-order-key | 13.050 | 12.209 | 0.936 | -6.4% | 0.919 0.956 | 0.9% | 7.4% |
| group-avg | 46.736 | 38.805 | 0.830 | -17.0% | 0.828 0.829 | 1.9% | 0.7% |
| knows-reach | 141.146 | 138.632 | 0.982 | -1.8% | 0.981 0.983 | 1.1% | 1.1% |
| lang-filter | 19.155 | 19.264 | 1.006 | +0.6% | 1.009 1.000 | 0.9% | 1.6% |
| minus | 5.825 | 3.726 | 0.640 | -36.0% | 0.666 0.630 | 6.7% | 5.8% |
| not-exists | 10.759 | 9.969 | 0.927 | -7.3% | 0.925 0.946 | 1.5% | 9.3% |
| optional-chain | 137.047 | 220.019 | 1.605 | +60.5% | 1.610 1.606 | 0.4% | 0.5% |
| optional-count | 6.651 | 6.566 | 0.987 | -1.3% | 0.993 0.980 | 4.9% | 3.8% |
| order-by-full | 858.015 | 1061.575 | 1.237 | +23.7% | 1.224 1.237 | 2.7% | 0.7% |
| path-plus | 1.328 | 1.974 | 1.486 | +48.6% | 1.498 1.210 | 50.8% | 4.5% |
| predicate-counts | 1.333 | 1.982 | 1.487 | +48.7% | 1.458 1.540 | 24.8% | 0.2% |
| predicate-subjects | 1.970 | 1.980 | 1.005 | +0.5% | 1.199 1.005 | 48.8% | 1.6% |
| range-topk | 6.501 | 6.470 | 0.995 | -0.5% | 1.002 0.979 | 8.4% | 3.7% |
| regex-iri | 39.166 | 39.280 | 1.003 | +0.3% | 1.004 1.001 | 1.4% | 1.8% |
| star-join | 36.763 | 52.107 | 1.417 | +41.7% | 1.408 1.438 | 2.4% | 0.1% |
| star-lookup | 1.986 | 1.989 | 1.001 | +0.1% | 1.001 1.003 | 0.3% | 0.1% |
| subquery-agg | 2.902 | 2.905 | 1.001 | +0.1% | 0.997 1.005 | 2.8% | 0.5% |
| two-hop-count | 28.929 | 28.966 | 1.001 | +0.1% | 1.000 1.002 | 0.7% | 0.5% |
| types-grouped | 1.972 | 1.974 | 1.001 | +0.1% | 1.019 1.001 | 24.8% | 0.1% |
| values-star | 1.981 | 1.983 | 1.001 | +0.1% | 0.990 1.003 | 2.9% | 0.8% |
| **geometric mean** | | | 1.045 | +4.5% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
