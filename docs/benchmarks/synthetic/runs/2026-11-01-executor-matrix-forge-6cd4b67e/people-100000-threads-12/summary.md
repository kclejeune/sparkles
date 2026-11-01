# Interleaved A/B on Namespace

- binary: /home/kclejeune/lanes/ws1a-gates-469-473/bin/sparkles-04d49b57
- binary sha256: 34aa23f8e08949fd166863059128e1afa3108501fb171b65d75494b0ba09e0fd
- binary commit: 04d49b57
- version: sparkles 0.1.0
- compilers: GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- build config: {"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{"CARGO_INCREMENTAL":"0"},"mbx_policy":"d8bf0b48d620f6bf62929d3db32bd253653505cdb33a5bd1efed1e79aaca05b4","profile":"release"}
- runtime env: clean
- scripts commit: unknown
- scripts dirty: 0
- host: forge
- started: 2026-11-01T10:02:19Z
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=12, server CPU 0-11, client CPU 12, spinners off, execution eager,streaming
- A: /home/kclejeune/lanes/ws1a-gates-469-473/bin/sparkles-04d49b57
- B: /home/kclejeune/lanes/ws1a-gates-469-473/bin/sparkles-04d49b57
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
| class-predicate-list | 0.996 | 1.015 | 1.019 | +1.9% | 1.007 1.029 | 5.4% | 3.9% |
| class-predicates | 1.001 | 1.014 | 1.013 | +1.3% | 1.025 1.005 | 2.2% | 3.3% |
| contains | 0.752 | 0.721 | 0.959 | -4.1% | 0.980 0.977 | 9.6% | 26.2% |
| count-all | 0.007 | 0.009 | 1.317 | +31.7% | 0.895 1.355 | 7.9% | 170.9% |
| distinct-join | 3.595 | 3.905 | 1.086 | +8.6% | 1.079 1.071 | 1.9% | 3.4% |
| distinct-obj | 0.006 | 0.008 | 1.324 | +32.4% | 1.376 0.952 | 15.5% | 233.8% |
| employee-docs | 0.047 | 0.051 | 1.087 | +8.7% | 1.104 0.937 | 6.4% | 130.5% |
| exists-join | 1.088 | 1.112 | 1.021 | +2.1% | 1.018 1.015 | 0.8% | 3.1% |
| export-500k | 2.554 | 0.408 | 0.160 | -84.0% | 0.161 0.167 | 32.3% | 2.3% |
| expr-agg-arg | 2.999 | 1.990 | 0.664 | -33.6% | 0.649 0.675 | 2.3% | 7.7% |
| expr-bind-group | 0.752 | 0.397 | 0.528 | -47.2% | 0.537 0.529 | 12.7% | 9.7% |
| expr-order-key | 1.282 | 1.198 | 0.935 | -6.5% | 0.923 0.906 | 6.5% | 3.3% |
| group-avg | 4.346 | 3.612 | 0.831 | -16.9% | 0.837 0.815 | 4.4% | 3.8% |
| knows-reach | 11.824 | 11.588 | 0.980 | -2.0% | 0.978 0.978 | 1.9% | 1.8% |
| lang-filter | 0.655 | 0.725 | 1.108 | +10.8% | 1.165 1.080 | 11.7% | 15.5% |
| minus | 0.345 | 0.275 | 0.797 | -20.3% | 0.788 0.813 | 10.7% | 7.0% |
| not-exists | 0.878 | 0.783 | 0.892 | -10.8% | 0.885 0.924 | 4.8% | 1.3% |
| optional-chain | 2.100 | 2.064 | 0.983 | -1.7% | 0.953 0.995 | 17.4% | 1.6% |
| optional-count | 0.542 | 0.550 | 1.015 | +1.5% | 0.988 1.048 | 3.2% | 5.9% |
| order-by-full | 13.953 | 14.287 | 1.024 | +2.4% | 1.014 1.017 | 2.2% | 4.2% |
| path-plus | 0.017 | 0.022 | 1.258 | +25.8% | 1.267 1.284 | 10.1% | 15.2% |
| predicate-counts | 0.006 | 0.011 | 1.748 | +74.8% | 1.845 2.080 | 47.2% | 34.2% |
| predicate-subjects | 0.010 | 0.015 | 1.527 | +52.7% | 1.593 1.785 | 45.0% | 37.4% |
| range-topk | 0.719 | 0.558 | 0.776 | -22.4% | 0.789 0.754 | 6.4% | 2.4% |
| regex-iri | 0.922 | 0.993 | 1.076 | +7.6% | 1.063 1.081 | 7.7% | 10.3% |
| star-join | 1.566 | 1.604 | 1.024 | +2.4% | 1.024 1.019 | 3.3% | 3.6% |
| star-lookup | 0.081 | 0.090 | 1.107 | +10.7% | 1.096 1.105 | 7.6% | 5.9% |
| subquery-agg | 0.225 | 0.217 | 0.965 | -3.5% | 0.747 1.128 | 45.5% | 39.0% |
| two-hop-count | 1.235 | 1.316 | 1.066 | +6.6% | 1.069 1.071 | 10.2% | 9.6% |
| types-grouped | 0.011 | 0.018 | 1.750 | +75.0% | 1.771 1.752 | 7.3% | 8.6% |
| values-star | 0.020 | 0.025 | 1.254 | +25.4% | 1.220 1.282 | 5.3% | 17.0% |
| **geometric mean** | | | 0.979 | -2.1% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 1.670 | 1.970 | 1.180 | +18.0% | 1.169 1.193 | 2.2% | 6.0% |
| class-predicates | 1.690 | 1.978 | 1.171 | +17.1% | 1.189 1.146 | 2.7% | 7.4% |
| contains | 1.435 | 1.585 | 1.104 | +10.4% | 1.105 1.127 | 7.5% | 22.1% |
| count-all | 0.483 | 0.775 | 1.603 | +60.3% | 1.447 1.646 | 11.4% | 44.2% |
| distinct-join | 4.848 | 5.649 | 1.165 | +16.5% | 1.146 1.222 | 1.1% | 16.6% |
| distinct-obj | 0.496 | 0.767 | 1.545 | +54.5% | 1.605 1.416 | 9.7% | 45.8% |
| employee-docs | 0.658 | 0.909 | 1.381 | +38.1% | 1.472 1.242 | 6.6% | 35.2% |
| exists-join | 1.843 | 2.133 | 1.158 | +15.8% | 1.163 1.139 | 2.6% | 12.7% |
| export-500k | 246.856 | 298.730 | 1.210 | +21.0% | 1.213 1.207 | 0.8% | 0.7% |
| expr-agg-arg | 3.829 | 3.030 | 0.791 | -20.9% | 0.762 0.817 | 2.4% | 7.3% |
| expr-bind-group | 1.474 | 1.353 | 0.919 | -8.1% | 0.927 0.935 | 11.2% | 9.0% |
| expr-order-key | 2.034 | 2.175 | 1.069 | +6.9% | 1.052 1.069 | 3.9% | 6.4% |
| group-avg | 5.188 | 4.740 | 0.914 | -8.6% | 0.914 0.898 | 3.8% | 3.0% |
| knows-reach | 12.911 | 12.703 | 0.984 | -1.6% | 0.980 0.990 | 2.0% | 2.0% |
| lang-filter | 1.359 | 1.599 | 1.177 | +17.7% | 1.212 1.131 | 6.1% | 6.5% |
| minus | 0.978 | 1.155 | 1.181 | +18.1% | 1.148 1.219 | 4.0% | 12.3% |
| not-exists | 1.543 | 1.713 | 1.110 | +11.0% | 1.105 1.114 | 2.9% | 7.2% |
| optional-chain | 14.790 | 18.788 | 1.270 | +27.0% | 1.267 1.274 | 1.6% | 3.5% |
| optional-count | 1.240 | 1.426 | 1.150 | +15.0% | 1.207 1.126 | 3.1% | 14.2% |
| order-by-full | 48.822 | 57.093 | 1.169 | +16.9% | 1.160 1.184 | 2.3% | 0.7% |
| path-plus | 0.510 | 0.822 | 1.613 | +61.3% | 1.620 1.598 | 9.4% | 22.1% |
| predicate-counts | 0.480 | 0.849 | 1.766 | +76.6% | 1.696 1.778 | 18.9% | 27.6% |
| predicate-subjects | 0.465 | 0.835 | 1.796 | +79.6% | 1.690 1.970 | 24.4% | 7.7% |
| range-topk | 1.512 | 1.637 | 1.083 | +8.3% | 1.072 1.096 | 0.7% | 4.1% |
| regex-iri | 1.648 | 2.026 | 1.229 | +22.9% | 1.198 1.251 | 4.8% | 7.4% |
| star-join | 4.955 | 6.658 | 1.344 | +34.4% | 1.332 1.355 | 2.8% | 1.8% |
| star-lookup | 0.817 | 1.087 | 1.330 | +33.0% | 1.335 1.309 | 5.2% | 10.8% |
| subquery-agg | 0.840 | 1.064 | 1.267 | +26.7% | 1.174 1.347 | 15.5% | 17.3% |
| two-hop-count | 2.029 | 2.372 | 1.170 | +17.0% | 1.182 1.158 | 7.4% | 4.5% |
| types-grouped | 0.558 | 0.821 | 1.470 | +47.0% | 1.482 1.475 | 8.5% | 13.9% |
| values-star | 0.676 | 0.944 | 1.396 | +39.6% | 1.369 1.413 | 3.7% | 14.3% |
| **geometric mean** | | | 1.228 | +22.8% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
