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
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=12, server CPU 0-11, client CPU 12, spinners off, execution eager,streaming
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
| class-predicate-list | 10.484 | 10.600 | 1.011 | +1.1% | 1.001 1.031 | 2.7% | 2.5% |
| class-predicates | 10.284 | 10.722 | 1.043 | +4.3% | 1.033 1.050 | 3.5% | 1.1% |
| contains | 4.256 | 4.275 | 1.004 | +0.4% | 1.014 1.001 | 1.1% | 3.1% |
| count-all | 0.009 | 0.011 | 1.324 | +32.4% | 1.177 1.473 | 37.8% | 61.7% |
| distinct-join | 39.279 | 39.092 | 0.995 | -0.5% | 1.000 0.986 | 0.5% | 1.7% |
| distinct-obj | 0.007 | 0.009 | 1.408 | +40.8% | 1.419 1.419 | 17.0% | 11.4% |
| employee-docs | 0.069 | 0.077 | 1.116 | +11.6% | 1.114 1.130 | 5.3% | 11.1% |
| exists-join | 8.950 | 8.915 | 0.996 | -0.4% | 0.993 0.997 | 1.7% | 4.8% |
| export-500k | 2.198 | 0.383 | 0.174 | -82.6% | 0.169 0.173 | 11.1% | 4.0% |
| expr-agg-arg | 27.674 | 23.414 | 0.846 | -15.4% | 0.843 0.848 | 2.2% | 1.5% |
| expr-bind-group | 4.751 | 3.413 | 0.718 | -28.2% | 0.726 0.706 | 2.1% | 1.6% |
| expr-order-key | 10.103 | 10.959 | 1.085 | +8.5% | 1.083 1.088 | 1.2% | 2.5% |
| group-avg | 45.441 | 37.719 | 0.830 | -17.0% | 0.833 0.829 | 1.1% | 2.0% |
| knows-reach | 141.111 | 139.260 | 0.987 | -1.3% | 0.989 0.982 | 0.7% | 0.7% |
| lang-filter | 3.124 | 3.056 | 0.978 | -2.2% | 0.992 0.969 | 1.7% | 3.7% |
| minus | 4.303 | 2.586 | 0.601 | -39.9% | 0.598 0.610 | 4.7% | 5.6% |
| not-exists | 9.755 | 8.949 | 0.917 | -8.3% | 0.938 0.910 | 3.5% | 2.4% |
| optional-chain | 25.146 | 26.508 | 1.054 | +5.4% | 1.050 1.054 | 2.7% | 2.3% |
| optional-count | 5.402 | 5.452 | 1.009 | +0.9% | 1.010 1.009 | 7.7% | 0.8% |
| order-by-full | 108.713 | 107.363 | 0.988 | -1.2% | 1.046 0.975 | 14.0% | 21.2% |
| path-plus | 0.012 | 0.015 | 1.229 | +22.9% | 1.260 1.216 | 30.3% | 50.6% |
| predicate-counts | 0.007 | 0.011 | 1.621 | +62.1% | 1.821 1.551 | 40.9% | 23.0% |
| predicate-subjects | 0.009 | 0.017 | 1.845 | +84.5% | 1.961 1.807 | 20.4% | 23.6% |
| range-topk | 2.111 | 2.071 | 0.981 | -1.9% | 0.957 0.986 | 16.3% | 8.7% |
| regex-iri | 6.499 | 6.503 | 1.001 | +0.1% | 0.994 1.008 | 1.2% | 1.1% |
| star-join | 13.309 | 13.919 | 1.046 | +4.6% | 1.036 1.049 | 6.9% | 4.3% |
| star-lookup | 0.073 | 0.084 | 1.148 | +14.8% | 1.141 1.161 | 10.2% | 9.8% |
| subquery-agg | 0.822 | 0.773 | 0.940 | -6.0% | 0.969 0.930 | 10.0% | 7.6% |
| two-hop-count | 11.313 | 11.176 | 0.988 | -1.2% | 0.973 0.996 | 2.3% | 1.5% |
| types-grouped | 0.012 | 0.020 | 1.649 | +64.9% | 1.685 1.580 | 1.8% | 23.6% |
| values-star | 0.021 | 0.027 | 1.329 | +32.9% | 1.471 1.237 | 6.5% | 32.1% |
| **geometric mean** | | | 1.001 | +0.1% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 11.551 | 11.724 | 1.015 | +1.5% | 0.998 1.034 | 2.9% | 2.8% |
| class-predicates | 11.402 | 11.899 | 1.044 | +4.4% | 1.030 1.047 | 3.7% | 2.1% |
| contains | 5.069 | 5.305 | 1.047 | +4.7% | 1.047 1.046 | 0.6% | 0.7% |
| count-all | 0.461 | 0.745 | 1.616 | +61.6% | 1.472 1.694 | 25.5% | 13.8% |
| distinct-join | 44.788 | 45.186 | 1.009 | +0.9% | 1.010 1.007 | 0.7% | 1.9% |
| distinct-obj | 0.512 | 0.764 | 1.491 | +49.1% | 1.412 1.562 | 6.3% | 15.2% |
| employee-docs | 0.726 | 0.960 | 1.324 | +32.4% | 1.246 1.339 | 3.0% | 12.5% |
| exists-join | 9.917 | 10.069 | 1.015 | +1.5% | 1.010 1.026 | 1.0% | 2.7% |
| export-500k | 225.286 | 283.745 | 1.259 | +25.9% | 1.269 1.243 | 1.2% | 3.1% |
| expr-agg-arg | 28.817 | 24.544 | 0.852 | -14.8% | 0.853 0.852 | 2.0% | 1.9% |
| expr-bind-group | 5.585 | 4.441 | 0.795 | -20.5% | 0.814 0.773 | 1.6% | 4.6% |
| expr-order-key | 11.866 | 12.134 | 1.023 | +2.3% | 1.015 1.028 | 2.6% | 2.4% |
| group-avg | 46.862 | 39.052 | 0.833 | -16.7% | 0.836 0.834 | 1.3% | 1.7% |
| knows-reach | 142.429 | 140.560 | 0.987 | -1.3% | 0.989 0.983 | 0.6% | 0.8% |
| lang-filter | 3.930 | 3.960 | 1.008 | +0.8% | 1.018 0.996 | 1.8% | 4.6% |
| minus | 5.137 | 3.564 | 0.694 | -30.6% | 0.697 0.690 | 3.5% | 1.4% |
| not-exists | 10.643 | 9.965 | 0.936 | -6.4% | 0.961 0.906 | 5.4% | 3.2% |
| optional-chain | 131.810 | 169.871 | 1.289 | +28.9% | 1.292 1.283 | 2.5% | 1.0% |
| optional-count | 6.286 | 6.481 | 1.031 | +3.1% | 1.043 1.026 | 6.2% | 3.0% |
| order-by-full | 434.529 | 501.717 | 1.155 | +15.5% | 1.188 1.141 | 4.6% | 5.0% |
| path-plus | 0.456 | 0.760 | 1.667 | +66.7% | 1.483 1.861 | 10.2% | 17.9% |
| predicate-counts | 0.479 | 0.807 | 1.683 | +68.3% | 1.805 1.498 | 24.1% | 28.0% |
| predicate-subjects | 0.448 | 0.797 | 1.779 | +77.9% | 1.919 1.614 | 21.9% | 32.9% |
| range-topk | 2.962 | 3.208 | 1.083 | +8.3% | 1.039 1.090 | 9.9% | 4.2% |
| regex-iri | 7.296 | 7.533 | 1.032 | +3.2% | 1.032 1.035 | 0.6% | 0.9% |
| star-join | 33.717 | 43.764 | 1.298 | +29.8% | 1.300 1.292 | 1.8% | 2.1% |
| star-lookup | 0.826 | 1.074 | 1.300 | +30.0% | 1.358 1.286 | 7.2% | 11.8% |
| subquery-agg | 1.606 | 1.677 | 1.045 | +4.5% | 1.093 1.016 | 8.1% | 9.7% |
| two-hop-count | 12.532 | 12.346 | 0.985 | -1.5% | 0.981 0.995 | 1.7% | 2.1% |
| types-grouped | 0.573 | 0.835 | 1.458 | +45.8% | 1.369 1.513 | 4.5% | 14.3% |
| values-star | 0.688 | 0.901 | 1.311 | +31.1% | 1.233 1.328 | 2.3% | 23.4% |
| **geometric mean** | | | 1.133 | +13.3% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
