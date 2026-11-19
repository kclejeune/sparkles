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
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners on, execution eager,streaming
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
| class-predicate-list | 10.034 | 10.436 | 1.040 | +4.0% | 1.054 1.048 | 1.0% | 2.1% |
| class-predicates | 10.132 | 10.401 | 1.027 | +2.7% | 1.033 1.025 | 1.2% | 1.7% |
| contains | 24.574 | 24.593 | 1.001 | +0.1% | 1.000 1.002 | 0.2% | 0.2% |
| count-all | 0.006 | 0.007 | 1.254 | +25.4% | 1.226 1.282 | 3.7% | 8.3% |
| distinct-join | 38.455 | 37.551 | 0.976 | -2.4% | 0.978 0.975 | 0.3% | 0.6% |
| distinct-obj | 0.004 | 0.006 | 1.323 | +32.3% | 1.266 1.513 | 15.1% | 10.0% |
| employee-docs | 0.049 | 0.054 | 1.090 | +9.0% | 1.101 1.079 | 5.7% | 10.4% |
| exists-join | 8.560 | 8.576 | 1.002 | +0.2% | 0.997 1.000 | 2.5% | 1.1% |
| export-500k | 2.389 | 0.418 | 0.175 | -82.5% | 0.174 0.174 | 8.5% | 3.0% |
| expr-agg-arg | 25.271 | 22.701 | 0.898 | -10.2% | 0.897 0.902 | 0.4% | 0.9% |
| expr-bind-group | 4.799 | 3.209 | 0.669 | -33.1% | 0.675 0.668 | 2.4% | 0.3% |
| expr-order-key | 11.028 | 10.918 | 0.990 | -1.0% | 0.992 0.989 | 0.2% | 1.1% |
| group-avg | 39.472 | 37.981 | 0.962 | -3.8% | 0.965 0.958 | 1.0% | 0.4% |
| knows-reach | 136.810 | 137.127 | 1.002 | +0.2% | 1.005 1.003 | 0.8% | 0.7% |
| lang-filter | 17.574 | 17.631 | 1.003 | +0.3% | 1.004 1.001 | 0.5% | 0.7% |
| minus | 3.774 | 2.463 | 0.653 | -34.7% | 0.654 0.644 | 2.3% | 0.3% |
| not-exists | 8.985 | 8.624 | 0.960 | -4.0% | 0.963 0.956 | 1.8% | 1.0% |
| optional-chain | 26.270 | 25.122 | 0.956 | -4.4% | 0.959 0.954 | 1.5% | 1.8% |
| optional-count | 4.510 | 5.303 | 1.176 | +17.6% | 1.177 1.153 | 3.8% | 0.3% |
| order-by-full | 518.709 | 520.744 | 1.004 | +0.4% | 1.007 1.002 | 0.5% | 0.3% |
| path-plus | 0.010 | 0.010 | 0.963 | -3.7% | 0.916 0.964 | 13.8% | 11.6% |
| predicate-counts | 0.005 | 0.007 | 1.502 | +50.2% | 1.511 1.509 | 9.6% | 3.9% |
| predicate-subjects | 0.007 | 0.009 | 1.337 | +33.7% | 1.304 1.400 | 12.5% | 2.4% |
| range-topk | 5.124 | 5.059 | 0.987 | -1.3% | 0.988 0.987 | 0.5% | 0.4% |
| regex-iri | 37.743 | 37.783 | 1.001 | +0.1% | 0.996 1.002 | 1.8% | 0.3% |
| star-join | 13.938 | 14.640 | 1.050 | +5.0% | 1.048 1.053 | 0.8% | 0.7% |
| star-lookup | 0.058 | 0.061 | 1.047 | +4.7% | 1.011 1.088 | 10.4% | 0.8% |
| subquery-agg | 1.699 | 1.644 | 0.967 | -3.3% | 0.964 0.969 | 0.5% | 1.6% |
| two-hop-count | 27.818 | 27.870 | 1.002 | +0.2% | 1.001 1.002 | 0.4% | 0.1% |
| types-grouped | 0.008 | 0.013 | 1.605 | +60.5% | 1.604 1.609 | 10.2% | 4.2% |
| values-star | 0.017 | 0.021 | 1.267 | +26.7% | 1.287 1.242 | 7.6% | 9.9% |
| **geometric mean** | | | 0.981 | -1.9% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 11.379 | 11.770 | 1.034 | +3.4% | 1.039 1.030 | 2.4% | 2.5% |
| class-predicates | 11.450 | 11.816 | 1.032 | +3.2% | 1.042 1.027 | 1.4% | 1.4% |
| contains | 25.801 | 25.846 | 1.002 | +0.2% | 1.001 1.002 | 0.3% | 0.2% |
| count-all | 1.352 | 1.975 | 1.461 | +46.1% | 1.852 1.017 | 98.4% | 0.2% |
| distinct-join | 43.587 | 45.750 | 1.050 | +5.0% | 1.050 1.053 | 1.0% | 0.4% |
| distinct-obj | 1.931 | 1.975 | 1.023 | +2.3% | 1.027 1.021 | 22.7% | 0.1% |
| employee-docs | 1.979 | 1.983 | 1.002 | +0.2% | 1.001 1.002 | 0.9% | 0.2% |
| exists-join | 9.989 | 9.963 | 0.997 | -0.3% | 0.996 0.995 | 3.0% | 1.0% |
| export-500k | 234.960 | 362.972 | 1.545 | +54.5% | 1.543 1.548 | 0.4% | 0.3% |
| expr-agg-arg | 26.712 | 23.871 | 0.894 | -10.6% | 0.887 0.898 | 0.5% | 2.1% |
| expr-bind-group | 6.034 | 4.494 | 0.745 | -25.5% | 0.755 0.738 | 1.6% | 10.3% |
| expr-order-key | 13.032 | 12.389 | 0.951 | -4.9% | 0.952 0.949 | 0.3% | 0.7% |
| group-avg | 41.080 | 39.546 | 0.963 | -3.7% | 0.964 0.960 | 0.9% | 0.5% |
| knows-reach | 138.230 | 138.493 | 1.002 | +0.2% | 1.006 1.003 | 0.8% | 0.7% |
| lang-filter | 18.801 | 18.869 | 1.004 | +0.4% | 1.003 1.004 | 0.5% | 0.9% |
| minus | 5.039 | 3.731 | 0.740 | -26.0% | 0.751 0.726 | 3.2% | 3.0% |
| not-exists | 10.241 | 9.947 | 0.971 | -2.9% | 0.971 0.966 | 2.4% | 0.6% |
| optional-chain | 136.004 | 222.053 | 1.633 | +63.3% | 1.621 1.637 | 1.4% | 0.1% |
| optional-count | 5.825 | 6.583 | 1.130 | +13.0% | 1.135 1.119 | 2.5% | 5.6% |
| order-by-full | 858.048 | 1066.690 | 1.243 | +24.3% | 1.245 1.242 | 0.5% | 0.2% |
| path-plus | 1.946 | 1.974 | 1.015 | +1.5% | 1.001 1.247 | 74.9% | 0.2% |
| predicate-counts | 1.298 | 1.982 | 1.527 | +52.7% | 1.759 1.137 | 75.1% | 0.1% |
| predicate-subjects | 1.971 | 1.982 | 1.006 | +0.6% | 1.005 1.007 | 23.2% | 0.2% |
| range-topk | 6.450 | 6.447 | 1.000 | -0.0% | 0.971 1.012 | 7.8% | 3.9% |
| regex-iri | 39.011 | 38.985 | 0.999 | -0.1% | 0.998 0.999 | 1.7% | 0.4% |
| star-join | 36.058 | 52.274 | 1.450 | +45.0% | 1.467 1.448 | 0.3% | 1.5% |
| star-lookup | 1.985 | 1.986 | 1.001 | +0.1% | 1.001 1.001 | 0.2% | 1.9% |
| subquery-agg | 2.878 | 2.913 | 1.012 | +1.2% | 1.010 1.013 | 4.2% | 2.0% |
| two-hop-count | 29.172 | 29.348 | 1.006 | +0.6% | 1.006 1.006 | 0.9% | 1.3% |
| types-grouped | 1.970 | 1.974 | 1.002 | +0.2% | 1.002 1.222 | 29.5% | 0.2% |
| values-star | 1.980 | 1.982 | 1.001 | +0.1% | 1.002 1.000 | 0.2% | 0.2% |
| **geometric mean** | | | 1.060 | +6.0% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
