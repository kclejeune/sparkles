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
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners on, execution eager,streaming
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
| class-predicate-list | 0.929 | 0.917 | 0.987 | -1.3% | 0.985 0.990 | 1.2% | 1.0% |
| class-predicates | 0.932 | 0.929 | 0.997 | -0.3% | 0.996 1.003 | 0.5% | 1.1% |
| contains | 2.410 | 2.473 | 1.026 | +2.6% | 1.026 1.027 | 0.5% | 0.5% |
| count-all | 0.005 | 0.006 | 1.178 | +17.8% | 1.177 1.177 | 7.6% | 7.0% |
| distinct-join | 3.488 | 3.643 | 1.044 | +4.4% | 1.035 1.050 | 3.4% | 0.7% |
| distinct-obj | 0.004 | 0.005 | 1.361 | +36.1% | 1.347 1.328 | 8.7% | 8.3% |
| employee-docs | 0.036 | 0.040 | 1.121 | +12.1% | 1.104 1.122 | 10.5% | 17.3% |
| exists-join | 1.025 | 1.039 | 1.013 | +1.3% | 1.002 1.021 | 3.2% | 0.9% |
| export-500k | 2.247 | 0.428 | 0.191 | -80.9% | 0.180 0.198 | 16.7% | 1.4% |
| expr-agg-arg | 2.548 | 1.899 | 0.745 | -25.5% | 0.746 0.746 | 1.3% | 0.7% |
| expr-bind-group | 0.593 | 0.369 | 0.622 | -37.8% | 0.622 0.618 | 2.5% | 0.2% |
| expr-order-key | 1.184 | 1.111 | 0.938 | -6.2% | 0.925 0.948 | 4.0% | 1.9% |
| group-avg | 4.012 | 3.446 | 0.859 | -14.1% | 0.857 0.861 | 2.7% | 0.5% |
| knows-reach | 11.531 | 11.361 | 0.985 | -1.5% | 0.988 0.982 | 0.3% | 1.0% |
| lang-filter | 1.606 | 1.627 | 1.013 | +1.3% | 1.012 1.013 | 0.8% | 0.2% |
| minus | 0.303 | 0.259 | 0.852 | -14.8% | 0.838 0.859 | 5.4% | 1.1% |
| not-exists | 0.799 | 0.750 | 0.938 | -6.2% | 0.929 0.944 | 2.5% | 0.7% |
| optional-chain | 1.991 | 1.969 | 0.989 | -1.1% | 0.998 0.975 | 1.5% | 2.2% |
| optional-count | 0.488 | 0.519 | 1.063 | +6.3% | 1.060 1.064 | 2.6% | 1.0% |
| order-by-full | 48.885 | 45.132 | 0.923 | -7.7% | 0.924 0.915 | 3.1% | 1.1% |
| path-plus | 0.011 | 0.012 | 1.112 | +11.2% | 1.072 1.163 | 13.8% | 8.0% |
| predicate-counts | 0.005 | 0.006 | 1.336 | +33.6% | 1.358 1.320 | 2.7% | 10.4% |
| predicate-subjects | 0.007 | 0.008 | 1.196 | +19.6% | 1.183 1.237 | 7.7% | 3.0% |
| range-topk | 0.664 | 0.528 | 0.796 | -20.4% | 0.795 0.796 | 0.6% | 0.5% |
| regex-iri | 3.543 | 3.678 | 1.038 | +3.8% | 1.034 1.039 | 1.3% | 0.3% |
| star-join | 1.383 | 1.423 | 1.029 | +2.9% | 1.026 1.031 | 0.6% | 0.6% |
| star-lookup | 0.061 | 0.066 | 1.082 | +8.2% | 1.064 1.098 | 4.4% | 10.9% |
| subquery-agg | 0.188 | 0.183 | 0.974 | -2.6% | 0.998 0.966 | 7.4% | 1.5% |
| two-hop-count | 2.620 | 2.615 | 0.998 | -0.2% | 1.000 0.997 | 0.9% | 0.4% |
| types-grouped | 0.007 | 0.012 | 1.685 | +68.5% | 1.674 1.649 | 10.6% | 4.5% |
| values-star | 0.015 | 0.019 | 1.280 | +28.0% | 1.271 1.313 | 6.0% | 3.9% |
| **geometric mean** | | | 0.968 | -3.2% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 2.795 | 2.794 | 1.000 | -0.0% | 1.000 1.001 | 9.1% | 9.4% |
| class-predicates | 2.792 | 2.796 | 1.001 | +0.1% | 1.164 1.000 | 38.9% | 0.1% |
| contains | 3.563 | 3.688 | 1.035 | +3.5% | 1.025 1.121 | 3.8% | 8.2% |
| count-all | 1.951 | 1.950 | 0.999 | -0.1% | 0.977 1.020 | 33.9% | 102.3% |
| distinct-join | 5.051 | 5.542 | 1.097 | +9.7% | 1.095 1.097 | 2.8% | 3.6% |
| distinct-obj | 1.968 | 1.883 | 0.957 | -4.3% | 0.493 1.289 | 33.9% | 101.1% |
| employee-docs | 1.978 | 1.980 | 1.001 | +0.1% | 1.000 1.001 | 0.2% | 1.3% |
| exists-join | 2.797 | 2.798 | 1.000 | +0.0% | 1.000 1.001 | 0.1% | 0.1% |
| export-500k | 234.044 | 364.969 | 1.559 | +55.9% | 1.560 1.552 | 0.6% | 0.4% |
| expr-agg-arg | 3.837 | 3.358 | 0.875 | -12.5% | 0.810 1.006 | 6.5% | 24.7% |
| expr-bind-group | 1.972 | 1.981 | 1.004 | +0.4% | 1.009 1.005 | 2.5% | 0.9% |
| expr-order-key | 2.797 | 2.798 | 1.000 | +0.0% | 1.001 1.000 | 0.0% | 0.1% |
| group-avg | 5.457 | 4.790 | 0.878 | -12.2% | 0.888 0.853 | 10.7% | 2.6% |
| knows-reach | 12.759 | 12.571 | 0.985 | -1.5% | 0.972 0.991 | 0.2% | 5.6% |
| lang-filter | 2.796 | 2.808 | 1.004 | +0.4% | 1.008 1.002 | 0.1% | 11.1% |
| minus | 1.971 | 1.972 | 1.000 | +0.0% | 1.001 1.001 | 0.2% | 1.3% |
| not-exists | 1.974 | 1.985 | 1.006 | +0.6% | 0.977 1.002 | 4.6% | 2.5% |
| optional-chain | 14.130 | 22.043 | 1.560 | +56.0% | 1.560 1.627 | 0.1% | 4.5% |
| optional-count | 1.971 | 1.971 | 1.000 | +0.0% | 1.000 1.000 | 0.0% | 1.1% |
| order-by-full | 84.019 | 101.418 | 1.207 | +20.7% | 1.209 1.213 | 1.8% | 2.5% |
| path-plus | 1.966 | 1.972 | 1.003 | +0.3% | 1.019 1.001 | 64.1% | 0.4% |
| predicate-counts | 1.930 | 1.978 | 1.024 | +2.4% | 1.198 1.023 | 34.5% | 12.4% |
| predicate-subjects | 1.937 | 1.976 | 1.020 | +2.0% | 1.001 1.853 | 97.4% | 35.6% |
| range-topk | 1.972 | 1.971 | 1.000 | -0.0% | 1.000 0.999 | 1.3% | 0.2% |
| regex-iri | 4.705 | 4.793 | 1.019 | +1.9% | 1.011 1.046 | 0.5% | 4.0% |
| star-join | 4.865 | 6.455 | 1.327 | +32.7% | 1.335 1.313 | 5.1% | 0.8% |
| star-lookup | 1.980 | 1.983 | 1.002 | +0.2% | 1.002 1.001 | 0.2% | 0.2% |
| subquery-agg | 1.969 | 1.971 | 1.001 | +0.1% | 1.002 1.001 | 0.1% | 0.1% |
| two-hop-count | 3.850 | 3.893 | 1.011 | +1.1% | 1.010 1.013 | 4.1% | 0.5% |
| types-grouped | 1.276 | 1.973 | 1.546 | +54.6% | 1.543 1.546 | 21.1% | 0.2% |
| values-star | 1.974 | 1.977 | 1.001 | +0.1% | 1.003 0.999 | 0.4% | 1.1% |
| **geometric mean** | | | 1.056 | +5.6% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
