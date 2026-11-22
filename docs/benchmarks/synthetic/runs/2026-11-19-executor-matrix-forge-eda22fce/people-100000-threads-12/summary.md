# Interleaved A/B on Namespace

- binary: /home/kclejeune/lanes/forge-backlog/bin/sparkles-eda22fce-matrix
- binary sha256: 915ba62e2bade5551f25d5b0c7acca90aa8578e21f180dfb7ccdbf6290fc023c
- binary commit: eda22fce
- version: sparkles 0.1.0
- compilers: GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- build config: {"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1},"sparkles-owl":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{"CARGO_INCREMENTAL":"0"},"mbx_policy":"779a99e845a5ac3f1c4d64a3c95bcc74b33f47418e829bf008542ec2dbbcd151","profile":"release"}
- runtime env: clean
- scripts commit: unknown
- scripts dirty: 0
- host: forge
- started: 2026-11-19T02:31:44Z
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=12, server CPU 0-11, client CPU 12, spinners off, execution eager,streaming
- A: /home/kclejeune/lanes/forge-backlog/bin/sparkles-eda22fce-matrix
- B: /home/kclejeune/lanes/forge-backlog/bin/sparkles-eda22fce-matrix
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
| class-predicate-list | 0.992 | 0.991 | 0.999 | -0.1% | 1.005 0.997 | 9.6% | 8.9% |
| class-predicates | 1.006 | 1.006 | 1.000 | +0.0% | 1.007 0.989 | 6.8% | 5.7% |
| contains | 0.735 | 0.742 | 1.008 | +0.8% | 1.058 0.936 | 5.0% | 22.0% |
| count-all | 0.007 | 0.011 | 1.570 | +57.0% | 2.157 1.395 | 30.1% | 44.0% |
| distinct-join | 3.862 | 3.970 | 1.028 | +2.8% | 1.009 1.056 | 9.2% | 3.9% |
| distinct-obj | 0.006 | 0.011 | 1.871 | +87.1% | 1.700 2.228 | 59.8% | 26.1% |
| employee-docs | 0.052 | 0.055 | 1.063 | +6.3% | 1.024 1.082 | 9.9% | 21.8% |
| exists-join | 1.162 | 1.141 | 0.982 | -1.8% | 0.960 0.997 | 6.2% | 3.7% |
| export-500k | 2.345 | 0.377 | 0.161 | -83.9% | 0.170 0.155 | 12.8% | 0.8% |
| expr-agg-arg | 2.895 | 2.028 | 0.700 | -30.0% | 0.701 0.699 | 3.4% | 2.7% |
| expr-bind-group | 0.828 | 0.392 | 0.473 | -52.7% | 0.443 0.480 | 22.7% | 10.3% |
| expr-order-key | 1.255 | 1.238 | 0.986 | -1.4% | 0.995 0.975 | 2.7% | 5.1% |
| group-avg | 4.051 | 3.359 | 0.829 | -17.1% | 0.814 0.836 | 14.2% | 2.7% |
| knows-reach | 11.795 | 11.513 | 0.976 | -2.4% | 0.971 0.979 | 2.0% | 1.8% |
| lang-filter | 0.802 | 0.766 | 0.955 | -4.5% | 0.943 0.982 | 14.1% | 12.3% |
| minus | 0.458 | 0.285 | 0.623 | -37.7% | 0.631 0.619 | 4.6% | 3.9% |
| not-exists | 0.869 | 0.796 | 0.917 | -8.3% | 0.919 0.907 | 6.9% | 1.4% |
| optional-chain | 2.128 | 2.200 | 1.034 | +3.4% | 1.034 1.051 | 4.6% | 11.1% |
| optional-count | 0.575 | 0.580 | 1.008 | +0.8% | 1.036 1.004 | 17.2% | 5.1% |
| order-by-full | 8.752 | 9.174 | 1.048 | +4.8% | 1.061 1.046 | 3.9% | 6.2% |
| path-plus | 0.018 | 0.021 | 1.138 | +13.8% | 1.161 1.095 | 6.4% | 23.0% |
| predicate-counts | 0.007 | 0.011 | 1.546 | +54.6% | 1.569 1.517 | 9.5% | 25.7% |
| predicate-subjects | 0.010 | 0.015 | 1.511 | +51.1% | 1.520 1.427 | 19.7% | 25.3% |
| range-topk | 0.589 | 0.580 | 0.984 | -1.6% | 1.014 0.947 | 8.8% | 7.6% |
| regex-iri | 1.056 | 1.087 | 1.029 | +2.9% | 1.066 0.982 | 11.5% | 4.7% |
| star-join | 1.643 | 1.660 | 1.010 | +1.0% | 0.996 1.011 | 7.8% | 4.0% |
| star-lookup | 0.086 | 0.089 | 1.032 | +3.2% | 1.099 0.993 | 13.3% | 12.2% |
| subquery-agg | 0.250 | 0.222 | 0.886 | -11.4% | 1.004 0.839 | 20.7% | 48.3% |
| two-hop-count | 1.237 | 1.228 | 0.992 | -0.8% | 0.978 1.028 | 5.2% | 13.1% |
| types-grouped | 0.012 | 0.019 | 1.587 | +58.7% | 1.550 1.650 | 7.5% | 57.1% |
| values-star | 0.021 | 0.027 | 1.264 | +26.4% | 1.264 1.280 | 3.2% | 6.9% |
| **geometric mean** | | | 0.971 | -2.9% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 1.677 | 1.946 | 1.160 | +16.0% | 1.164 1.149 | 2.2% | 2.4% |
| class-predicates | 1.700 | 1.970 | 1.159 | +15.9% | 1.144 1.168 | 4.1% | 5.3% |
| contains | 1.460 | 1.647 | 1.128 | +12.8% | 1.155 1.102 | 3.7% | 19.6% |
| count-all | 0.427 | 0.792 | 1.856 | +85.6% | 1.753 1.854 | 3.5% | 23.1% |
| distinct-join | 5.111 | 5.570 | 1.090 | +9.0% | 1.080 1.101 | 5.3% | 5.7% |
| distinct-obj | 0.482 | 0.762 | 1.582 | +58.2% | 1.489 1.635 | 27.1% | 7.3% |
| employee-docs | 0.694 | 0.860 | 1.238 | +23.8% | 1.231 1.232 | 6.4% | 29.2% |
| exists-join | 1.911 | 2.099 | 1.098 | +9.8% | 1.087 1.122 | 4.4% | 9.2% |
| export-500k | 218.201 | 274.778 | 1.259 | +25.9% | 1.260 1.257 | 0.6% | 0.1% |
| expr-agg-arg | 3.696 | 3.034 | 0.821 | -17.9% | 0.839 0.818 | 3.2% | 7.6% |
| expr-bind-group | 1.551 | 1.303 | 0.840 | -16.0% | 0.820 0.809 | 10.8% | 23.1% |
| expr-order-key | 2.018 | 2.109 | 1.045 | +4.5% | 1.058 1.005 | 4.6% | 11.8% |
| group-avg | 4.957 | 4.354 | 0.878 | -12.2% | 0.872 0.880 | 11.6% | 4.7% |
| knows-reach | 12.848 | 12.558 | 0.977 | -2.3% | 0.972 0.978 | 2.1% | 2.8% |
| lang-filter | 1.512 | 1.648 | 1.090 | +9.0% | 1.053 1.147 | 4.8% | 8.9% |
| minus | 1.152 | 1.195 | 1.036 | +3.6% | 1.029 1.020 | 4.0% | 6.8% |
| not-exists | 1.575 | 1.748 | 1.110 | +11.0% | 1.114 1.109 | 0.7% | 9.8% |
| optional-chain | 14.018 | 17.889 | 1.276 | +27.6% | 1.267 1.278 | 2.5% | 2.7% |
| optional-count | 1.288 | 1.489 | 1.156 | +15.6% | 1.109 1.213 | 3.3% | 11.3% |
| order-by-full | 41.102 | 48.714 | 1.185 | +18.5% | 1.197 1.175 | 1.5% | 1.8% |
| path-plus | 0.528 | 0.823 | 1.559 | +55.9% | 1.438 1.632 | 9.1% | 20.8% |
| predicate-counts | 0.514 | 0.845 | 1.641 | +64.1% | 1.610 1.575 | 9.7% | 35.6% |
| predicate-subjects | 0.501 | 0.740 | 1.478 | +47.8% | 1.493 1.419 | 18.3% | 16.5% |
| range-topk | 1.409 | 1.631 | 1.158 | +15.8% | 1.181 1.126 | 0.6% | 10.2% |
| regex-iri | 1.773 | 1.955 | 1.103 | +10.3% | 1.111 1.082 | 7.7% | 5.6% |
| star-join | 5.080 | 5.857 | 1.153 | +15.3% | 1.146 1.160 | 2.9% | 5.0% |
| star-lookup | 0.808 | 1.062 | 1.314 | +31.4% | 1.346 1.316 | 10.4% | 7.8% |
| subquery-agg | 0.892 | 1.026 | 1.151 | +15.1% | 1.196 1.127 | 14.6% | 9.4% |
| two-hop-count | 2.026 | 2.316 | 1.143 | +14.3% | 1.152 1.136 | 4.9% | 9.0% |
| types-grouped | 0.569 | 0.784 | 1.377 | +37.7% | 1.517 1.250 | 9.6% | 23.6% |
| values-star | 0.685 | 0.932 | 1.361 | +36.1% | 1.466 1.264 | 7.9% | 13.8% |
| **geometric mean** | | | 1.187 | +18.7% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
