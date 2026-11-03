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
| class-predicate-list | 9.832 | 10.273 | 1.045 | +4.5% | 1.050 1.044 | 0.8% | 2.1% |
| class-predicates | 9.848 | 10.245 | 1.040 | +4.0% | 1.042 1.042 | 0.9% | 2.3% |
| contains | 23.466 | 24.045 | 1.025 | +2.5% | 1.024 1.025 | 0.4% | 0.3% |
| count-all | 0.006 | 0.007 | 1.180 | +18.0% | 1.191 1.164 | 2.6% | 14.4% |
| distinct-join | 38.249 | 37.922 | 0.991 | -0.9% | 0.992 0.993 | 0.6% | 0.5% |
| distinct-obj | 0.004 | 0.005 | 1.289 | +28.9% | 1.313 1.245 | 10.9% | 14.0% |
| employee-docs | 0.048 | 0.054 | 1.109 | +10.9% | 1.144 1.059 | 7.5% | 14.7% |
| exists-join | 8.564 | 8.560 | 1.000 | -0.0% | 0.998 1.001 | 1.8% | 2.7% |
| export-500k | 2.218 | 0.442 | 0.200 | -80.0% | 0.201 0.195 | 9.9% | 3.2% |
| expr-agg-arg | 29.446 | 23.001 | 0.781 | -21.9% | 0.703 0.792 | 36.9% | 1.1% |
| expr-bind-group | 5.570 | 3.299 | 0.592 | -40.8% | 0.499 0.594 | 29.1% | 0.4% |
| expr-order-key | 11.881 | 10.535 | 0.887 | -11.3% | 0.812 0.881 | 39.4% | 2.8% |
| group-avg | 47.515 | 39.342 | 0.828 | -17.2% | 0.733 0.834 | 32.5% | 0.8% |
| knows-reach | 141.565 | 137.470 | 0.971 | -2.9% | 0.799 0.989 | 42.7% | 1.8% |
| lang-filter | 15.654 | 15.909 | 1.016 | +1.6% | 0.997 1.019 | 11.5% | 0.1% |
| minus | 3.171 | 2.438 | 0.769 | -23.1% | 0.679 0.777 | 24.4% | 0.8% |
| not-exists | 9.770 | 8.869 | 0.908 | -9.2% | 0.881 0.922 | 14.3% | 4.4% |
| optional-chain | 29.844 | 22.853 | 0.766 | -23.4% | 0.603 0.825 | 48.5% | 3.5% |
| optional-count | 5.323 | 5.016 | 0.942 | -5.8% | 0.890 0.944 | 18.4% | 0.3% |
| order-by-full | 677.315 | 596.671 | 0.881 | -11.9% | 0.830 0.831 | 32.0% | 26.4% |
| path-plus | 0.009 | 0.009 | 1.033 | +3.3% | 1.127 0.991 | 5.9% | 26.0% |
| predicate-counts | 0.005 | 0.006 | 1.366 | +36.6% | 1.374 1.358 | 22.3% | 18.9% |
| predicate-subjects | 0.006 | 0.009 | 1.354 | +35.4% | 1.371 1.353 | 15.0% | 14.6% |
| range-topk | 5.851 | 5.095 | 0.871 | -12.9% | 0.873 0.867 | 1.1% | 1.9% |
| regex-iri | 35.546 | 36.740 | 1.034 | +3.4% | 1.031 1.035 | 0.7% | 0.2% |
| star-join | 13.891 | 14.512 | 1.045 | +4.5% | 1.025 1.065 | 4.1% | 3.3% |
| star-lookup | 0.056 | 0.060 | 1.088 | +8.8% | 1.079 1.055 | 5.9% | 10.2% |
| subquery-agg | 1.653 | 1.639 | 0.991 | -0.9% | 0.990 0.991 | 1.3% | 1.6% |
| two-hop-count | 27.661 | 27.732 | 1.003 | +0.3% | 1.004 1.002 | 0.1% | 0.4% |
| types-grouped | 0.007 | 0.013 | 1.820 | +82.0% | 1.944 1.702 | 9.5% | 5.8% |
| values-star | 0.017 | 0.020 | 1.165 | +16.5% | 1.236 1.121 | 11.5% | 12.9% |
| **geometric mean** | | | 0.953 | -4.7% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 11.140 | 11.587 | 1.040 | +4.0% | 1.039 1.040 | 3.6% | 2.1% |
| class-predicates | 11.153 | 11.666 | 1.046 | +4.6% | 1.037 1.049 | 7.5% | 1.7% |
| contains | 24.556 | 25.316 | 1.031 | +3.1% | 1.031 1.029 | 1.0% | 0.2% |
| count-all | 1.031 | 1.970 | 1.910 | +91.0% | 2.006 1.645 | 63.9% | 2.4% |
| distinct-join | 43.444 | 45.758 | 1.053 | +5.3% | 1.052 1.058 | 0.9% | 0.5% |
| distinct-obj | 1.879 | 1.899 | 1.011 | +1.1% | 0.514 1.221 | 102.8% | 111.6% |
| employee-docs | 1.979 | 1.980 | 1.001 | +0.1% | 1.001 1.000 | 0.2% | 0.2% |
| exists-join | 9.919 | 9.954 | 1.003 | +0.3% | 1.010 1.000 | 0.7% | 2.4% |
| export-500k | 241.608 | 365.974 | 1.515 | +51.5% | 1.515 1.515 | 0.4% | 0.3% |
| expr-agg-arg | 30.915 | 24.308 | 0.786 | -21.4% | 0.709 0.803 | 35.1% | 1.8% |
| expr-bind-group | 6.847 | 4.590 | 0.670 | -33.0% | 0.561 0.672 | 39.0% | 5.8% |
| expr-order-key | 13.933 | 11.841 | 0.850 | -15.0% | 0.783 0.843 | 34.7% | 2.0% |
| group-avg | 49.127 | 40.904 | 0.833 | -16.7% | 0.743 0.837 | 35.0% | 0.8% |
| knows-reach | 142.966 | 138.848 | 0.971 | -2.9% | 0.799 0.990 | 43.1% | 1.8% |
| lang-filter | 16.900 | 17.176 | 1.016 | +1.6% | 0.995 1.021 | 12.0% | 0.2% |
| minus | 4.665 | 3.595 | 0.771 | -22.9% | 0.724 0.841 | 19.3% | 4.6% |
| not-exists | 10.991 | 10.060 | 0.915 | -8.5% | 0.847 0.927 | 14.6% | 4.8% |
| optional-chain | 135.058 | 217.966 | 1.614 | +61.4% | 1.472 1.626 | 13.1% | 0.5% |
| optional-count | 6.762 | 6.284 | 0.929 | -7.1% | 0.894 0.960 | 17.0% | 10.9% |
| order-by-full | 1010.510 | 1144.985 | 1.133 | +13.3% | 1.047 1.099 | 21.9% | 13.5% |
| path-plus | 1.970 | 1.971 | 1.001 | +0.1% | 1.002 1.001 | 32.3% | 25.7% |
| predicate-counts | 1.930 | 1.979 | 1.025 | +2.5% | 1.005 1.033 | 75.4% | 0.2% |
| predicate-subjects | 1.969 | 1.980 | 1.006 | +0.6% | 1.003 1.026 | 7.4% | 0.3% |
| range-topk | 7.174 | 6.476 | 0.903 | -9.7% | 0.905 0.886 | 1.0% | 8.6% |
| regex-iri | 36.688 | 38.055 | 1.037 | +3.7% | 1.033 1.041 | 1.1% | 0.2% |
| star-join | 35.047 | 52.969 | 1.511 | +51.1% | 1.511 1.513 | 0.2% | 1.5% |
| star-lookup | 1.983 | 1.982 | 0.999 | -0.1% | 1.000 0.999 | 0.1% | 0.2% |
| subquery-agg | 2.851 | 2.908 | 1.020 | +2.0% | 1.018 1.023 | 1.1% | 1.1% |
| two-hop-count | 28.959 | 29.069 | 1.004 | +0.4% | 1.005 1.003 | 0.3% | 0.7% |
| types-grouped | 1.962 | 1.973 | 1.006 | +0.6% | 1.149 1.002 | 33.2% | 0.9% |
| values-star | 1.974 | 1.977 | 1.001 | +0.1% | 1.004 1.002 | 0.9% | 0.2% |
| **geometric mean** | | | 1.027 | +2.7% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
