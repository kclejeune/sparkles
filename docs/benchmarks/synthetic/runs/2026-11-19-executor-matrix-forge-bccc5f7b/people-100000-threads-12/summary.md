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
| class-predicate-list | 0.989 | 1.029 | 1.040 | +4.0% | 1.030 1.047 | 0.8% | 3.1% |
| class-predicates | 0.997 | 1.052 | 1.055 | +5.5% | 1.044 1.065 | 3.0% | 1.7% |
| contains | 0.732 | 0.762 | 1.042 | +4.2% | 1.071 1.019 | 14.1% | 10.0% |
| count-all | 0.007 | 0.010 | 1.517 | +51.7% | 1.500 1.466 | 35.6% | 31.0% |
| distinct-join | 3.695 | 3.919 | 1.060 | +6.0% | 1.069 1.056 | 2.9% | 2.6% |
| distinct-obj | 0.006 | 0.009 | 1.396 | +39.6% | 1.355 1.459 | 9.2% | 7.3% |
| employee-docs | 0.051 | 0.056 | 1.111 | +11.1% | 1.120 1.097 | 4.3% | 5.6% |
| exists-join | 1.145 | 1.142 | 0.997 | -0.3% | 1.008 0.974 | 7.2% | 3.2% |
| export-500k | 2.377 | 0.376 | 0.158 | -84.2% | 0.159 0.157 | 22.7% | 2.9% |
| expr-agg-arg | 2.679 | 2.068 | 0.772 | -22.8% | 0.826 0.761 | 9.7% | 4.5% |
| expr-bind-group | 0.668 | 0.395 | 0.592 | -40.8% | 0.655 0.580 | 22.5% | 6.3% |
| expr-order-key | 1.253 | 1.271 | 1.014 | +1.4% | 0.957 1.039 | 11.7% | 3.4% |
| group-avg | 3.887 | 3.418 | 0.879 | -12.1% | 0.865 0.903 | 5.4% | 4.0% |
| knows-reach | 11.640 | 11.705 | 1.006 | +0.6% | 1.004 1.012 | 1.1% | 1.7% |
| lang-filter | 0.756 | 0.816 | 1.080 | +8.0% | 1.140 1.041 | 14.6% | 3.5% |
| minus | 0.435 | 0.287 | 0.658 | -34.2% | 0.656 0.656 | 4.5% | 5.0% |
| not-exists | 0.833 | 0.813 | 0.976 | -2.4% | 0.937 0.993 | 7.3% | 1.0% |
| optional-chain | 2.141 | 2.173 | 1.015 | +1.5% | 1.050 0.998 | 6.7% | 2.9% |
| optional-count | 0.469 | 0.602 | 1.283 | +28.3% | 1.262 1.311 | 5.9% | 6.1% |
| order-by-full | 9.415 | 9.681 | 1.028 | +2.8% | 1.013 1.022 | 7.5% | 7.2% |
| path-plus | 0.020 | 0.019 | 0.954 | -4.6% | 0.983 0.945 | 13.7% | 20.9% |
| predicate-counts | 0.007 | 0.011 | 1.538 | +53.8% | 1.557 1.516 | 8.0% | 15.2% |
| predicate-subjects | 0.010 | 0.016 | 1.575 | +57.5% | 1.763 1.564 | 20.5% | 31.7% |
| range-topk | 0.570 | 0.579 | 1.015 | +1.5% | 1.008 1.005 | 5.3% | 3.1% |
| regex-iri | 0.974 | 1.025 | 1.052 | +5.2% | 1.094 0.987 | 6.8% | 12.7% |
| star-join | 1.540 | 1.637 | 1.063 | +6.3% | 1.092 1.036 | 9.8% | 9.6% |
| star-lookup | 0.087 | 0.092 | 1.057 | +5.7% | 1.081 1.042 | 4.2% | 9.7% |
| subquery-agg | 0.241 | 0.231 | 0.958 | -4.2% | 1.058 0.864 | 25.6% | 47.6% |
| two-hop-count | 1.242 | 1.252 | 1.008 | +0.8% | 1.023 0.986 | 6.3% | 2.7% |
| types-grouped | 0.011 | 0.021 | 1.845 | +84.5% | 1.794 1.884 | 9.8% | 28.5% |
| values-star | 0.020 | 0.029 | 1.420 | +42.0% | 1.627 1.265 | 16.6% | 62.2% |
| **geometric mean** | | | 1.006 | +0.6% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 1.677 | 1.935 | 1.154 | +15.4% | 1.130 1.160 | 2.1% | 6.4% |
| class-predicates | 1.700 | 1.981 | 1.165 | +16.5% | 1.164 1.153 | 2.5% | 3.1% |
| contains | 1.451 | 1.685 | 1.162 | +16.2% | 1.191 1.124 | 5.9% | 3.6% |
| count-all | 0.450 | 0.825 | 1.834 | +83.4% | 1.885 1.828 | 23.6% | 15.0% |
| distinct-join | 4.949 | 5.543 | 1.120 | +12.0% | 1.136 1.103 | 2.7% | 5.0% |
| distinct-obj | 0.508 | 0.802 | 1.578 | +57.8% | 1.529 1.632 | 13.2% | 8.1% |
| employee-docs | 0.688 | 0.978 | 1.422 | +42.2% | 1.400 1.422 | 2.0% | 5.0% |
| exists-join | 1.916 | 2.135 | 1.114 | +11.4% | 1.169 1.064 | 3.8% | 10.7% |
| export-500k | 216.165 | 292.121 | 1.351 | +35.1% | 1.352 1.350 | 0.9% | 0.6% |
| expr-agg-arg | 3.494 | 3.110 | 0.890 | -11.0% | 0.913 0.889 | 5.5% | 4.8% |
| expr-bind-group | 1.370 | 1.334 | 0.974 | -2.6% | 1.021 0.933 | 8.0% | 14.4% |
| expr-order-key | 1.994 | 2.277 | 1.142 | +14.2% | 1.119 1.157 | 5.0% | 7.0% |
| group-avg | 4.737 | 4.478 | 0.945 | -5.5% | 0.936 0.956 | 4.5% | 3.9% |
| knows-reach | 12.710 | 12.824 | 1.009 | +0.9% | 1.009 1.007 | 2.4% | 1.8% |
| lang-filter | 1.455 | 1.727 | 1.187 | +18.7% | 1.223 1.141 | 11.1% | 7.1% |
| minus | 1.123 | 1.150 | 1.024 | +2.4% | 0.996 1.038 | 3.3% | 8.3% |
| not-exists | 1.533 | 1.701 | 1.110 | +11.0% | 1.130 1.108 | 2.9% | 5.1% |
| optional-chain | 14.146 | 18.663 | 1.319 | +31.9% | 1.313 1.329 | 3.3% | 1.7% |
| optional-count | 1.194 | 1.535 | 1.286 | +28.6% | 1.308 1.269 | 6.3% | 4.9% |
| order-by-full | 42.076 | 52.117 | 1.239 | +23.9% | 1.230 1.268 | 2.7% | 4.3% |
| path-plus | 0.516 | 0.770 | 1.491 | +49.1% | 1.743 1.365 | 18.7% | 27.4% |
| predicate-counts | 0.534 | 0.879 | 1.644 | +64.4% | 1.715 1.520 | 20.7% | 14.6% |
| predicate-subjects | 0.497 | 0.861 | 1.733 | +73.3% | 1.919 1.611 | 18.6% | 15.4% |
| range-topk | 1.384 | 1.673 | 1.209 | +20.9% | 1.247 1.171 | 5.0% | 7.5% |
| regex-iri | 1.717 | 1.837 | 1.070 | +7.0% | 1.119 1.046 | 4.3% | 5.9% |
| star-join | 4.876 | 5.943 | 1.219 | +21.9% | 1.232 1.208 | 3.7% | 2.0% |
| star-lookup | 0.819 | 1.097 | 1.338 | +33.8% | 1.354 1.303 | 5.0% | 16.2% |
| subquery-agg | 0.860 | 1.007 | 1.172 | +17.2% | 1.157 1.174 | 10.9% | 25.0% |
| two-hop-count | 2.032 | 2.321 | 1.143 | +14.3% | 1.141 1.135 | 5.0% | 6.2% |
| types-grouped | 0.578 | 0.817 | 1.413 | +41.3% | 1.451 1.373 | 10.0% | 21.2% |
| values-star | 0.681 | 0.839 | 1.231 | +23.1% | 1.326 1.204 | 5.1% | 19.1% |
| **geometric mean** | | | 1.230 | +23.0% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
