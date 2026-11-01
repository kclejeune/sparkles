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
| class-predicate-list | 0.933 | 0.927 | 0.993 | -0.7% | 0.996 0.986 | 0.9% | 1.2% |
| class-predicates | 0.941 | 0.939 | 0.998 | -0.2% | 1.000 0.999 | 1.6% | 1.5% |
| contains | 2.444 | 2.467 | 1.009 | +0.9% | 1.010 1.009 | 0.3% | 0.4% |
| count-all | 0.004 | 0.005 | 1.275 | +27.5% | 1.217 1.307 | 10.9% | 15.7% |
| distinct-join | 3.472 | 3.683 | 1.061 | +6.1% | 1.056 1.066 | 1.4% | 0.1% |
| distinct-obj | 0.004 | 0.005 | 1.226 | +22.6% | 1.241 1.284 | 15.1% | 14.2% |
| employee-docs | 0.033 | 0.037 | 1.115 | +11.5% | 1.109 1.118 | 8.3% | 8.9% |
| exists-join | 1.023 | 1.024 | 1.002 | +0.2% | 0.999 1.011 | 2.0% | 0.6% |
| export-500k | 2.280 | 0.411 | 0.180 | -82.0% | 0.179 0.181 | 0.9% | 1.5% |
| expr-agg-arg | 2.634 | 1.903 | 0.723 | -27.7% | 0.724 0.719 | 3.0% | 0.4% |
| expr-bind-group | 0.588 | 0.367 | 0.624 | -37.6% | 0.625 0.623 | 0.8% | 0.9% |
| expr-order-key | 1.177 | 1.105 | 0.939 | -6.1% | 0.941 0.934 | 1.6% | 1.2% |
| group-avg | 4.080 | 3.465 | 0.849 | -15.1% | 0.851 0.844 | 2.4% | 1.0% |
| knows-reach | 11.541 | 11.367 | 0.985 | -1.5% | 0.987 0.984 | 0.9% | 1.2% |
| lang-filter | 1.624 | 1.618 | 0.996 | -0.4% | 0.996 0.998 | 0.8% | 0.3% |
| minus | 0.307 | 0.253 | 0.825 | -17.5% | 0.825 0.838 | 4.9% | 2.0% |
| not-exists | 0.793 | 0.727 | 0.917 | -8.3% | 0.915 0.920 | 0.7% | 0.5% |
| optional-chain | 1.980 | 1.959 | 0.989 | -1.1% | 0.992 0.992 | 3.4% | 2.2% |
| optional-count | 0.488 | 0.516 | 1.058 | +5.8% | 1.057 1.060 | 1.0% | 0.9% |
| order-by-full | 48.477 | 45.227 | 0.933 | -6.7% | 0.928 0.937 | 1.7% | 2.1% |
| path-plus | 0.010 | 0.012 | 1.136 | +13.6% | 1.109 1.179 | 10.5% | 2.1% |
| predicate-counts | 0.004 | 0.006 | 1.433 | +43.3% | 1.514 1.398 | 16.3% | 6.7% |
| predicate-subjects | 0.006 | 0.008 | 1.320 | +32.0% | 1.394 1.245 | 7.4% | 9.6% |
| range-topk | 0.659 | 0.527 | 0.800 | -20.0% | 0.798 0.801 | 0.4% | 0.6% |
| regex-iri | 3.572 | 3.665 | 1.026 | +2.6% | 1.026 1.026 | 0.9% | 0.2% |
| star-join | 1.352 | 1.392 | 1.030 | +3.0% | 1.027 1.033 | 2.7% | 2.1% |
| star-lookup | 0.059 | 0.062 | 1.047 | +4.7% | 1.058 1.038 | 2.2% | 7.9% |
| subquery-agg | 0.183 | 0.181 | 0.990 | -1.0% | 1.001 0.982 | 4.3% | 0.7% |
| two-hop-count | 2.625 | 2.615 | 0.996 | -0.4% | 0.999 0.995 | 0.8% | 0.1% |
| types-grouped | 0.007 | 0.013 | 1.817 | +81.7% | 1.789 1.806 | 7.1% | 7.0% |
| values-star | 0.015 | 0.019 | 1.272 | +27.2% | 1.257 1.310 | 11.2% | 4.6% |
| **geometric mean** | | | 0.969 | -3.1% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 2.795 | 2.796 | 1.001 | +0.1% | 1.001 0.862 | 15.9% | 16.2% |
| class-predicates | 2.796 | 2.798 | 1.001 | +0.1% | 1.001 1.000 | 0.2% | 0.1% |
| contains | 3.600 | 3.667 | 1.019 | +1.9% | 1.018 1.036 | 12.2% | 4.9% |
| count-all | 1.153 | 1.972 | 1.710 | +71.0% | 1.021 2.001 | 98.7% | 95.8% |
| distinct-join | 5.051 | 5.580 | 1.105 | +10.5% | 1.091 1.139 | 3.7% | 6.4% |
| distinct-obj | 1.891 | 1.969 | 1.041 | +4.1% | 0.765 1.936 | 102.4% | 34.0% |
| employee-docs | 1.981 | 1.981 | 1.000 | +0.0% | 1.002 0.999 | 1.1% | 0.2% |
| exists-join | 2.800 | 2.801 | 1.001 | +0.1% | 1.001 1.000 | 0.1% | 0.2% |
| export-500k | 264.968 | 370.170 | 1.397 | +39.7% | 1.402 1.398 | 0.7% | 0.4% |
| expr-agg-arg | 3.859 | 3.184 | 0.825 | -17.5% | 0.819 0.826 | 4.1% | 13.9% |
| expr-bind-group | 1.973 | 1.982 | 1.005 | +0.5% | 1.005 1.004 | 0.2% | 0.1% |
| expr-order-key | 2.802 | 2.801 | 1.000 | -0.0% | 0.999 0.999 | 0.1% | 0.1% |
| group-avg | 5.851 | 4.790 | 0.819 | -18.1% | 0.857 0.814 | 10.1% | 4.4% |
| knows-reach | 12.761 | 12.598 | 0.987 | -1.3% | 0.987 0.985 | 0.5% | 1.4% |
| lang-filter | 2.797 | 2.803 | 1.002 | +0.2% | 1.001 1.013 | 0.1% | 3.1% |
| minus | 1.973 | 1.973 | 1.000 | +0.0% | 1.000 1.001 | 0.2% | 0.1% |
| not-exists | 1.972 | 1.972 | 1.000 | +0.0% | 1.000 1.001 | 0.2% | 0.1% |
| optional-chain | 15.133 | 22.040 | 1.456 | +45.6% | 1.450 1.457 | 1.0% | 0.1% |
| optional-count | 1.973 | 1.973 | 1.000 | +0.0% | 1.000 1.000 | 0.1% | 0.4% |
| order-by-full | 85.541 | 101.487 | 1.186 | +18.6% | 1.186 1.187 | 1.1% | 1.0% |
| path-plus | 1.964 | 1.372 | 0.699 | -30.1% | 1.323 0.497 | 34.0% | 106.4% |
| predicate-counts | 1.302 | 1.982 | 1.522 | +52.2% | 1.047 1.750 | 71.8% | 0.9% |
| predicate-subjects | 1.972 | 1.642 | 0.833 | -16.7% | 0.850 0.760 | 2.2% | 101.5% |
| range-topk | 1.972 | 1.974 | 1.001 | +0.1% | 1.001 1.000 | 0.2% | 0.1% |
| regex-iri | 4.739 | 4.912 | 1.037 | +3.7% | 0.986 1.076 | 8.5% | 4.5% |
| star-join | 5.000 | 6.407 | 1.282 | +28.2% | 1.285 1.281 | 0.8% | 4.3% |
| star-lookup | 1.983 | 1.985 | 1.001 | +0.1% | 1.001 1.001 | 0.8% | 0.3% |
| subquery-agg | 1.972 | 1.973 | 1.001 | +0.1% | 1.001 1.001 | 0.1% | 0.1% |
| two-hop-count | 3.850 | 3.886 | 1.009 | +0.9% | 1.011 1.010 | 0.7% | 5.2% |
| types-grouped | 1.962 | 1.974 | 1.006 | +0.6% | 1.003 1.328 | 102.8% | 2.1% |
| values-star | 1.977 | 1.978 | 1.001 | +0.1% | 1.000 1.002 | 2.0% | 0.1% |
| **geometric mean** | | | 1.045 | +4.5% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
