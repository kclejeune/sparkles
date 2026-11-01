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
| class-predicate-list | 10.189 | 10.706 | 1.051 | +5.1% | 1.050 1.046 | 3.0% | 1.5% |
| class-predicates | 10.235 | 10.796 | 1.055 | +5.5% | 1.057 1.052 | 1.5% | 2.0% |
| contains | 3.991 | 4.127 | 1.034 | +3.4% | 1.033 1.041 | 4.8% | 1.3% |
| count-all | 0.008 | 0.011 | 1.371 | +37.1% | 1.472 1.566 | 37.4% | 31.9% |
| distinct-join | 39.181 | 38.912 | 0.993 | -0.7% | 0.991 0.995 | 1.1% | 3.8% |
| distinct-obj | 0.005 | 0.008 | 1.529 | +52.9% | 1.440 1.749 | 24.9% | 49.0% |
| employee-docs | 0.058 | 0.074 | 1.275 | +27.5% | 1.327 1.231 | 8.4% | 18.2% |
| exists-join | 8.983 | 8.941 | 0.995 | -0.5% | 0.995 0.994 | 10.2% | 2.3% |
| export-500k | 2.364 | 0.414 | 0.175 | -82.5% | 0.167 0.180 | 40.5% | 1.7% |
| expr-agg-arg | 28.400 | 23.143 | 0.815 | -18.5% | 0.807 0.829 | 2.2% | 1.9% |
| expr-bind-group | 4.732 | 3.413 | 0.721 | -27.9% | 0.725 0.720 | 3.1% | 1.4% |
| expr-order-key | 10.243 | 10.840 | 1.058 | +5.8% | 1.044 1.081 | 4.7% | 1.7% |
| group-avg | 48.110 | 40.365 | 0.839 | -16.1% | 0.833 0.842 | 3.6% | 0.5% |
| knows-reach | 141.045 | 139.427 | 0.989 | -1.1% | 0.991 0.985 | 0.5% | 0.7% |
| lang-filter | 2.827 | 2.924 | 1.034 | +3.4% | 1.012 1.050 | 3.7% | 4.4% |
| minus | 3.277 | 2.580 | 0.787 | -21.3% | 0.775 0.796 | 4.7% | 3.6% |
| not-exists | 9.689 | 9.085 | 0.938 | -6.2% | 0.898 0.957 | 7.0% | 3.6% |
| optional-chain | 27.437 | 24.907 | 0.908 | -9.2% | 0.927 0.884 | 8.7% | 4.0% |
| optional-count | 5.374 | 5.201 | 0.968 | -3.2% | 0.970 0.967 | 3.6% | 0.7% |
| order-by-full | 181.829 | 158.978 | 0.874 | -12.6% | 0.875 0.874 | 1.7% | 3.2% |
| path-plus | 0.013 | 0.015 | 1.158 | +15.8% | 1.281 1.109 | 6.1% | 23.1% |
| predicate-counts | 0.007 | 0.010 | 1.578 | +57.8% | 1.610 1.503 | 52.2% | 8.8% |
| predicate-subjects | 0.010 | 0.015 | 1.597 | +59.7% | 1.540 1.996 | 7.0% | 38.9% |
| range-topk | 2.275 | 2.240 | 0.985 | -1.5% | 0.948 1.039 | 6.7% | 20.5% |
| regex-iri | 5.907 | 6.245 | 1.057 | +5.7% | 1.062 1.058 | 3.1% | 2.1% |
| star-join | 12.776 | 13.463 | 1.054 | +5.4% | 1.041 1.065 | 2.7% | 2.6% |
| star-lookup | 0.066 | 0.073 | 1.095 | +9.5% | 1.135 1.083 | 10.9% | 11.2% |
| subquery-agg | 0.713 | 0.806 | 1.130 | +13.0% | 1.192 1.114 | 17.1% | 4.4% |
| two-hop-count | 11.241 | 11.270 | 1.003 | +0.3% | 0.992 1.011 | 2.2% | 1.7% |
| types-grouped | 0.010 | 0.019 | 1.880 | +88.0% | 1.829 1.941 | 14.1% | 28.5% |
| values-star | 0.019 | 0.028 | 1.438 | +43.8% | 1.957 1.275 | 5.1% | 55.4% |
| **geometric mean** | | | 1.018 | +1.8% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 11.258 | 11.790 | 1.047 | +4.7% | 1.054 1.050 | 3.1% | 1.2% |
| class-predicates | 11.277 | 11.950 | 1.060 | +6.0% | 1.049 1.069 | 2.5% | 2.9% |
| contains | 4.786 | 5.157 | 1.077 | +7.7% | 1.080 1.070 | 3.4% | 1.5% |
| count-all | 0.470 | 0.751 | 1.600 | +60.0% | 1.573 1.607 | 23.9% | 21.4% |
| distinct-join | 44.566 | 45.123 | 1.012 | +1.2% | 1.008 1.017 | 1.6% | 3.9% |
| distinct-obj | 0.476 | 0.750 | 1.576 | +57.6% | 1.569 1.595 | 18.4% | 7.9% |
| employee-docs | 0.694 | 0.995 | 1.433 | +43.3% | 1.497 1.425 | 4.9% | 14.1% |
| exists-join | 9.947 | 10.058 | 1.011 | +1.1% | 1.014 1.011 | 10.8% | 2.2% |
| export-500k | 254.480 | 294.608 | 1.158 | +15.8% | 1.161 1.157 | 0.9% | 0.4% |
| expr-agg-arg | 29.543 | 24.390 | 0.826 | -17.4% | 0.816 0.835 | 2.4% | 2.2% |
| expr-bind-group | 5.535 | 4.517 | 0.816 | -18.4% | 0.818 0.814 | 3.9% | 1.3% |
| expr-order-key | 12.108 | 11.851 | 0.979 | -2.1% | 0.968 1.007 | 6.1% | 2.0% |
| group-avg | 49.620 | 41.776 | 0.842 | -15.8% | 0.836 0.847 | 3.6% | 0.4% |
| knows-reach | 142.282 | 140.788 | 0.990 | -1.0% | 0.992 0.985 | 0.5% | 0.6% |
| lang-filter | 3.614 | 3.938 | 1.090 | +9.0% | 1.062 1.105 | 1.2% | 3.9% |
| minus | 4.083 | 3.593 | 0.880 | -12.0% | 0.869 0.889 | 2.5% | 3.5% |
| not-exists | 10.643 | 10.152 | 0.954 | -4.6% | 0.915 0.972 | 8.8% | 3.5% |
| optional-chain | 140.377 | 169.669 | 1.209 | +20.9% | 1.220 1.198 | 3.2% | 1.7% |
| optional-count | 6.223 | 6.245 | 1.004 | +0.4% | 1.008 0.999 | 2.4% | 2.5% |
| order-by-full | 518.274 | 557.496 | 1.076 | +7.6% | 1.076 1.075 | 0.6% | 0.6% |
| path-plus | 0.482 | 0.796 | 1.652 | +65.2% | 1.738 1.574 | 14.2% | 20.3% |
| predicate-counts | 0.494 | 0.807 | 1.634 | +63.4% | 1.644 1.636 | 28.1% | 15.1% |
| predicate-subjects | 0.467 | 0.796 | 1.704 | +70.4% | 1.626 1.799 | 8.7% | 13.6% |
| range-topk | 3.146 | 3.446 | 1.096 | +9.6% | 1.073 1.093 | 6.8% | 13.8% |
| regex-iri | 6.743 | 7.309 | 1.084 | +8.4% | 1.093 1.080 | 3.2% | 2.0% |
| star-join | 33.566 | 43.495 | 1.296 | +29.6% | 1.290 1.306 | 1.4% | 1.4% |
| star-lookup | 0.782 | 1.031 | 1.319 | +31.9% | 1.290 1.361 | 1.4% | 8.2% |
| subquery-agg | 1.458 | 1.967 | 1.349 | +34.9% | 1.362 1.403 | 9.0% | 13.2% |
| two-hop-count | 12.367 | 12.451 | 1.007 | +0.7% | 1.003 1.010 | 2.4% | 1.9% |
| types-grouped | 0.571 | 0.806 | 1.411 | +41.1% | 1.386 1.466 | 34.2% | 10.5% |
| values-star | 0.663 | 0.887 | 1.338 | +33.8% | 1.418 1.316 | 1.4% | 21.3% |
| **geometric mean** | | | 1.152 | +15.2% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
