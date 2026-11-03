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
- data sha256: c2d387f07a0717fdcd1d001081507b5ac09608961f562f0102d971c36e36edd8
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
| class-predicate-list | 0.966 | 0.994 | 1.029 | +2.9% | 1.036 1.026 | 8.3% | 9.5% |
| class-predicates | 0.973 | 1.009 | 1.037 | +3.7% | 1.036 1.044 | 0.7% | 4.7% |
| contains | 0.718 | 0.708 | 0.987 | -1.3% | 0.950 1.007 | 17.7% | 3.3% |
| count-all | 0.008 | 0.010 | 1.284 | +28.4% | 1.465 1.249 | 23.1% | 21.2% |
| distinct-join | 3.497 | 4.004 | 1.145 | +14.5% | 1.140 1.145 | 4.1% | 4.5% |
| distinct-obj | 0.007 | 0.009 | 1.393 | +39.3% | 1.410 1.459 | 12.6% | 43.9% |
| employee-docs | 0.050 | 0.054 | 1.080 | +8.0% | 1.099 1.047 | 5.8% | 10.0% |
| exists-join | 1.104 | 1.141 | 1.034 | +3.4% | 1.035 1.033 | 2.5% | 2.0% |
| export-500k | 2.370 | 0.408 | 0.172 | -82.8% | 0.173 0.165 | 6.0% | 1.1% |
| expr-agg-arg | 2.935 | 2.080 | 0.708 | -29.2% | 0.703 0.710 | 3.3% | 3.6% |
| expr-bind-group | 0.789 | 0.401 | 0.508 | -49.2% | 0.504 0.531 | 14.1% | 9.1% |
| expr-order-key | 1.247 | 1.261 | 1.012 | +1.2% | 1.008 1.017 | 2.9% | 4.0% |
| group-avg | 4.370 | 3.562 | 0.815 | -18.5% | 0.798 0.824 | 9.5% | 1.5% |
| knows-reach | 11.789 | 11.595 | 0.984 | -1.6% | 0.983 0.987 | 2.1% | 0.9% |
| lang-filter | 0.775 | 0.760 | 0.980 | -2.0% | 1.036 0.880 | 12.4% | 22.2% |
| minus | 0.348 | 0.298 | 0.855 | -14.5% | 0.899 0.820 | 7.1% | 15.0% |
| not-exists | 0.878 | 0.818 | 0.931 | -6.9% | 0.924 0.941 | 2.9% | 2.6% |
| optional-chain | 2.147 | 2.113 | 0.984 | -1.6% | 0.954 1.049 | 5.4% | 8.7% |
| optional-count | 0.555 | 0.544 | 0.981 | -1.9% | 0.942 1.012 | 9.5% | 4.4% |
| order-by-full | 14.168 | 14.482 | 1.022 | +2.2% | 1.041 1.006 | 7.6% | 3.7% |
| path-plus | 0.018 | 0.020 | 1.105 | +10.5% | 1.152 1.080 | 9.3% | 42.0% |
| predicate-counts | 0.006 | 0.010 | 1.697 | +69.7% | 1.350 1.761 | 47.3% | 8.7% |
| predicate-subjects | 0.011 | 0.015 | 1.398 | +39.8% | 1.600 1.341 | 35.7% | 10.8% |
| range-topk | 0.568 | 0.572 | 1.007 | +0.7% | 0.972 1.018 | 6.8% | 2.7% |
| regex-iri | 1.036 | 1.047 | 1.011 | +1.1% | 0.990 1.079 | 5.9% | 8.5% |
| star-join | 1.644 | 1.675 | 1.018 | +1.8% | 0.979 1.036 | 7.8% | 11.5% |
| star-lookup | 0.084 | 0.089 | 1.055 | +5.5% | 1.052 1.040 | 8.0% | 6.1% |
| subquery-agg | 0.221 | 0.188 | 0.854 | -14.6% | 1.076 0.708 | 45.1% | 40.0% |
| two-hop-count | 1.231 | 1.252 | 1.017 | +1.7% | 1.053 0.991 | 11.8% | 7.7% |
| types-grouped | 0.012 | 0.022 | 1.834 | +83.4% | 2.480 1.628 | 6.8% | 61.6% |
| values-star | 0.020 | 0.032 | 1.590 | +59.0% | 1.583 1.619 | 3.6% | 37.9% |
| **geometric mean** | | | 0.987 | -1.3% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 1.661 | 1.910 | 1.150 | +15.0% | 1.168 1.140 | 2.2% | 3.3% |
| class-predicates | 1.672 | 1.948 | 1.165 | +16.5% | 1.159 1.169 | 2.5% | 5.1% |
| contains | 1.413 | 1.593 | 1.128 | +12.8% | 1.118 1.155 | 6.0% | 5.4% |
| count-all | 0.473 | 0.756 | 1.598 | +59.8% | 1.672 1.574 | 15.6% | 8.7% |
| distinct-join | 4.771 | 5.613 | 1.177 | +17.7% | 1.186 1.158 | 2.9% | 8.7% |
| distinct-obj | 0.475 | 0.780 | 1.641 | +64.1% | 1.707 1.592 | 18.8% | 9.7% |
| employee-docs | 0.685 | 0.899 | 1.312 | +31.2% | 1.376 1.224 | 4.1% | 18.0% |
| exists-join | 1.860 | 2.183 | 1.174 | +17.4% | 1.179 1.153 | 2.9% | 11.0% |
| export-500k | 249.233 | 279.998 | 1.123 | +12.3% | 1.123 1.125 | 0.2% | 0.4% |
| expr-agg-arg | 3.740 | 3.087 | 0.826 | -17.4% | 0.820 0.833 | 3.0% | 5.0% |
| expr-bind-group | 1.492 | 1.367 | 0.916 | -8.4% | 0.917 0.915 | 10.2% | 8.2% |
| expr-order-key | 1.970 | 2.221 | 1.128 | +12.8% | 1.147 1.104 | 2.2% | 6.9% |
| group-avg | 5.177 | 4.617 | 0.892 | -10.8% | 0.880 0.888 | 7.0% | 2.1% |
| knows-reach | 12.815 | 12.640 | 0.986 | -1.4% | 0.986 0.979 | 2.5% | 1.0% |
| lang-filter | 1.464 | 1.656 | 1.131 | +13.1% | 1.163 1.112 | 6.7% | 16.5% |
| minus | 1.001 | 1.142 | 1.141 | +14.1% | 1.198 1.086 | 6.3% | 11.8% |
| not-exists | 1.575 | 1.761 | 1.118 | +11.8% | 1.145 1.086 | 3.5% | 5.8% |
| optional-chain | 14.663 | 17.795 | 1.214 | +21.4% | 1.206 1.216 | 3.4% | 5.8% |
| optional-count | 1.257 | 1.429 | 1.136 | +13.6% | 1.192 1.099 | 4.6% | 9.1% |
| order-by-full | 48.629 | 54.140 | 1.113 | +11.3% | 1.120 1.108 | 2.0% | 1.0% |
| path-plus | 0.521 | 0.775 | 1.488 | +48.8% | 1.490 1.509 | 15.2% | 13.9% |
| predicate-counts | 0.464 | 0.802 | 1.729 | +72.9% | 1.592 1.821 | 31.0% | 11.9% |
| predicate-subjects | 0.481 | 0.756 | 1.572 | +57.2% | 1.635 1.551 | 25.6% | 6.7% |
| range-topk | 1.378 | 1.617 | 1.173 | +17.3% | 1.201 1.157 | 2.1% | 8.1% |
| regex-iri | 1.742 | 1.937 | 1.112 | +11.2% | 1.057 1.170 | 7.2% | 10.0% |
| star-join | 5.040 | 5.761 | 1.143 | +14.3% | 1.136 1.157 | 4.2% | 6.1% |
| star-lookup | 0.793 | 1.110 | 1.399 | +39.9% | 1.460 1.331 | 3.4% | 17.3% |
| subquery-agg | 0.821 | 1.046 | 1.274 | +27.4% | 1.443 1.121 | 5.4% | 30.6% |
| two-hop-count | 1.998 | 2.297 | 1.150 | +15.0% | 1.176 1.091 | 10.0% | 11.1% |
| types-grouped | 0.573 | 0.804 | 1.405 | +40.5% | 1.391 1.433 | 9.5% | 5.8% |
| values-star | 0.684 | 0.889 | 1.300 | +30.0% | 1.305 1.328 | 3.9% | 6.8% |
| **geometric mean** | | | 1.202 | +20.2% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
