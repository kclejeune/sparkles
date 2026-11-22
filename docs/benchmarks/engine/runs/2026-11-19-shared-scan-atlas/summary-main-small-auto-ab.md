# Interleaved A/B on Namespace

- A: commit c0948014705b4bb40aaa7fe4c86c3c671444b7e9 (c0948014), built on wally
- A sha256: 1b04d43f6bb91343a5de3204f366d973b976851dab1a44f94350bd0bd065edff
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

Load: A 1.03 s, B 0.66 s (B/A 0.645). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| count-all | 0.005 | 0.004 | 0.923 | -7.7% | 0.885 0.943 0.945 0.921 0.911 0.897 0.933 0.851 0.945 0.954 0.928 0.961 | 20.6% | 15.4% |
| distinct-obj | 0.004 | 0.004 | 0.947 | -5.3% | 0.869 0.990 0.970 0.921 0.946 0.919 0.959 0.862 0.997 0.961 0.982 0.977 | 22.3% | 19.1% |
| path-plus | 0.011 | 0.011 | 1.011 | +1.1% | 0.986 1.066 1.020 1.044 0.998 0.980 0.983 0.976 1.033 1.037 1.008 1.029 | 8.7% | 14.1% |
| predicate-counts | 0.005 | 0.004 | 0.929 | -7.1% | 0.937 0.989 0.942 0.959 0.832 0.918 0.939 0.849 0.955 0.911 0.951 0.927 | 36.5% | 14.9% |
| root-limit | 0.011 | 0.011 | 0.968 | -3.2% | 0.961 0.996 0.868 1.005 1.100 0.947 1.080 0.962 0.946 0.953 0.983 0.978 | 292.4% | 215.1% |
| root-scan | 0.087 | 0.085 | 0.975 | -2.5% | 0.979 0.972 0.971 0.971 0.972 0.994 0.981 0.987 0.966 0.975 0.980 0.963 | 4.5% | 3.9% |
| star-lookup | 0.061 | 0.061 | 1.002 | +0.2% | 1.014 1.009 1.024 1.017 0.948 0.991 1.001 1.014 0.990 0.994 1.012 1.002 | 14.1% | 5.9% |
| subquery-agg | 0.185 | 0.185 | 1.002 | +0.2% | 1.013 0.999 1.012 0.992 0.986 0.997 0.994 1.007 1.005 1.007 1.009 1.003 | 3.6% | 3.8% |
| values-star | 0.015 | 0.015 | 0.975 | -2.5% | 0.972 0.981 0.959 0.999 0.987 0.943 0.991 0.971 0.964 0.986 0.996 0.956 | 9.8% | 9.0% |
| **geometric mean** | | | 0.970 | -3.0% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| count-all | 0.975 | 0.976 | 1.001 | +0.1% | 0.922 1.042 1.007 1.018 1.000 1.000 1.028 1.001 1.001 1.013 1.024 1.000 | 98.8% | 5.4% |
| distinct-obj | 0.974 | 0.975 | 1.001 | +0.1% | 0.944 1.039 1.002 1.003 1.000 1.000 1.015 0.998 1.002 1.003 0.995 1.001 | 91.1% | 5.6% |
| path-plus | 0.975 | 0.975 | 1.000 | +0.0% | 1.001 1.002 1.027 1.031 1.001 0.999 1.001 1.000 1.001 1.001 1.000 0.999 | 1.2% | 5.0% |
| predicate-counts | 0.974 | 0.974 | 1.000 | +0.0% | 0.999 1.001 1.001 1.000 0.998 1.001 0.999 1.001 1.001 1.000 1.001 0.999 | 2.7% | 0.5% |
| root-limit | 2.034 | 2.034 | 1.000 | +0.0% | 1.000 1.000 1.000 0.999 1.000 1.000 1.000 1.000 1.000 1.000 1.000 1.001 | 0.3% | 0.1% |
| root-scan | 52.017 | 52.058 | 1.001 | +0.1% | 1.001 1.001 1.001 1.001 1.000 1.001 1.001 1.001 1.001 1.001 1.001 1.001 | 0.3% | 0.1% |
| star-lookup | 1.981 | 1.981 | 1.000 | +0.0% | 1.001 0.999 1.000 1.001 0.998 1.000 0.999 1.001 1.000 0.998 0.999 1.000 | 0.5% | 1.6% |
| subquery-agg | 1.971 | 1.971 | 1.000 | +0.0% | 1.001 1.000 1.001 1.001 0.999 1.001 1.001 1.001 1.000 0.999 1.001 0.999 | 0.2% | 0.2% |
| values-star | 1.976 | 1.976 | 1.000 | +0.0% | 1.001 1.000 1.000 0.999 0.999 0.994 0.997 1.007 1.000 0.999 1.000 1.000 | 3.8% | 4.0% |
| **geometric mean** | | | 1.000 | +0.0% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
