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
| class-predicate-list | 0.973 | 1.005 | 1.033 | +3.3% | 1.023 1.047 | 2.3% | 3.2% |
| class-predicates | 0.981 | 1.026 | 1.045 | +4.5% | 1.031 1.050 | 4.0% | 3.4% |
| contains | 0.718 | 0.750 | 1.045 | +4.5% | 1.017 1.151 | 13.7% | 6.7% |
| count-all | 0.007 | 0.010 | 1.422 | +42.2% | 1.523 1.660 | 15.5% | 46.3% |
| distinct-join | 3.529 | 3.876 | 1.098 | +9.8% | 1.094 1.101 | 2.0% | 1.2% |
| distinct-obj | 0.006 | 0.009 | 1.508 | +50.8% | 1.461 1.524 | 15.6% | 17.7% |
| employee-docs | 0.049 | 0.057 | 1.147 | +14.7% | 1.179 1.140 | 5.4% | 8.0% |
| exists-join | 1.091 | 1.135 | 1.040 | +4.0% | 1.026 1.043 | 1.9% | 2.5% |
| export-500k | 2.423 | 0.408 | 0.168 | -83.2% | 0.163 0.169 | 4.3% | 1.8% |
| expr-agg-arg | 3.002 | 2.010 | 0.670 | -33.0% | 0.670 0.668 | 5.9% | 2.1% |
| expr-bind-group | 0.855 | 0.393 | 0.459 | -54.1% | 0.425 0.499 | 18.5% | 1.3% |
| expr-order-key | 1.289 | 1.208 | 0.937 | -6.3% | 0.926 0.941 | 12.1% | 2.2% |
| group-avg | 4.480 | 3.591 | 0.801 | -19.9% | 0.808 0.800 | 5.8% | 1.3% |
| knows-reach | 11.838 | 11.688 | 0.987 | -1.3% | 0.985 0.989 | 1.8% | 1.6% |
| lang-filter | 0.705 | 0.748 | 1.061 | +6.1% | 1.078 1.026 | 9.7% | 27.1% |
| minus | 0.352 | 0.281 | 0.799 | -20.1% | 0.773 0.811 | 5.5% | 3.9% |
| not-exists | 0.865 | 0.799 | 0.923 | -7.7% | 0.928 0.885 | 6.1% | 0.6% |
| optional-chain | 2.136 | 2.078 | 0.973 | -2.7% | 0.953 0.998 | 1.9% | 6.4% |
| optional-count | 0.542 | 0.556 | 1.024 | +2.4% | 1.014 1.023 | 7.1% | 4.1% |
| order-by-full | 14.096 | 15.752 | 1.118 | +11.8% | 1.023 1.227 | 8.7% | 22.4% |
| path-plus | 0.018 | 0.022 | 1.246 | +24.6% | 1.615 1.124 | 4.7% | 56.0% |
| predicate-counts | 0.007 | 0.011 | 1.606 | +60.6% | 1.593 1.608 | 26.1% | 64.3% |
| predicate-subjects | 0.010 | 0.015 | 1.625 | +62.5% | 1.598 1.667 | 43.1% | 42.0% |
| range-topk | 0.713 | 0.569 | 0.798 | -20.2% | 0.790 0.817 | 5.1% | 4.0% |
| regex-iri | 0.922 | 0.997 | 1.081 | +8.1% | 1.054 1.133 | 12.8% | 4.5% |
| star-join | 1.541 | 1.651 | 1.071 | +7.1% | 1.074 1.050 | 7.6% | 6.7% |
| star-lookup | 0.084 | 0.089 | 1.059 | +5.9% | 1.076 1.046 | 7.2% | 9.7% |
| subquery-agg | 0.239 | 0.222 | 0.931 | -6.9% | 0.942 0.942 | 21.3% | 29.2% |
| two-hop-count | 1.268 | 1.249 | 0.985 | -1.5% | 0.944 1.009 | 13.9% | 6.0% |
| types-grouped | 0.011 | 0.019 | 1.686 | +68.6% | 2.013 1.675 | 8.8% | 78.6% |
| values-star | 0.020 | 0.026 | 1.283 | +28.3% | 1.267 1.332 | 10.1% | 27.8% |
| **geometric mean** | | | 0.987 | -1.3% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 1.619 | 1.985 | 1.226 | +22.6% | 1.186 1.249 | 1.9% | 4.7% |
| class-predicates | 1.676 | 1.989 | 1.186 | +18.6% | 1.158 1.240 | 3.0% | 5.7% |
| contains | 1.419 | 1.714 | 1.208 | +20.8% | 1.165 1.282 | 8.6% | 10.6% |
| count-all | 0.498 | 0.841 | 1.688 | +68.8% | 1.705 1.707 | 12.5% | 12.6% |
| distinct-join | 4.742 | 5.687 | 1.199 | +19.9% | 1.246 1.177 | 2.7% | 8.1% |
| distinct-obj | 0.505 | 0.820 | 1.624 | +62.4% | 1.627 1.633 | 8.0% | 12.8% |
| employee-docs | 0.676 | 0.984 | 1.456 | +45.6% | 1.470 1.510 | 5.5% | 11.9% |
| exists-join | 1.841 | 2.205 | 1.198 | +19.8% | 1.175 1.223 | 1.6% | 4.1% |
| export-500k | 220.028 | 295.875 | 1.345 | +34.5% | 1.341 1.346 | 1.2% | 1.0% |
| expr-agg-arg | 3.844 | 3.111 | 0.809 | -19.1% | 0.819 0.809 | 4.8% | 5.1% |
| expr-bind-group | 1.560 | 1.389 | 0.890 | -11.0% | 0.811 0.940 | 11.5% | 9.7% |
| expr-order-key | 2.057 | 2.177 | 1.058 | +5.8% | 1.061 1.060 | 5.8% | 3.4% |
| group-avg | 5.314 | 4.729 | 0.890 | -11.0% | 0.898 0.884 | 3.6% | 2.3% |
| knows-reach | 12.941 | 12.733 | 0.984 | -1.6% | 0.986 0.982 | 1.6% | 1.0% |
| lang-filter | 1.409 | 1.664 | 1.181 | +18.1% | 1.143 1.210 | 10.3% | 19.5% |
| minus | 1.010 | 1.224 | 1.211 | +21.1% | 1.120 1.276 | 5.9% | 14.2% |
| not-exists | 1.577 | 1.679 | 1.065 | +6.5% | 1.062 1.110 | 4.7% | 10.2% |
| optional-chain | 13.739 | 18.934 | 1.378 | +37.8% | 1.344 1.397 | 5.2% | 9.8% |
| optional-count | 1.260 | 1.549 | 1.229 | +22.9% | 1.208 1.260 | 6.2% | 4.8% |
| order-by-full | 46.047 | 58.524 | 1.271 | +27.1% | 1.223 1.316 | 5.6% | 6.2% |
| path-plus | 0.534 | 0.772 | 1.446 | +44.6% | 1.454 1.443 | 4.8% | 20.7% |
| predicate-counts | 0.528 | 0.847 | 1.605 | +60.5% | 1.574 1.669 | 12.9% | 3.9% |
| predicate-subjects | 0.486 | 0.810 | 1.666 | +66.6% | 1.509 1.864 | 26.2% | 20.8% |
| range-topk | 1.501 | 1.664 | 1.109 | +10.9% | 1.082 1.117 | 4.1% | 1.6% |
| regex-iri | 1.671 | 1.979 | 1.184 | +18.4% | 1.169 1.245 | 8.0% | 11.6% |
| star-join | 4.761 | 6.753 | 1.419 | +41.9% | 1.424 1.390 | 6.3% | 2.0% |
| star-lookup | 0.798 | 1.155 | 1.447 | +44.7% | 1.364 1.484 | 7.0% | 21.9% |
| subquery-agg | 0.849 | 1.104 | 1.300 | +30.0% | 1.267 1.305 | 13.0% | 8.4% |
| two-hop-count | 2.039 | 2.251 | 1.104 | +10.4% | 1.041 1.168 | 11.9% | 9.0% |
| types-grouped | 0.578 | 0.830 | 1.436 | +43.6% | 1.427 1.444 | 3.6% | 24.4% |
| values-star | 0.688 | 0.871 | 1.265 | +26.5% | 1.278 1.285 | 3.8% | 10.1% |
| **geometric mean** | | | 1.241 | +24.1% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
