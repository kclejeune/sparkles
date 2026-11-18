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
| class-predicate-list | 10.213 | 10.540 | 1.032 | +3.2% | 1.056 1.011 | 4.5% | 3.3% |
| class-predicates | 10.147 | 10.562 | 1.041 | +4.1% | 1.044 1.030 | 3.4% | 2.6% |
| contains | 4.209 | 4.198 | 0.997 | -0.3% | 1.011 0.986 | 1.5% | 2.2% |
| count-all | 0.009 | 0.011 | 1.182 | +18.2% | 1.232 0.622 | 10.0% | 207.9% |
| distinct-join | 39.015 | 37.799 | 0.969 | -3.1% | 0.963 0.973 | 7.3% | 1.3% |
| distinct-obj | 0.006 | 0.009 | 1.395 | +39.5% | 1.346 1.791 | 26.5% | 46.6% |
| employee-docs | 0.067 | 0.080 | 1.193 | +19.3% | 1.193 1.206 | 5.6% | 4.8% |
| exists-join | 8.969 | 8.834 | 0.985 | -1.5% | 0.970 0.979 | 4.0% | 4.3% |
| export-500k | 2.583 | 0.423 | 0.164 | -83.6% | 0.139 0.164 | 95.0% | 2.7% |
| expr-agg-arg | 28.797 | 23.193 | 0.805 | -19.5% | 0.786 0.817 | 4.1% | 1.0% |
| expr-bind-group | 4.826 | 3.428 | 0.710 | -29.0% | 0.696 0.730 | 3.5% | 6.0% |
| expr-order-key | 10.106 | 11.111 | 1.099 | +9.9% | 1.112 1.094 | 1.7% | 1.4% |
| group-avg | 46.154 | 37.578 | 0.814 | -18.6% | 0.811 0.812 | 2.8% | 1.2% |
| knows-reach | 143.563 | 140.487 | 0.979 | -2.1% | 0.977 0.980 | 1.5% | 3.0% |
| lang-filter | 2.907 | 2.917 | 1.003 | +0.3% | 0.999 0.993 | 1.8% | 5.6% |
| minus | 3.302 | 2.571 | 0.779 | -22.1% | 0.783 0.775 | 4.8% | 1.7% |
| not-exists | 10.163 | 9.099 | 0.895 | -10.5% | 0.895 0.890 | 7.6% | 4.3% |
| optional-chain | 45.234 | 25.478 | 0.563 | -43.7% | 0.634 0.546 | 23.3% | 18.7% |
| optional-count | 5.464 | 5.180 | 0.948 | -5.2% | 0.954 0.943 | 3.8% | 1.6% |
| order-by-full | 187.553 | 191.771 | 1.022 | +2.2% | 0.989 0.983 | 24.6% | 43.7% |
| path-plus | 0.015 | 0.015 | 0.974 | -2.6% | 0.983 0.953 | 7.2% | 8.7% |
| predicate-counts | 0.008 | 0.011 | 1.457 | +45.7% | 1.380 1.628 | 52.0% | 36.1% |
| predicate-subjects | 0.009 | 0.015 | 1.602 | +60.2% | 1.732 1.361 | 29.5% | 29.2% |
| range-topk | 2.175 | 2.221 | 1.021 | +2.1% | 1.021 1.028 | 14.9% | 27.3% |
| regex-iri | 6.202 | 6.220 | 1.003 | +0.3% | 1.011 0.997 | 1.3% | 4.7% |
| star-join | 13.719 | 14.027 | 1.022 | +2.2% | 1.020 1.018 | 5.3% | 2.0% |
| star-lookup | 0.073 | 0.081 | 1.099 | +9.9% | 1.079 1.160 | 3.8% | 12.0% |
| subquery-agg | 0.806 | 0.836 | 1.036 | +3.6% | 1.002 1.098 | 7.3% | 9.6% |
| two-hop-count | 11.323 | 11.256 | 0.994 | -0.6% | 1.000 0.989 | 2.0% | 3.0% |
| types-grouped | 0.012 | 0.020 | 1.700 | +70.0% | 1.788 1.577 | 7.0% | 60.6% |
| values-star | 0.021 | 0.032 | 1.579 | +57.9% | 1.495 1.629 | 10.2% | 40.3% |
| **geometric mean** | | | 0.975 | -2.5% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 11.285 | 11.634 | 1.031 | +3.1% | 1.042 1.016 | 2.8% | 3.9% |
| class-predicates | 11.261 | 11.741 | 1.043 | +4.3% | 1.051 1.029 | 3.5% | 1.9% |
| contains | 5.011 | 5.162 | 1.030 | +3.0% | 1.036 1.024 | 1.9% | 1.3% |
| count-all | 0.489 | 0.740 | 1.512 | +51.2% | 1.655 1.209 | 10.5% | 52.5% |
| distinct-join | 44.702 | 43.724 | 0.978 | -2.2% | 0.975 0.988 | 6.0% | 0.5% |
| distinct-obj | 0.474 | 0.800 | 1.685 | +68.5% | 1.635 1.752 | 22.3% | 16.3% |
| employee-docs | 0.718 | 0.978 | 1.363 | +36.3% | 1.366 1.363 | 4.3% | 18.6% |
| exists-join | 9.931 | 10.010 | 1.008 | +0.8% | 0.995 1.006 | 3.7% | 4.2% |
| export-500k | 242.538 | 266.629 | 1.099 | +9.9% | 1.093 1.104 | 2.1% | 2.6% |
| expr-agg-arg | 29.963 | 24.415 | 0.815 | -18.5% | 0.792 0.830 | 4.4% | 1.1% |
| expr-bind-group | 5.678 | 4.463 | 0.786 | -21.4% | 0.769 0.806 | 3.6% | 2.9% |
| expr-order-key | 11.899 | 12.267 | 1.031 | +3.1% | 1.039 1.020 | 1.1% | 2.4% |
| group-avg | 47.573 | 38.805 | 0.816 | -18.4% | 0.811 0.812 | 2.9% | 1.0% |
| knows-reach | 144.935 | 141.739 | 0.978 | -2.2% | 0.977 0.980 | 1.5% | 3.0% |
| lang-filter | 3.702 | 3.889 | 1.051 | +5.1% | 1.035 1.060 | 1.2% | 6.8% |
| minus | 4.103 | 3.567 | 0.869 | -13.1% | 0.850 0.894 | 2.7% | 5.9% |
| not-exists | 11.244 | 10.097 | 0.898 | -10.2% | 0.908 0.908 | 7.2% | 3.3% |
| optional-chain | 155.953 | 161.172 | 1.033 | +3.3% | 1.040 1.023 | 12.1% | 5.6% |
| optional-count | 6.354 | 6.244 | 0.983 | -1.7% | 0.987 0.982 | 2.1% | 1.2% |
| order-by-full | 528.413 | 555.395 | 1.051 | +5.1% | 1.046 1.051 | 8.3% | 14.4% |
| path-plus | 0.522 | 0.776 | 1.488 | +48.8% | 1.515 1.425 | 3.1% | 12.2% |
| predicate-counts | 0.520 | 0.821 | 1.581 | +58.1% | 1.648 1.612 | 33.5% | 21.7% |
| predicate-subjects | 0.448 | 0.800 | 1.787 | +78.7% | 1.806 1.701 | 21.7% | 14.3% |
| range-topk | 3.078 | 3.312 | 1.076 | +7.6% | 1.108 1.052 | 13.6% | 17.3% |
| regex-iri | 7.008 | 7.217 | 1.030 | +3.0% | 1.037 1.029 | 1.9% | 3.7% |
| star-join | 34.535 | 42.565 | 1.233 | +23.3% | 1.232 1.230 | 4.1% | 1.5% |
| star-lookup | 0.812 | 1.084 | 1.334 | +33.4% | 1.382 1.264 | 7.2% | 22.0% |
| subquery-agg | 1.590 | 1.864 | 1.173 | +17.3% | 1.143 1.205 | 3.7% | 9.9% |
| two-hop-count | 12.497 | 12.439 | 0.995 | -0.5% | 0.997 0.995 | 1.6% | 2.6% |
| types-grouped | 0.581 | 0.845 | 1.454 | +45.4% | 1.453 1.490 | 15.3% | 14.3% |
| values-star | 0.677 | 0.915 | 1.351 | +35.1% | 1.328 1.403 | 3.9% | 10.8% |
| **geometric mean** | | | 1.120 | +12.0% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
