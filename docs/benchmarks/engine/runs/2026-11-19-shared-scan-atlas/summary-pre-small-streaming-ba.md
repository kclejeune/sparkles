# Interleaved A/B on Namespace

- A: commit 4e9185f15e1397660651dad90c7817be1beef91e (4e9185f1), built on wally
- A sha256: 3ba8fd687308e297e5056a7d69e7cda65b0264b5a57e7e5d2a4a1c284bcc9dc3
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit 9acd6d742115a10249e89e752dcb8444c3266832 (9acd6d74), built on wally
- B sha256: f0090fa03425eb22765931c552c74ab41d35448e95aebdf4f64f1100e2984e8f
- B compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- data: 100000 people (scripts/gen-data.py), 6.0M compressed
- host: atlas (ssh, nsc-bench-remote.sh run directly)
- settings: 9 queries, 12 rounds of A,B,B,A, 3 warm-up and 200 timed requests per query and server, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners 1, execution streaming
- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

A and B returned the same sorted TSV answers to all 9 queries.

Load: A 0.93 s, B 0.62 s (B/A 0.671). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| count-all | 0.006 | 0.005 | 0.975 | -2.5% | 0.989 0.972 0.949 1.007 0.994 0.924 0.962 0.954 1.003 0.993 0.977 0.999 | 10.2% | 11.3% |
| distinct-obj | 0.005 | 0.005 | 0.987 | -1.3% | 1.027 0.992 0.960 1.011 0.984 0.982 0.979 0.988 0.974 0.988 0.951 1.025 | 14.3% | 14.4% |
| path-plus | 0.011 | 0.010 | 0.954 | -4.6% | 0.973 0.962 0.931 0.990 0.936 0.969 0.933 0.934 0.909 0.985 0.946 0.975 | 17.1% | 18.4% |
| predicate-counts | 0.006 | 0.006 | 1.020 | +2.0% | 1.008 0.990 0.976 1.079 1.019 1.014 1.013 1.045 0.984 1.010 0.970 1.107 | 16.1% | 17.7% |
| root-limit | 0.004 | 0.004 | 0.997 | -0.3% | 0.959 0.946 1.116 0.934 1.054 1.006 1.053 0.907 1.039 0.962 1.009 0.947 | 14.7% | 18.0% |
| root-scan | 0.085 | 0.083 | 0.978 | -2.2% | 0.970 0.969 0.973 0.987 0.974 0.984 0.978 0.982 0.983 0.977 0.977 0.982 | 3.1% | 2.9% |
| star-lookup | 0.063 | 0.064 | 1.009 | +0.9% | 1.018 1.014 1.017 1.031 1.010 1.028 1.020 0.977 0.986 1.009 0.999 1.008 | 6.5% | 10.0% |
| subquery-agg | 0.183 | 0.182 | 0.997 | -0.3% | 0.995 0.978 0.993 1.020 0.995 0.998 0.992 0.997 0.997 1.000 1.003 0.990 | 4.0% | 2.9% |
| values-star | 0.019 | 0.019 | 0.999 | -0.1% | 0.969 1.008 0.971 1.024 1.016 1.013 1.005 0.970 0.994 1.004 0.994 1.005 | 7.6% | 8.3% |
| **geometric mean** | | | 0.990 | -1.0% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| count-all | 0.974 | 0.974 | 1.000 | +0.0% | 0.999 0.999 1.001 1.000 1.001 0.999 0.999 1.001 1.002 1.000 0.998 1.000 | 6.7% | 2.4% |
| distinct-obj | 0.976 | 0.977 | 1.001 | +0.1% | 1.001 1.035 1.001 1.001 1.004 1.001 1.001 1.000 1.003 1.000 1.000 1.001 | 0.5% | 5.7% |
| path-plus | 0.977 | 0.977 | 1.000 | +0.0% | 1.027 1.000 1.001 1.000 0.939 1.001 1.000 1.000 1.002 0.999 0.997 0.998 | 6.8% | 5.7% |
| predicate-counts | 1.086 | 1.370 | 1.262 | +26.2% | 1.368 0.986 1.032 1.371 0.888 1.268 0.969 1.083 0.772 1.031 0.698 1.611 | 92.4% | 99.2% |
| root-limit | 2.042 | 2.043 | 1.000 | +0.0% | 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 | 0.1% | 0.1% |
| root-scan | 51.287 | 51.045 | 0.995 | -0.5% | 0.991 0.994 0.995 0.996 0.960 0.994 0.997 0.999 0.997 0.995 0.989 0.995 | 5.7% | 0.1% |
| star-lookup | 1.983 | 1.983 | 1.000 | +0.0% | 0.998 1.000 1.000 0.999 0.999 1.001 1.001 1.002 1.001 1.001 0.999 1.000 | 1.2% | 1.6% |
| subquery-agg | 1.972 | 1.972 | 1.000 | +0.0% | 0.999 0.999 1.000 0.999 0.999 1.000 1.001 1.001 1.000 1.001 1.001 0.999 | 1.4% | 1.6% |
| values-star | 1.978 | 1.978 | 1.000 | +0.0% | 0.999 0.999 0.999 1.000 0.999 0.999 1.001 1.001 1.001 1.001 1.000 0.999 | 2.0% | 1.5% |
| **geometric mean** | | | 1.026 | +2.6% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
