# Interleaved A/B on Namespace

- A: commit 4e9185f15e1397660651dad90c7817be1beef91e (4e9185f1), built on wally
- A sha256: 3ba8fd687308e297e5056a7d69e7cda65b0264b5a57e7e5d2a4a1c284bcc9dc3
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit 9acd6d742115a10249e89e752dcb8444c3266832 (9acd6d74), built on wally
- B sha256: f0090fa03425eb22765931c552c74ab41d35448e95aebdf4f64f1100e2984e8f
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

Load: A 0.96 s, B 0.73 s (B/A 0.758). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| count-all | 0.005 | 0.005 | 0.948 | -5.2% | 0.996 0.895 0.926 1.056 1.059 0.917 0.923 0.905 0.956 0.957 0.906 0.942 | 27.1% | 19.3% |
| distinct-obj | 0.004 | 0.004 | 0.949 | -5.1% | 1.005 0.928 0.927 1.026 1.071 0.948 0.932 0.917 0.948 0.920 0.902 0.938 | 32.9% | 20.5% |
| path-plus | 0.011 | 0.010 | 0.856 | -14.4% | 0.927 0.848 0.834 0.891 0.896 0.834 0.826 0.841 0.852 0.879 0.846 0.849 | 17.2% | 21.1% |
| predicate-counts | 0.005 | 0.005 | 0.952 | -4.8% | 1.039 0.905 0.964 0.997 0.970 0.922 0.909 0.929 0.936 0.941 0.942 0.975 | 22.3% | 16.7% |
| root-limit | 0.012 | 0.012 | 0.956 | -4.4% | 1.020 0.951 0.935 0.962 1.015 0.948 0.973 0.921 0.939 1.002 0.915 0.829 | 10.8% | 290.1% |
| root-scan | 0.088 | 0.087 | 0.991 | -0.9% | 0.982 1.007 0.998 0.991 0.989 0.985 0.985 0.985 0.993 0.994 0.991 1.002 | 4.8% | 1.8% |
| star-lookup | 0.064 | 0.064 | 0.994 | -0.6% | 0.961 1.011 1.031 0.974 1.013 0.976 0.990 0.992 0.996 0.981 0.992 1.000 | 12.3% | 11.3% |
| subquery-agg | 0.188 | 0.185 | 0.986 | -1.4% | 0.996 0.986 0.990 0.985 0.979 0.990 0.983 0.987 0.970 0.973 0.991 0.998 | 7.0% | 4.2% |
| values-star | 0.016 | 0.016 | 0.991 | -0.9% | 0.985 0.987 0.984 0.980 0.973 1.014 0.986 0.995 0.975 1.013 1.002 1.007 | 11.7% | 10.4% |
| **geometric mean** | | | 0.957 | -4.3% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| count-all | 0.975 | 0.974 | 0.999 | -0.1% | 1.000 0.999 1.001 1.034 0.998 0.980 0.999 1.000 0.997 0.974 1.002 0.999 | 3.2% | 4.9% |
| distinct-obj | 0.974 | 0.974 | 1.000 | +0.0% | 0.998 1.000 1.000 0.997 0.996 1.001 1.000 1.004 1.010 0.997 1.007 0.999 | 3.0% | 3.4% |
| path-plus | 0.975 | 0.975 | 1.000 | +0.0% | 0.999 0.998 1.002 1.000 0.997 1.000 1.001 1.001 1.001 0.998 1.000 0.996 | 2.0% | 2.7% |
| predicate-counts | 0.974 | 0.974 | 1.000 | +0.0% | 1.000 1.001 1.001 1.002 1.001 1.001 1.000 1.001 0.999 0.999 1.000 1.000 | 0.3% | 0.3% |
| root-limit | 2.034 | 2.034 | 1.000 | +0.0% | 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.000 | 0.1% | 0.1% |
| root-scan | 52.049 | 51.966 | 0.998 | -0.2% | 0.998 0.984 0.998 0.999 0.999 0.999 0.999 0.998 0.998 0.990 0.999 0.998 | 0.2% | 1.8% |
| star-lookup | 1.981 | 1.981 | 1.000 | +0.0% | 1.000 1.001 1.000 1.001 0.999 0.999 1.001 1.001 0.999 1.000 1.001 1.001 | 0.3% | 1.8% |
| subquery-agg | 1.971 | 1.971 | 1.000 | +0.0% | 1.001 1.001 1.000 0.999 1.001 0.999 0.999 1.000 1.000 1.000 1.000 1.000 | 0.3% | 0.4% |
| values-star | 1.975 | 1.976 | 1.001 | +0.1% | 1.002 1.013 1.000 0.988 1.016 1.002 1.000 1.001 1.001 0.998 1.002 0.997 | 4.1% | 3.1% |
| **geometric mean** | | | 1.000 | -0.0% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
