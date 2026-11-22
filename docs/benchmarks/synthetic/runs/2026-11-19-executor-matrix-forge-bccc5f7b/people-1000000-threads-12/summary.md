# Interleaved A/B on Namespace

- binary: /home/kclejeune/lanes/forge-backlog/bin/sparkles-bccc5f7b-matrix
- binary sha256: 27cf6f507e4ad5ad12f65605fe4f6c4961d95c43dcf4affb7aa4ca90d5703cd6
- binary commit: bccc5f7b
- version: sparkles 0.1.0
- compilers: GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- build config: {"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1},"sparkles-owl":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{"CARGO_INCREMENTAL":"0"},"mbx_policy":"779a99e845a5ac3f1c4d64a3c95bcc74b33f47418e829bf008542ec2dbbcd151","profile":"release"}
- runtime env: clean
- scripts commit: unknown
- scripts dirty: 0
- host: forge
- started: 2026-11-19T00:08:03Z
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=12, server CPU 0-11, client CPU 12, spinners off, execution eager,streaming
- A: /home/kclejeune/lanes/forge-backlog/bin/sparkles-bccc5f7b-matrix
- B: /home/kclejeune/lanes/forge-backlog/bin/sparkles-bccc5f7b-matrix
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
| class-predicate-list | 10.433 | 10.690 | 1.025 | +2.5% | 1.047 0.999 | 6.5% | 2.5% |
| class-predicates | 10.329 | 10.678 | 1.034 | +3.4% | 1.052 1.011 | 2.0% | 2.4% |
| contains | 4.291 | 4.263 | 0.994 | -0.6% | 1.000 0.989 | 2.5% | 1.4% |
| count-all | 0.009 | 0.011 | 1.284 | +28.4% | 0.883 1.592 | 13.4% | 263.0% |
| distinct-join | 38.891 | 38.095 | 0.980 | -2.0% | 0.982 0.978 | 4.6% | 1.8% |
| distinct-obj | 0.007 | 0.009 | 1.292 | +29.2% | 1.512 1.194 | 36.8% | 39.9% |
| employee-docs | 0.069 | 0.079 | 1.155 | +15.5% | 1.242 1.105 | 5.9% | 8.9% |
| exists-join | 9.085 | 8.767 | 0.965 | -3.5% | 0.973 0.958 | 0.9% | 2.7% |
| export-500k | 2.306 | 0.383 | 0.166 | -83.4% | 0.119 0.166 | 103.7% | 1.2% |
| expr-agg-arg | 23.987 | 23.346 | 0.973 | -2.7% | 0.966 0.983 | 3.0% | 1.3% |
| expr-bind-group | 3.684 | 3.379 | 0.917 | -8.3% | 0.922 0.911 | 4.4% | 3.1% |
| expr-order-key | 10.079 | 11.275 | 1.119 | +11.9% | 1.119 1.115 | 1.7% | 0.8% |
| group-avg | 40.394 | 39.081 | 0.967 | -3.3% | 0.969 0.960 | 2.5% | 1.9% |
| knows-reach | 141.859 | 140.383 | 0.990 | -1.0% | 0.988 0.992 | 3.0% | 1.2% |
| lang-filter | 3.047 | 3.059 | 1.004 | +0.4% | 1.009 1.004 | 1.8% | 4.3% |
| minus | 3.907 | 2.622 | 0.671 | -32.9% | 0.655 0.671 | 7.7% | 3.3% |
| not-exists | 9.183 | 9.144 | 0.996 | -0.4% | 1.010 0.967 | 3.9% | 5.9% |
| optional-chain | 27.535 | 27.628 | 1.003 | +0.3% | 1.014 1.021 | 5.0% | 29.2% |
| optional-count | 4.646 | 5.521 | 1.188 | +18.8% | 1.173 1.210 | 1.5% | 3.4% |
| order-by-full | 146.426 | 166.012 | 1.134 | +13.4% | 1.363 1.013 | 21.8% | 43.4% |
| path-plus | 0.016 | 0.015 | 0.892 | -10.8% | 0.874 0.895 | 21.7% | 60.9% |
| predicate-counts | 0.008 | 0.011 | 1.396 | +39.6% | 1.820 1.358 | 9.5% | 68.2% |
| predicate-subjects | 0.011 | 0.015 | 1.412 | +41.2% | 1.949 1.249 | 34.3% | 52.1% |
| range-topk | 2.093 | 2.277 | 1.088 | +8.8% | 1.088 1.092 | 3.1% | 37.2% |
| regex-iri | 6.319 | 6.330 | 1.002 | +0.2% | 0.999 1.005 | 1.8% | 0.9% |
| star-join | 13.246 | 13.719 | 1.036 | +3.6% | 1.031 1.028 | 8.7% | 2.5% |
| star-lookup | 0.072 | 0.081 | 1.123 | +12.3% | 1.152 1.081 | 7.0% | 23.6% |
| subquery-agg | 0.783 | 0.800 | 1.021 | +2.1% | 1.004 1.007 | 6.3% | 8.1% |
| two-hop-count | 11.382 | 11.363 | 0.998 | -0.2% | 0.997 1.000 | 2.4% | 2.5% |
| types-grouped | 0.013 | 0.020 | 1.610 | +61.0% | 1.648 1.603 | 6.2% | 16.6% |
| values-star | 0.021 | 0.028 | 1.326 | +32.6% | 1.547 1.284 | 1.9% | 23.6% |
| **geometric mean** | | | 1.009 | +0.9% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 11.535 | 11.824 | 1.025 | +2.5% | 1.040 1.003 | 4.8% | 1.4% |
| class-predicates | 11.462 | 11.782 | 1.028 | +2.8% | 1.037 1.011 | 2.0% | 1.3% |
| contains | 5.089 | 5.217 | 1.025 | +2.5% | 1.037 1.020 | 3.5% | 2.6% |
| count-all | 0.506 | 0.764 | 1.512 | +51.2% | 1.325 1.551 | 15.1% | 63.2% |
| distinct-join | 44.292 | 43.956 | 0.992 | -0.8% | 0.994 0.992 | 4.6% | 2.1% |
| distinct-obj | 0.520 | 0.780 | 1.498 | +49.8% | 1.603 1.485 | 25.2% | 11.6% |
| employee-docs | 0.719 | 0.937 | 1.303 | +30.3% | 1.335 1.299 | 3.4% | 11.0% |
| exists-join | 10.096 | 9.909 | 0.982 | -1.8% | 0.989 0.980 | 1.8% | 2.9% |
| export-500k | 228.272 | 289.663 | 1.269 | +26.9% | 1.277 1.264 | 4.3% | 1.7% |
| expr-agg-arg | 25.147 | 24.552 | 0.976 | -2.4% | 0.973 0.984 | 2.7% | 1.6% |
| expr-bind-group | 4.495 | 4.390 | 0.977 | -2.3% | 0.988 0.975 | 4.6% | 4.5% |
| expr-order-key | 11.870 | 12.401 | 1.045 | +4.5% | 1.046 1.040 | 2.3% | 2.0% |
| group-avg | 41.803 | 40.483 | 0.968 | -3.2% | 0.968 0.958 | 3.0% | 1.9% |
| knows-reach | 143.335 | 141.608 | 0.988 | -1.2% | 0.987 0.992 | 2.9% | 1.2% |
| lang-filter | 3.846 | 4.014 | 1.043 | +4.3% | 1.040 1.045 | 2.9% | 2.7% |
| minus | 4.739 | 3.614 | 0.762 | -23.8% | 0.736 0.777 | 4.8% | 5.9% |
| not-exists | 10.130 | 10.162 | 1.003 | +0.3% | 1.026 0.994 | 4.1% | 5.8% |
| optional-chain | 133.348 | 173.888 | 1.304 | +30.4% | 1.287 1.309 | 5.5% | 6.7% |
| optional-count | 5.508 | 6.565 | 1.192 | +19.2% | 1.182 1.199 | 2.7% | 2.1% |
| order-by-full | 469.392 | 566.968 | 1.208 | +20.8% | 1.261 1.178 | 4.7% | 10.4% |
| path-plus | 0.510 | 0.757 | 1.484 | +48.4% | 1.556 1.430 | 21.4% | 34.1% |
| predicate-counts | 0.524 | 0.853 | 1.627 | +62.7% | 1.610 1.640 | 13.3% | 16.4% |
| predicate-subjects | 0.482 | 0.754 | 1.564 | +56.4% | 1.772 1.498 | 21.7% | 12.4% |
| range-topk | 2.998 | 3.367 | 1.123 | +12.3% | 1.128 1.125 | 3.0% | 23.0% |
| regex-iri | 7.135 | 7.341 | 1.029 | +2.9% | 1.031 1.022 | 0.7% | 2.8% |
| star-join | 33.808 | 43.397 | 1.284 | +28.4% | 1.281 1.295 | 3.7% | 0.2% |
| star-lookup | 0.810 | 1.036 | 1.278 | +27.8% | 1.319 1.230 | 6.1% | 32.0% |
| subquery-agg | 1.525 | 1.714 | 1.124 | +12.4% | 1.161 1.103 | 6.0% | 11.3% |
| two-hop-count | 12.570 | 12.538 | 0.997 | -0.3% | 0.999 0.999 | 2.4% | 1.5% |
| types-grouped | 0.594 | 0.838 | 1.411 | +41.1% | 1.520 1.406 | 7.8% | 34.3% |
| values-star | 0.685 | 0.863 | 1.260 | +26.0% | 1.365 1.210 | 3.7% | 21.3% |
| **geometric mean** | | | 1.152 | +15.2% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
