# Interleaved A/B on Namespace

- binary: /home/kclejeune/lanes/exec-453/bin/sparkles-e52937ca
- binary sha256: b403198489322036131b9e64908761ce12dbd9e67087cc90dca38943fe6b172e
- binary commit: e52937ca
- version: sparkles 0.1.0
- compilers: GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- build config: {"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{},"mbx_policy":"d8bf0b48d620f6bf62929d3db32bd253653505cdb33a5bd1efed1e79aaca05b4","profile":"release"}
- runtime env: clean
- scripts commit: unknown
- scripts dirty: 0
- host: forge
- started: 2026-11-02T03:37:44Z
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=12, server CPU 0-11, client CPU 12, spinners off, execution eager,streaming
- A: /home/kclejeune/lanes/exec-453/bin/sparkles-e52937ca
- B: /home/kclejeune/lanes/exec-453/bin/sparkles-e52937ca
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
| class-predicate-list | 10.178 | 10.497 | 1.031 | +3.1% | 1.027 1.034 | 3.6% | 1.7% |
| class-predicates | 10.116 | 10.517 | 1.040 | +4.0% | 1.032 1.049 | 1.8% | 2.3% |
| contains | 4.004 | 4.080 | 1.019 | +1.9% | 1.030 1.008 | 0.8% | 3.4% |
| count-all | 0.008 | 0.012 | 1.495 | +49.5% | 1.531 1.515 | 11.3% | 66.8% |
| distinct-join | 38.520 | 38.051 | 0.988 | -1.2% | 0.980 0.991 | 1.0% | 1.6% |
| distinct-obj | 0.006 | 0.009 | 1.587 | +58.7% | 1.581 1.556 | 23.8% | 66.0% |
| employee-docs | 0.068 | 0.079 | 1.164 | +16.4% | 1.139 1.203 | 10.2% | 33.8% |
| exists-join | 8.852 | 8.825 | 0.997 | -0.3% | 0.951 1.033 | 7.1% | 3.5% |
| export-500k | 2.480 | 0.409 | 0.165 | -83.5% | 0.158 0.178 | 58.1% | 3.7% |
| expr-agg-arg | 28.034 | 23.225 | 0.828 | -17.2% | 0.826 0.831 | 1.7% | 1.7% |
| expr-bind-group | 4.812 | 3.436 | 0.714 | -28.6% | 0.716 0.729 | 4.1% | 2.8% |
| expr-order-key | 10.323 | 10.784 | 1.045 | +4.5% | 1.026 1.050 | 9.8% | 1.7% |
| group-avg | 47.820 | 40.089 | 0.838 | -16.2% | 0.831 0.841 | 7.7% | 0.7% |
| knows-reach | 142.627 | 140.373 | 0.984 | -1.6% | 0.989 0.976 | 5.5% | 1.3% |
| lang-filter | 2.806 | 2.869 | 1.022 | +2.2% | 1.055 1.000 | 3.8% | 3.8% |
| minus | 3.238 | 2.603 | 0.804 | -19.6% | 0.779 0.827 | 4.8% | 3.6% |
| not-exists | 9.798 | 8.934 | 0.912 | -8.8% | 0.935 0.906 | 4.2% | 4.9% |
| optional-chain | 28.455 | 25.517 | 0.897 | -10.3% | 0.891 0.894 | 22.9% | 7.8% |
| optional-count | 5.399 | 5.220 | 0.967 | -3.3% | 0.961 0.972 | 1.2% | 2.0% |
| order-by-full | 239.272 | 266.403 | 1.113 | +11.3% | 1.061 1.156 | 54.4% | 49.4% |
| path-plus | 0.014 | 0.014 | 1.033 | +3.3% | 1.097 0.979 | 57.7% | 75.3% |
| predicate-counts | 0.007 | 0.011 | 1.627 | +62.7% | 1.447 2.657 | 29.6% | 69.8% |
| predicate-subjects | 0.008 | 0.016 | 2.004 | +100.4% | 1.797 2.157 | 38.2% | 38.6% |
| range-topk | 2.456 | 2.247 | 0.915 | -8.5% | 0.912 0.972 | 38.7% | 26.1% |
| regex-iri | 5.991 | 6.239 | 1.041 | +4.1% | 1.038 1.045 | 2.3% | 0.8% |
| star-join | 13.063 | 13.501 | 1.034 | +3.4% | 1.031 1.036 | 34.4% | 1.5% |
| star-lookup | 0.073 | 0.082 | 1.130 | +13.0% | 1.122 1.146 | 3.5% | 19.7% |
| subquery-agg | 0.757 | 0.830 | 1.097 | +9.7% | 1.062 1.136 | 14.1% | 5.9% |
| two-hop-count | 11.194 | 11.240 | 1.004 | +0.4% | 0.997 1.003 | 1.8% | 1.4% |
| types-grouped | 0.010 | 0.020 | 1.938 | +93.8% | 2.010 1.900 | 9.9% | 24.4% |
| values-star | 0.020 | 0.027 | 1.316 | +31.6% | 1.311 1.359 | 4.4% | 13.4% |
| **geometric mean** | | | 1.021 | +2.1% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 11.274 | 11.575 | 1.027 | +2.7% | 1.030 1.023 | 3.8% | 0.7% |
| class-predicates | 11.186 | 11.627 | 1.040 | +4.0% | 1.036 1.048 | 1.0% | 1.2% |
| contains | 4.774 | 5.053 | 1.059 | +5.9% | 1.062 1.046 | 1.5% | 2.9% |
| count-all | 0.499 | 0.747 | 1.495 | +49.5% | 1.566 1.425 | 6.8% | 50.0% |
| distinct-join | 43.656 | 44.433 | 1.018 | +1.8% | 1.008 1.021 | 1.4% | 2.0% |
| distinct-obj | 0.504 | 0.746 | 1.481 | +48.1% | 1.675 1.390 | 15.5% | 26.0% |
| employee-docs | 0.706 | 0.946 | 1.340 | +34.0% | 1.382 1.302 | 10.3% | 30.2% |
| exists-join | 9.789 | 9.988 | 1.020 | +2.0% | 0.985 1.050 | 8.0% | 3.6% |
| export-500k | 228.080 | 292.973 | 1.285 | +28.5% | 1.288 1.285 | 0.7% | 0.6% |
| expr-agg-arg | 29.171 | 24.481 | 0.839 | -16.1% | 0.835 0.843 | 1.7% | 1.5% |
| expr-bind-group | 5.639 | 4.473 | 0.793 | -20.7% | 0.788 0.812 | 2.8% | 1.4% |
| expr-order-key | 12.107 | 11.844 | 0.978 | -2.2% | 0.967 0.984 | 8.0% | 0.8% |
| group-avg | 49.380 | 41.511 | 0.841 | -15.9% | 0.836 0.843 | 7.3% | 0.8% |
| knows-reach | 144.315 | 141.678 | 0.982 | -1.8% | 0.989 0.974 | 5.2% | 1.2% |
| lang-filter | 3.577 | 3.812 | 1.066 | +6.6% | 1.091 1.057 | 2.8% | 5.6% |
| minus | 4.050 | 3.550 | 0.877 | -12.3% | 0.878 0.873 | 3.3% | 5.7% |
| not-exists | 10.841 | 10.002 | 0.923 | -7.7% | 0.944 0.916 | 5.6% | 4.4% |
| optional-chain | 130.175 | 171.385 | 1.317 | +31.7% | 1.298 1.323 | 14.7% | 2.5% |
| optional-count | 6.247 | 6.236 | 0.998 | -0.2% | 0.981 1.009 | 1.9% | 4.3% |
| order-by-full | 577.207 | 668.779 | 1.159 | +15.9% | 1.187 1.174 | 35.7% | 19.1% |
| path-plus | 0.522 | 0.770 | 1.476 | +47.6% | 1.609 1.421 | 50.5% | 24.9% |
| predicate-counts | 0.523 | 0.801 | 1.532 | +53.2% | 1.464 1.676 | 17.7% | 9.0% |
| predicate-subjects | 0.435 | 0.792 | 1.820 | +82.0% | 1.740 1.800 | 13.0% | 10.6% |
| range-topk | 3.313 | 3.442 | 1.039 | +3.9% | 1.055 1.060 | 37.3% | 16.4% |
| regex-iri | 6.793 | 7.359 | 1.083 | +8.3% | 1.083 1.084 | 2.1% | 1.9% |
| star-join | 32.124 | 43.770 | 1.363 | +36.3% | 1.370 1.368 | 16.9% | 2.7% |
| star-lookup | 0.792 | 1.139 | 1.438 | +43.8% | 1.462 1.421 | 2.0% | 6.5% |
| subquery-agg | 1.514 | 2.008 | 1.326 | +32.6% | 1.307 1.327 | 7.9% | 9.8% |
| two-hop-count | 12.369 | 12.401 | 1.003 | +0.3% | 1.001 1.004 | 2.9% | 1.2% |
| types-grouped | 0.565 | 0.814 | 1.441 | +44.1% | 1.545 1.400 | 2.9% | 20.2% |
| values-star | 0.667 | 0.956 | 1.432 | +43.2% | 1.445 1.367 | 3.3% | 18.6% |
| **geometric mean** | | | 1.151 | +15.1% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
