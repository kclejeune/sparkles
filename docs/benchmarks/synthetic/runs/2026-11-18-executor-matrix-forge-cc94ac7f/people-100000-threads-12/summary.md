# Interleaved A/B on Namespace

- binary: /home/kclejeune/lanes/forge-backlog/bin/sparkles-cc94ac7f-matrix
- binary sha256: 9efa1cd4655ff0d75af758bf0bb0f54ff5950f152b61b3e0afb765ff63e0fcba
- binary commit: cc94ac7f
- version: sparkles 0.1.0
- compilers: GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- build config: {"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1},"sparkles-owl":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{"CARGO_INCREMENTAL":"0"},"mbx_policy":"779a99e845a5ac3f1c4d64a3c95bcc74b33f47418e829bf008542ec2dbbcd151","profile":"release"}
- runtime env: clean
- scripts commit: unknown
- scripts dirty: 0
- host: forge
- started: 2026-11-18T07:04:20Z
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=12, server CPU 0-11, client CPU 12, spinners off, execution eager,streaming
- A: /home/kclejeune/lanes/forge-backlog/bin/sparkles-cc94ac7f-matrix
- B: /home/kclejeune/lanes/forge-backlog/bin/sparkles-cc94ac7f-matrix
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
| class-predicate-list | 0.980 | 0.977 | 0.998 | -0.2% | 1.019 0.979 | 6.3% | 6.4% |
| class-predicates | 1.003 | 0.985 | 0.982 | -1.8% | 0.998 0.965 | 8.2% | 9.1% |
| contains | 0.730 | 0.759 | 1.040 | +4.0% | 0.971 1.056 | 16.9% | 9.7% |
| count-all | 0.007 | 0.009 | 1.269 | +26.9% | 1.126 1.389 | 21.2% | 226.9% |
| distinct-join | 3.579 | 3.766 | 1.052 | +5.2% | 1.035 1.067 | 2.7% | 6.1% |
| distinct-obj | 0.007 | 0.008 | 1.153 | +15.3% | 1.018 1.169 | 8.8% | 260.6% |
| employee-docs | 0.049 | 0.053 | 1.078 | +7.8% | 1.098 1.035 | 7.3% | 94.9% |
| exists-join | 1.104 | 1.101 | 0.997 | -0.3% | 1.003 0.987 | 7.9% | 5.5% |
| export-500k | 2.330 | 0.411 | 0.176 | -82.4% | 0.172 0.181 | 10.6% | 1.3% |
| expr-agg-arg | 2.936 | 2.007 | 0.684 | -31.6% | 0.665 0.694 | 11.7% | 6.5% |
| expr-bind-group | 0.823 | 0.398 | 0.484 | -51.6% | 0.511 0.445 | 18.1% | 10.4% |
| expr-order-key | 1.234 | 1.280 | 1.038 | +3.8% | 1.028 1.066 | 7.5% | 10.1% |
| group-avg | 4.141 | 3.401 | 0.821 | -17.9% | 0.825 0.814 | 5.8% | 1.8% |
| knows-reach | 11.768 | 11.655 | 0.990 | -1.0% | 0.999 0.986 | 2.9% | 3.2% |
| lang-filter | 0.754 | 0.729 | 0.967 | -3.3% | 0.977 0.956 | 13.3% | 7.2% |
| minus | 0.340 | 0.278 | 0.816 | -18.4% | 0.805 0.821 | 6.6% | 6.5% |
| not-exists | 0.856 | 0.797 | 0.931 | -6.9% | 0.924 0.944 | 2.6% | 2.6% |
| optional-chain | 2.129 | 2.097 | 0.985 | -1.5% | 0.987 0.984 | 7.4% | 6.5% |
| optional-count | 0.536 | 0.558 | 1.040 | +4.0% | 1.018 1.067 | 4.3% | 9.7% |
| order-by-full | 8.920 | 9.247 | 1.037 | +3.7% | 1.053 1.027 | 8.5% | 6.5% |
| path-plus | 0.019 | 0.020 | 1.080 | +8.0% | 1.037 1.087 | 18.9% | 27.3% |
| predicate-counts | 0.007 | 0.010 | 1.484 | +48.4% | 1.515 1.620 | 48.3% | 86.8% |
| predicate-subjects | 0.009 | 0.015 | 1.684 | +68.4% | 1.653 1.640 | 32.8% | 45.1% |
| range-topk | 0.572 | 0.567 | 0.992 | -0.8% | 0.962 1.004 | 14.2% | 4.8% |
| regex-iri | 0.986 | 0.958 | 0.972 | -2.8% | 0.967 0.941 | 18.2% | 8.9% |
| star-join | 1.614 | 1.643 | 1.018 | +1.8% | 0.990 1.058 | 6.6% | 7.1% |
| star-lookup | 0.084 | 0.094 | 1.116 | +11.6% | 1.096 1.138 | 9.8% | 6.2% |
| subquery-agg | 0.233 | 0.225 | 0.965 | -3.5% | 0.691 1.090 | 27.7% | 50.6% |
| two-hop-count | 1.214 | 1.253 | 1.032 | +3.2% | 1.068 1.019 | 7.8% | 14.1% |
| types-grouped | 0.012 | 0.019 | 1.573 | +57.3% | 1.576 1.568 | 12.7% | 8.4% |
| values-star | 0.019 | 0.028 | 1.443 | +44.3% | 1.316 1.518 | 7.3% | 21.5% |
| **geometric mean** | | | 0.973 | -2.7% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 1.667 | 1.851 | 1.110 | +11.0% | 1.087 1.102 | 5.0% | 11.2% |
| class-predicates | 1.700 | 1.854 | 1.091 | +9.1% | 1.147 1.058 | 3.3% | 20.0% |
| contains | 1.470 | 1.607 | 1.093 | +9.3% | 1.077 1.157 | 9.5% | 5.1% |
| count-all | 0.449 | 0.723 | 1.612 | +61.2% | 1.383 1.660 | 19.4% | 40.3% |
| distinct-join | 4.849 | 5.688 | 1.173 | +17.3% | 1.099 1.180 | 2.3% | 21.2% |
| distinct-obj | 0.480 | 0.771 | 1.606 | +60.6% | 1.385 1.667 | 10.4% | 55.5% |
| employee-docs | 0.688 | 0.887 | 1.288 | +28.8% | 1.267 1.292 | 2.4% | 34.4% |
| exists-join | 1.861 | 2.114 | 1.136 | +13.6% | 1.080 1.140 | 3.9% | 19.4% |
| export-500k | 236.463 | 270.645 | 1.145 | +14.5% | 1.144 1.161 | 0.4% | 1.6% |
| expr-agg-arg | 3.766 | 2.992 | 0.794 | -20.6% | 0.786 0.793 | 8.0% | 11.7% |
| expr-bind-group | 1.542 | 1.286 | 0.835 | -16.5% | 0.900 0.783 | 5.7% | 26.6% |
| expr-order-key | 1.980 | 2.191 | 1.106 | +10.6% | 1.097 1.118 | 3.5% | 4.1% |
| group-avg | 5.027 | 4.462 | 0.888 | -11.2% | 0.888 0.880 | 4.1% | 2.1% |
| knows-reach | 12.796 | 12.763 | 0.997 | -0.3% | 1.002 0.991 | 1.9% | 3.0% |
| lang-filter | 1.458 | 1.538 | 1.055 | +5.5% | 1.109 1.033 | 12.2% | 13.3% |
| minus | 1.035 | 1.192 | 1.151 | +15.1% | 1.195 1.076 | 6.9% | 23.2% |
| not-exists | 1.562 | 1.758 | 1.125 | +12.5% | 1.133 1.073 | 0.5% | 20.1% |
| optional-chain | 14.561 | 17.710 | 1.216 | +21.6% | 1.239 1.200 | 4.2% | 3.7% |
| optional-count | 1.217 | 1.381 | 1.135 | +13.5% | 1.143 1.149 | 3.0% | 4.2% |
| order-by-full | 43.306 | 48.442 | 1.119 | +11.9% | 1.138 1.109 | 2.6% | 3.6% |
| path-plus | 0.499 | 0.794 | 1.589 | +58.9% | 1.572 1.672 | 8.6% | 24.4% |
| predicate-counts | 0.472 | 0.835 | 1.768 | +76.8% | 1.726 1.728 | 28.3% | 19.5% |
| predicate-subjects | 0.442 | 0.824 | 1.864 | +86.4% | 1.768 2.029 | 12.3% | 15.7% |
| range-topk | 1.377 | 1.554 | 1.128 | +12.8% | 1.097 1.188 | 1.7% | 9.0% |
| regex-iri | 1.712 | 1.792 | 1.046 | +4.6% | 1.059 1.051 | 12.4% | 4.3% |
| star-join | 5.020 | 5.739 | 1.143 | +14.3% | 1.138 1.153 | 3.4% | 2.6% |
| star-lookup | 0.819 | 1.080 | 1.319 | +31.9% | 1.315 1.300 | 4.1% | 14.1% |
| subquery-agg | 0.859 | 0.986 | 1.149 | +14.9% | 1.113 1.205 | 3.9% | 17.8% |
| two-hop-count | 2.010 | 2.251 | 1.120 | +12.0% | 1.132 1.105 | 4.9% | 6.4% |
| types-grouped | 0.541 | 0.778 | 1.439 | +43.9% | 1.359 1.663 | 16.5% | 29.5% |
| values-star | 0.675 | 0.877 | 1.299 | +29.9% | 1.167 1.341 | 1.9% | 24.3% |
| **geometric mean** | | | 1.188 | +18.8% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
