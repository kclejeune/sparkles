# Interleaved A/B on Namespace

- binary: /home/kclejeune/lanes/exec-453/bin/sparkles-final-noinc
- binary sha256: c0774599858cac5034cfb26fc04c62b5d507b6d8115e3441bef3cb3945ae74f7
- binary commit: final
- version: sparkles 0.1.0
- compilers: GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- build config: {"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{},"mbx_policy":"779a99e845a5ac3f1c4d64a3c95bcc74b33f47418e829bf008542ec2dbbcd151","profile":"release"}
- runtime env: clean
- scripts commit: unknown
- scripts dirty: 0
- host: forge
- started: 2026-11-02T18:32:08Z
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=12, server CPU 0-11, client CPU 12, spinners off, execution eager,streaming
- A: /home/kclejeune/lanes/exec-453/bin/sparkles-final-noinc
- B: /home/kclejeune/lanes/exec-453/bin/sparkles-final-noinc
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
| class-predicate-list | 9.945 | 10.508 | 1.057 | +5.7% | 1.059 1.041 | 1.7% | 2.1% |
| class-predicates | 10.037 | 10.442 | 1.040 | +4.0% | 1.039 1.045 | 1.2% | 3.8% |
| contains | 4.141 | 4.148 | 1.002 | +0.2% | 1.015 1.001 | 2.4% | 1.8% |
| count-all | 0.009 | 0.013 | 1.341 | +34.1% | 1.346 1.337 | 5.3% | 24.0% |
| distinct-join | 38.552 | 38.876 | 1.008 | +0.8% | 1.025 1.008 | 0.7% | 4.0% |
| distinct-obj | 0.007 | 0.011 | 1.666 | +66.6% | 1.854 1.528 | 31.4% | 55.8% |
| employee-docs | 0.068 | 0.077 | 1.132 | +13.2% | 1.187 1.030 | 7.9% | 45.5% |
| exists-join | 8.912 | 8.934 | 1.003 | +0.3% | 0.979 1.022 | 6.6% | 6.2% |
| export-500k | 2.328 | 0.409 | 0.176 | -82.4% | 0.118 0.189 | 117.5% | 2.2% |
| expr-agg-arg | 28.509 | 23.547 | 0.826 | -17.4% | 0.814 0.840 | 4.2% | 0.8% |
| expr-bind-group | 4.750 | 3.423 | 0.721 | -27.9% | 0.723 0.723 | 2.7% | 4.0% |
| expr-order-key | 10.496 | 11.234 | 1.070 | +7.0% | 1.069 1.075 | 2.3% | 0.7% |
| group-avg | 47.578 | 40.071 | 0.842 | -15.8% | 0.841 0.843 | 0.3% | 1.5% |
| knows-reach | 141.699 | 139.594 | 0.985 | -1.5% | 0.987 0.985 | 1.3% | 0.6% |
| lang-filter | 3.123 | 3.055 | 0.978 | -2.2% | 0.981 0.982 | 3.4% | 2.4% |
| minus | 3.287 | 2.583 | 0.786 | -21.4% | 0.797 0.774 | 4.4% | 4.3% |
| not-exists | 9.701 | 8.935 | 0.921 | -7.9% | 0.905 0.932 | 5.8% | 0.9% |
| optional-chain | 29.340 | 25.564 | 0.871 | -12.9% | 0.897 0.803 | 64.2% | 6.5% |
| optional-count | 5.360 | 5.222 | 0.974 | -2.6% | 0.990 0.965 | 0.4% | 3.2% |
| order-by-full | 250.395 | 268.349 | 1.072 | +7.2% | 1.272 0.971 | 48.3% | 39.9% |
| path-plus | 0.014 | 0.014 | 1.047 | +4.7% | 1.068 1.067 | 18.4% | 31.8% |
| predicate-counts | 0.008 | 0.015 | 1.869 | +86.9% | 1.420 2.014 | 9.3% | 47.9% |
| predicate-subjects | 0.009 | 0.015 | 1.658 | +65.8% | 1.506 2.077 | 17.9% | 47.8% |
| range-topk | 2.424 | 2.323 | 0.958 | -4.2% | 0.984 0.933 | 31.5% | 12.2% |
| regex-iri | 6.513 | 6.444 | 0.989 | -1.1% | 0.985 0.997 | 3.2% | 3.0% |
| star-join | 13.210 | 13.868 | 1.050 | +5.0% | 1.040 1.063 | 2.5% | 3.0% |
| star-lookup | 0.073 | 0.084 | 1.154 | +15.4% | 1.185 1.126 | 4.8% | 3.6% |
| subquery-agg | 0.816 | 0.810 | 0.992 | -0.8% | 0.987 0.982 | 5.9% | 11.1% |
| two-hop-count | 11.140 | 11.122 | 0.998 | -0.2% | 0.998 0.997 | 2.8% | 3.1% |
| types-grouped | 0.012 | 0.019 | 1.585 | +58.5% | 1.574 1.647 | 15.2% | 13.8% |
| values-star | 0.021 | 0.028 | 1.365 | +36.5% | 1.377 1.354 | 5.5% | 2.7% |
| **geometric mean** | | | 1.010 | +1.0% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 11.068 | 11.581 | 1.046 | +4.6% | 1.056 1.030 | 3.8% | 3.5% |
| class-predicates | 11.139 | 11.610 | 1.042 | +4.2% | 1.045 1.036 | 2.2% | 5.5% |
| contains | 4.902 | 5.119 | 1.044 | +4.4% | 1.062 1.034 | 2.1% | 2.7% |
| count-all | 0.468 | 0.744 | 1.589 | +58.9% | 1.650 1.576 | 3.5% | 29.5% |
| distinct-join | 44.192 | 44.859 | 1.015 | +1.5% | 1.022 1.012 | 1.3% | 4.5% |
| distinct-obj | 0.470 | 0.748 | 1.593 | +59.3% | 1.503 1.683 | 11.2% | 26.7% |
| employee-docs | 0.710 | 0.927 | 1.304 | +30.4% | 1.312 1.290 | 7.4% | 17.2% |
| exists-join | 10.079 | 10.088 | 1.001 | +0.1% | 0.995 1.028 | 6.2% | 7.9% |
| export-500k | 253.042 | 271.828 | 1.074 | +7.4% | 1.073 1.077 | 2.2% | 0.2% |
| expr-agg-arg | 29.685 | 24.712 | 0.832 | -16.8% | 0.823 0.847 | 4.1% | 0.5% |
| expr-bind-group | 5.567 | 4.489 | 0.806 | -19.4% | 0.800 0.813 | 2.6% | 5.3% |
| expr-order-key | 12.267 | 12.305 | 1.003 | +0.3% | 1.006 1.005 | 0.8% | 1.0% |
| group-avg | 49.073 | 41.388 | 0.843 | -15.7% | 0.840 0.846 | 0.8% | 1.4% |
| knows-reach | 143.020 | 140.894 | 0.985 | -1.5% | 0.987 0.985 | 1.3% | 0.5% |
| lang-filter | 3.883 | 3.994 | 1.028 | +2.8% | 1.027 1.037 | 2.7% | 3.8% |
| minus | 4.072 | 3.552 | 0.872 | -12.8% | 0.879 0.866 | 4.6% | 3.6% |
| not-exists | 10.721 | 9.973 | 0.930 | -7.0% | 0.923 0.934 | 9.0% | 0.9% |
| optional-chain | 141.600 | 161.960 | 1.144 | +14.4% | 1.159 1.091 | 33.0% | 1.8% |
| optional-count | 6.229 | 6.242 | 1.002 | +0.2% | 1.015 1.000 | 0.8% | 3.4% |
| order-by-full | 595.639 | 649.162 | 1.090 | +9.0% | 1.128 1.045 | 25.0% | 16.4% |
| path-plus | 0.472 | 0.739 | 1.566 | +56.6% | 1.628 1.538 | 22.0% | 21.0% |
| predicate-counts | 0.533 | 0.833 | 1.564 | +56.4% | 1.683 1.445 | 12.2% | 16.4% |
| predicate-subjects | 0.459 | 0.822 | 1.793 | +79.3% | 1.826 1.737 | 17.2% | 6.5% |
| range-topk | 3.330 | 3.392 | 1.018 | +1.8% | 1.063 0.981 | 22.6% | 7.6% |
| regex-iri | 7.321 | 7.453 | 1.018 | +1.8% | 1.021 1.020 | 2.9% | 3.2% |
| star-join | 33.165 | 42.378 | 1.278 | +27.8% | 1.281 1.273 | 0.7% | 0.5% |
| star-lookup | 0.788 | 1.064 | 1.349 | +34.9% | 1.396 1.352 | 5.3% | 10.9% |
| subquery-agg | 1.585 | 1.742 | 1.099 | +9.9% | 1.033 1.139 | 4.1% | 16.4% |
| two-hop-count | 12.318 | 12.273 | 0.996 | -0.4% | 0.997 0.994 | 2.0% | 1.8% |
| types-grouped | 0.562 | 0.857 | 1.524 | +52.4% | 1.524 1.419 | 11.3% | 23.9% |
| values-star | 0.690 | 0.925 | 1.340 | +34.0% | 1.370 1.312 | 3.3% | 14.1% |
| **geometric mean** | | | 1.128 | +12.8% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
