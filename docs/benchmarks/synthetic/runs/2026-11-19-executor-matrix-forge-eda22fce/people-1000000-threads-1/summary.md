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
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners on, execution eager,streaming
- A: /home/kclejeune/lanes/forge-backlog/bin/sparkles-eda22fce-matrix
- B: /home/kclejeune/lanes/forge-backlog/bin/sparkles-eda22fce-matrix
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
| class-predicate-list | 10.057 | 10.345 | 1.029 | +2.9% | 1.029 1.026 | 2.5% | 1.0% |
| class-predicates | 9.966 | 10.375 | 1.041 | +4.1% | 1.031 1.050 | 1.6% | 1.8% |
| contains | 24.722 | 24.663 | 0.998 | -0.2% | 0.997 0.998 | 1.1% | 0.4% |
| count-all | 0.006 | 0.007 | 1.181 | +18.1% | 1.229 1.099 | 6.3% | 12.2% |
| distinct-join | 39.112 | 38.223 | 0.977 | -2.3% | 0.984 0.975 | 0.2% | 1.2% |
| distinct-obj | 0.005 | 0.005 | 1.140 | +14.0% | 1.208 1.098 | 5.2% | 11.8% |
| employee-docs | 0.049 | 0.053 | 1.077 | +7.7% | 1.113 1.065 | 5.4% | 11.2% |
| exists-join | 8.597 | 8.588 | 0.999 | -0.1% | 1.001 0.992 | 1.9% | 1.0% |
| export-500k | 2.182 | 0.404 | 0.185 | -81.5% | 0.187 0.182 | 12.9% | 2.5% |
| expr-agg-arg | 28.045 | 23.051 | 0.822 | -17.8% | 0.822 0.821 | 1.6% | 0.4% |
| expr-bind-group | 5.635 | 3.293 | 0.584 | -41.6% | 0.584 0.594 | 2.2% | 0.3% |
| expr-order-key | 11.057 | 10.738 | 0.971 | -2.9% | 0.973 0.967 | 0.7% | 0.7% |
| group-avg | 44.592 | 36.927 | 0.828 | -17.2% | 0.834 0.827 | 1.4% | 0.6% |
| knows-reach | 139.016 | 136.442 | 0.981 | -1.9% | 0.983 0.980 | 0.7% | 0.5% |
| lang-filter | 18.382 | 18.456 | 1.004 | +0.4% | 1.005 1.003 | 0.2% | 0.3% |
| minus | 4.232 | 2.464 | 0.582 | -41.8% | 0.590 0.578 | 2.5% | 1.5% |
| not-exists | 9.451 | 8.724 | 0.923 | -7.7% | 0.930 0.903 | 1.3% | 2.6% |
| optional-chain | 26.728 | 24.874 | 0.931 | -6.9% | 0.930 0.934 | 3.9% | 3.8% |
| optional-count | 5.275 | 5.275 | 1.000 | -0.0% | 0.998 1.002 | 2.9% | 0.1% |
| order-by-full | 514.678 | 518.713 | 1.008 | +0.8% | 1.012 1.006 | 0.7% | 0.4% |
| path-plus | 0.009 | 0.009 | 1.063 | +6.3% | 0.988 1.096 | 8.6% | 9.3% |
| predicate-counts | 0.005 | 0.006 | 1.236 | +23.6% | 1.250 1.198 | 10.2% | 4.7% |
| predicate-subjects | 0.007 | 0.009 | 1.245 | +24.5% | 1.228 1.266 | 10.4% | 8.1% |
| range-topk | 5.151 | 5.091 | 0.988 | -1.2% | 0.988 0.990 | 0.4% | 0.7% |
| regex-iri | 37.660 | 37.635 | 0.999 | -0.1% | 0.999 1.000 | 0.2% | 0.2% |
| star-join | 14.074 | 14.875 | 1.057 | +5.7% | 1.056 1.057 | 0.6% | 1.1% |
| star-lookup | 0.058 | 0.061 | 1.047 | +4.7% | 1.033 1.070 | 3.9% | 4.3% |
| subquery-agg | 1.697 | 1.632 | 0.962 | -3.8% | 0.956 0.969 | 1.4% | 1.3% |
| two-hop-count | 27.656 | 27.656 | 1.000 | -0.0% | 1.000 1.000 | 0.1% | 0.2% |
| types-grouped | 0.008 | 0.013 | 1.576 | +57.6% | 1.559 1.575 | 5.9% | 2.4% |
| values-star | 0.017 | 0.021 | 1.220 | +22.0% | 1.190 1.242 | 7.1% | 4.2% |
| **geometric mean** | | | 0.946 | -5.4% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 11.460 | 11.683 | 1.019 | +1.9% | 1.007 1.030 | 3.1% | 0.7% |
| class-predicates | 11.317 | 11.755 | 1.039 | +3.9% | 1.032 1.050 | 2.0% | 1.5% |
| contains | 25.940 | 25.932 | 1.000 | -0.0% | 0.998 1.001 | 1.1% | 0.4% |
| count-all | 1.610 | 1.974 | 1.226 | +22.6% | 1.254 1.194 | 76.0% | 0.4% |
| distinct-join | 44.279 | 45.943 | 1.038 | +3.8% | 1.039 1.035 | 0.3% | 2.6% |
| distinct-obj | 1.971 | 1.976 | 1.003 | +0.3% | 1.002 1.010 | 12.1% | 0.1% |
| employee-docs | 1.982 | 1.986 | 1.002 | +0.2% | 1.002 1.002 | 0.2% | 0.2% |
| exists-join | 9.927 | 9.951 | 1.002 | +0.2% | 1.007 1.001 | 1.1% | 1.1% |
| export-500k | 236.022 | 342.020 | 1.449 | +44.9% | 1.455 1.449 | 0.9% | 0.0% |
| expr-agg-arg | 29.480 | 24.422 | 0.828 | -17.2% | 0.832 0.827 | 1.6% | 1.0% |
| expr-bind-group | 6.866 | 4.578 | 0.667 | -33.3% | 0.662 0.673 | 2.2% | 0.4% |
| expr-order-key | 13.058 | 12.239 | 0.937 | -6.3% | 0.940 0.937 | 0.6% | 3.7% |
| group-avg | 46.260 | 38.514 | 0.833 | -16.7% | 0.840 0.830 | 1.3% | 0.7% |
| knows-reach | 140.419 | 137.868 | 0.982 | -1.8% | 0.984 0.981 | 0.8% | 0.4% |
| lang-filter | 19.471 | 19.671 | 1.010 | +1.0% | 1.011 1.006 | 0.6% | 1.1% |
| minus | 5.479 | 3.624 | 0.661 | -33.9% | 0.658 0.671 | 3.3% | 5.2% |
| not-exists | 10.752 | 9.962 | 0.927 | -7.3% | 0.939 0.916 | 2.0% | 2.4% |
| optional-chain | 135.031 | 213.014 | 1.578 | +57.8% | 1.578 1.560 | 1.8% | 0.0% |
| optional-count | 6.566 | 6.591 | 1.004 | +0.4% | 1.036 0.999 | 5.5% | 6.6% |
| order-by-full | 850.098 | 1039.980 | 1.223 | +22.3% | 1.226 1.221 | 0.5% | 0.3% |
| path-plus | 1.973 | 1.976 | 1.002 | +0.2% | 1.002 1.001 | 16.4% | 0.9% |
| predicate-counts | 1.403 | 1.984 | 1.413 | +41.3% | 1.292 1.537 | 38.7% | 0.3% |
| predicate-subjects | 1.909 | 1.982 | 1.038 | +3.8% | 1.005 1.538 | 53.3% | 0.1% |
| range-topk | 6.497 | 6.469 | 0.996 | -0.4% | 1.026 0.994 | 6.9% | 7.2% |
| regex-iri | 38.922 | 38.896 | 0.999 | -0.1% | 0.999 1.000 | 0.3% | 0.4% |
| star-join | 36.072 | 51.153 | 1.418 | +41.8% | 1.420 1.417 | 0.3% | 0.8% |
| star-lookup | 1.985 | 1.986 | 1.001 | +0.1% | 1.000 1.000 | 0.3% | 1.1% |
| subquery-agg | 2.893 | 2.879 | 0.995 | -0.5% | 1.001 0.974 | 2.4% | 5.3% |
| two-hop-count | 29.000 | 29.034 | 1.001 | +0.1% | 1.000 1.002 | 0.1% | 0.2% |
| types-grouped | 1.971 | 1.976 | 1.002 | +0.2% | 1.002 1.002 | 21.3% | 0.2% |
| values-star | 1.981 | 1.983 | 1.001 | +0.1% | 1.001 1.001 | 0.2% | 0.3% |
| **geometric mean** | | | 1.024 | +2.4% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
