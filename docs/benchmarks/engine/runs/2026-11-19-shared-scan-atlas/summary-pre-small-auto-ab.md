# Interleaved A/B on Namespace

- A: commit 9acd6d742115a10249e89e752dcb8444c3266832 (9acd6d74), built on wally
- A sha256: f0090fa03425eb22765931c552c74ab41d35448e95aebdf4f64f1100e2984e8f
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit 4e9185f15e1397660651dad90c7817be1beef91e (4e9185f1), built on wally
- B sha256: 3ba8fd687308e297e5056a7d69e7cda65b0264b5a57e7e5d2a4a1c284bcc9dc3
- B compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- data: 100000 people (scripts/gen-data.py), 6.0M compressed
- host: atlas (ssh, nsc-bench-remote.sh run directly)
- settings: 9 queries, 12 rounds of A,B,B,A, 3 warm-up and 200 timed requests per query and server, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners 1, execution auto
- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

A and B returned the same sorted TSV answers to all 9 queries.

Load: A 0.94 s, B 0.68 s (B/A 0.728). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| count-all | 0.005 | 0.005 | 0.982 | -1.8% | 0.908 1.175 0.970 0.970 1.012 0.935 1.000 1.032 1.037 0.966 0.999 0.931 | 12.8% | 42.4% |
| distinct-obj | 0.004 | 0.004 | 0.965 | -3.5% | 0.952 1.116 0.937 0.922 1.001 0.939 0.965 0.960 0.998 0.999 1.002 0.878 | 18.2% | 39.9% |
| path-plus | 0.010 | 0.011 | 1.121 | +12.1% | 1.096 1.584 1.115 1.117 1.152 1.110 1.156 1.076 1.082 1.122 1.105 1.072 | 16.0% | 50.5% |
| predicate-counts | 0.005 | 0.005 | 0.985 | -1.5% | 0.936 1.062 1.029 0.974 1.027 0.962 1.022 0.949 1.026 0.983 0.986 0.900 | 14.8% | 30.3% |
| root-limit | 0.012 | 0.012 | 1.030 | +3.0% | 0.978 1.340 1.051 1.182 0.954 1.009 1.040 1.043 1.013 1.195 0.983 0.976 | 248.0% | 394.7% |
| root-scan | 0.084 | 0.085 | 1.012 | +1.2% | 1.020 1.043 0.999 1.002 1.012 1.012 1.018 1.011 1.011 1.015 1.001 1.014 | 2.8% | 8.5% |
| star-lookup | 0.062 | 0.061 | 0.995 | -0.5% | 0.990 1.047 0.975 1.013 0.984 0.964 1.010 1.004 0.976 0.991 0.981 0.998 | 6.6% | 13.4% |
| subquery-agg | 0.183 | 0.186 | 1.014 | +1.4% | 1.011 1.027 1.009 1.012 1.004 1.013 1.020 1.023 1.021 1.024 1.008 0.999 | 3.2% | 5.1% |
| values-star | 0.016 | 0.015 | 0.959 | -4.1% | 1.002 1.002 0.981 0.957 0.950 0.919 0.976 0.924 0.958 0.948 0.955 0.947 | 13.9% | 15.8% |
| **geometric mean** | | | 1.006 | +0.6% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| count-all | 0.975 | 0.975 | 1.000 | +0.0% | 0.997 1.004 1.000 0.997 1.001 1.029 1.000 0.998 1.000 1.000 1.001 1.000 | 4.9% | 8.8% |
| distinct-obj | 0.974 | 0.974 | 1.000 | +0.0% | 1.036 1.059 0.999 1.000 0.999 1.008 1.000 0.998 0.999 0.999 1.000 0.999 | 5.4% | 33.2% |
| path-plus | 0.975 | 0.975 | 1.000 | +0.0% | 1.015 1.355 0.998 0.999 0.996 1.003 0.998 0.999 0.999 0.998 1.000 0.998 | 4.7% | 65.3% |
| predicate-counts | 0.974 | 0.974 | 1.000 | +0.0% | 1.000 1.000 1.000 0.999 0.998 0.999 1.000 0.999 1.001 1.001 0.999 0.998 | 2.4% | 4.7% |
| root-limit | 2.034 | 2.033 | 1.000 | -0.0% | 1.000 0.999 1.000 1.000 1.000 0.999 1.000 1.000 1.000 1.000 1.000 1.000 | 0.1% | 0.1% |
| root-scan | 51.299 | 52.027 | 1.014 | +1.4% | 1.016 1.018 1.018 1.014 1.018 1.006 1.009 1.016 1.016 1.015 1.007 1.001 | 1.7% | 0.1% |
| star-lookup | 1.981 | 1.982 | 1.001 | +0.1% | 1.000 1.000 1.000 1.001 1.001 0.999 1.001 0.999 1.000 1.002 1.000 1.001 | 0.8% | 0.2% |
| subquery-agg | 1.971 | 1.970 | 1.000 | -0.0% | 1.001 0.999 1.000 1.000 0.999 0.999 0.999 1.001 0.999 1.001 1.000 1.001 | 0.3% | 0.3% |
| values-star | 1.975 | 1.975 | 1.000 | +0.0% | 1.001 1.000 1.001 0.998 0.997 0.986 1.012 1.010 1.000 1.005 1.011 0.999 | 1.7% | 2.1% |
| **geometric mean** | | | 1.002 | +0.2% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
