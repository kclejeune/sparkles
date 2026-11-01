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
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners on, execution eager,streaming
- A: /home/kclejeune/lanes/ws1a-gates-469-473/bin/sparkles-04d49b57
- B: /home/kclejeune/lanes/ws1a-gates-469-473/bin/sparkles-04d49b57
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
| class-predicate-list | 9.982 | 10.437 | 1.046 | +4.6% | 1.051 1.046 | 0.2% | 1.1% |
| class-predicates | 9.998 | 10.452 | 1.045 | +4.5% | 1.045 1.042 | 1.4% | 1.5% |
| contains | 23.825 | 24.100 | 1.012 | +1.2% | 1.011 1.012 | 0.1% | 0.5% |
| count-all | 0.006 | 0.006 | 1.101 | +10.1% | 1.194 1.017 | 13.4% | 13.4% |
| distinct-join | 39.055 | 38.578 | 0.988 | -1.2% | 0.987 0.989 | 0.2% | 0.4% |
| distinct-obj | 0.004 | 0.005 | 1.053 | +5.3% | 1.172 0.943 | 21.2% | 23.3% |
| employee-docs | 0.041 | 0.045 | 1.091 | +9.1% | 1.170 1.053 | 8.5% | 19.8% |
| exists-join | 8.573 | 8.587 | 1.002 | +0.2% | 1.002 0.996 | 1.3% | 1.4% |
| export-500k | 2.358 | 0.438 | 0.186 | -81.4% | 0.181 0.188 | 17.9% | 1.7% |
| expr-agg-arg | 29.354 | 22.898 | 0.780 | -22.0% | 0.780 0.780 | 0.8% | 0.3% |
| expr-bind-group | 5.583 | 3.288 | 0.589 | -41.1% | 0.588 0.593 | 0.9% | 1.0% |
| expr-order-key | 11.945 | 10.384 | 0.869 | -13.1% | 0.869 0.869 | 1.0% | 0.7% |
| group-avg | 47.198 | 39.198 | 0.831 | -16.9% | 0.832 0.828 | 0.7% | 0.8% |
| knows-reach | 139.139 | 137.255 | 0.986 | -1.4% | 0.986 0.987 | 0.2% | 0.7% |
| lang-filter | 15.871 | 15.814 | 0.996 | -0.4% | 0.997 0.996 | 0.2% | 0.1% |
| minus | 3.235 | 2.424 | 0.749 | -25.1% | 0.748 0.751 | 1.2% | 0.6% |
| not-exists | 9.511 | 8.580 | 0.902 | -9.8% | 0.917 0.896 | 2.3% | 3.5% |
| optional-chain | 27.872 | 23.022 | 0.826 | -17.4% | 0.833 0.825 | 3.1% | 5.6% |
| optional-count | 5.325 | 4.993 | 0.938 | -6.2% | 0.949 0.927 | 4.1% | 0.8% |
| order-by-full | 676.120 | 561.392 | 0.830 | -17.0% | 0.830 0.831 | 0.3% | 0.5% |
| path-plus | 0.008 | 0.008 | 1.069 | +6.9% | 1.052 1.098 | 12.1% | 10.4% |
| predicate-counts | 0.005 | 0.006 | 1.389 | +38.9% | 1.394 1.393 | 6.6% | 8.7% |
| predicate-subjects | 0.006 | 0.008 | 1.329 | +32.9% | 1.347 1.270 | 5.2% | 10.1% |
| range-topk | 5.819 | 4.992 | 0.858 | -14.2% | 0.876 0.856 | 1.1% | 4.0% |
| regex-iri | 35.995 | 36.653 | 1.018 | +1.8% | 1.018 1.019 | 0.2% | 0.3% |
| star-join | 13.588 | 14.283 | 1.051 | +5.1% | 1.056 1.046 | 0.9% | 0.8% |
| star-lookup | 0.052 | 0.053 | 1.017 | +1.7% | 1.008 1.013 | 3.5% | 5.2% |
| subquery-agg | 1.640 | 1.620 | 0.987 | -1.3% | 0.987 0.989 | 0.6% | 0.9% |
| two-hop-count | 27.732 | 27.769 | 1.001 | +0.1% | 1.002 1.000 | 0.3% | 0.4% |
| types-grouped | 0.007 | 0.013 | 1.768 | +76.8% | 1.720 1.832 | 11.9% | 4.7% |
| values-star | 0.015 | 0.020 | 1.344 | +34.4% | 1.286 1.391 | 6.7% | 2.4% |
| **geometric mean** | | | 0.942 | -5.8% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 11.283 | 11.744 | 1.041 | +4.1% | 1.043 1.029 | 3.6% | 0.6% |
| class-predicates | 11.348 | 11.791 | 1.039 | +3.9% | 1.047 1.023 | 3.3% | 1.6% |
| contains | 25.028 | 25.317 | 1.012 | +1.2% | 1.013 1.011 | 0.5% | 0.4% |
| count-all | 1.889 | 1.621 | 0.858 | -14.2% | 0.829 1.053 | 35.4% | 64.9% |
| distinct-join | 44.484 | 46.126 | 1.037 | +3.7% | 1.036 1.038 | 0.8% | 0.2% |
| distinct-obj | 1.917 | 1.945 | 1.015 | +1.5% | 0.986 1.202 | 32.2% | 32.0% |
| employee-docs | 1.981 | 1.982 | 1.001 | +0.1% | 1.001 1.001 | 0.6% | 0.0% |
| exists-join | 9.921 | 9.924 | 1.000 | +0.0% | 1.000 0.993 | 1.7% | 1.5% |
| export-500k | 271.946 | 369.978 | 1.360 | +36.0% | 1.360 1.368 | 0.9% | 0.4% |
| expr-agg-arg | 30.704 | 24.008 | 0.782 | -21.8% | 0.780 0.787 | 0.7% | 1.6% |
| expr-bind-group | 6.827 | 4.544 | 0.666 | -33.4% | 0.663 0.668 | 3.1% | 7.4% |
| expr-order-key | 13.937 | 11.621 | 0.834 | -16.6% | 0.832 0.835 | 0.8% | 0.5% |
| group-avg | 48.752 | 40.724 | 0.835 | -16.5% | 0.836 0.834 | 0.7% | 0.6% |
| knows-reach | 140.482 | 138.602 | 0.987 | -1.3% | 0.988 0.986 | 0.2% | 0.7% |
| lang-filter | 17.095 | 17.051 | 0.997 | -0.3% | 0.997 0.998 | 0.5% | 0.2% |
| minus | 4.491 | 3.681 | 0.820 | -18.0% | 0.834 0.791 | 9.3% | 11.4% |
| not-exists | 10.745 | 9.911 | 0.922 | -7.8% | 0.923 0.922 | 2.4% | 2.9% |
| optional-chain | 145.118 | 218.977 | 1.509 | +50.9% | 1.515 1.509 | 0.5% | 1.0% |
| optional-count | 6.579 | 6.267 | 0.953 | -4.7% | 0.958 0.947 | 3.1% | 2.2% |
| order-by-full | 1036.165 | 1115.970 | 1.077 | +7.7% | 1.077 1.077 | 0.4% | 0.4% |
| path-plus | 1.969 | 1.970 | 1.001 | +0.1% | 1.000 1.005 | 34.1% | 16.2% |
| predicate-counts | 1.970 | 1.981 | 1.006 | +0.6% | 1.008 1.005 | 33.5% | 1.8% |
| predicate-subjects | 1.937 | 1.981 | 1.023 | +2.3% | 1.014 1.342 | 94.2% | 0.9% |
| range-topk | 7.174 | 6.369 | 0.888 | -11.2% | 0.923 0.881 | 2.0% | 6.2% |
| regex-iri | 37.239 | 37.963 | 1.019 | +1.9% | 1.019 1.018 | 0.5% | 0.3% |
| star-join | 36.090 | 52.075 | 1.443 | +44.3% | 1.445 1.424 | 1.5% | 1.0% |
| star-lookup | 1.983 | 1.985 | 1.001 | +0.1% | 1.002 0.990 | 2.5% | 0.3% |
| subquery-agg | 2.838 | 2.888 | 1.018 | +1.8% | 1.015 1.027 | 1.2% | 0.3% |
| two-hop-count | 29.005 | 29.133 | 1.004 | +0.4% | 1.003 1.005 | 0.6% | 0.8% |
| types-grouped | 1.948 | 1.975 | 1.014 | +1.4% | 1.015 1.016 | 58.7% | 0.2% |
| values-star | 1.978 | 1.978 | 1.000 | +0.0% | 1.001 0.999 | 1.3% | 0.0% |
| **geometric mean** | | | 0.992 | -0.8% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
