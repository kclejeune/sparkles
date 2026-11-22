# Interleaved A/B on Namespace

- A: commit 4e9185f15e1397660651dad90c7817be1beef91e (4e9185f1), built on wally
- A sha256: 3ba8fd687308e297e5056a7d69e7cda65b0264b5a57e7e5d2a4a1c284bcc9dc3
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit c0948014705b4bb40aaa7fe4c86c3c671444b7e9 (c0948014), built on wally
- B sha256: 1b04d43f6bb91343a5de3204f366d973b976851dab1a44f94350bd0bd065edff
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

Load: A 0.79 s, B 0.60 s (B/A 0.752). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| count-all | 0.005 | 0.004 | 0.971 | -2.9% | 1.025 0.968 0.998 0.996 0.978 0.954 0.918 0.930 0.974 0.961 0.972 1.008 | 16.9% | 12.8% |
| distinct-obj | 0.004 | 0.004 | 0.993 | -0.7% | 1.039 1.036 1.005 0.976 1.013 1.012 0.921 0.946 0.993 0.989 1.009 1.041 | 19.7% | 16.6% |
| path-plus | 0.011 | 0.011 | 1.030 | +3.0% | 1.025 1.040 0.986 1.051 1.030 1.004 1.024 1.044 1.018 1.038 1.020 1.082 | 11.2% | 10.9% |
| predicate-counts | 0.005 | 0.005 | 0.964 | -3.6% | 0.969 0.987 0.963 0.965 0.952 0.998 0.905 0.935 0.959 0.956 0.974 0.996 | 13.1% | 12.2% |
| root-limit | 0.011 | 0.012 | 1.050 | +5.0% | 1.173 1.006 1.267 0.884 2.985 1.001 1.043 1.002 0.983 1.204 1.018 1.218 | 219.0% | 191.9% |
| root-scan | 0.085 | 0.087 | 1.030 | +3.0% | 1.036 1.034 1.039 1.027 1.026 1.029 1.033 1.031 1.017 1.032 1.029 1.027 | 2.2% | 2.3% |
| star-lookup | 0.060 | 0.062 | 1.022 | +2.2% | 1.036 1.016 1.014 1.007 1.028 1.043 1.022 1.011 1.035 1.027 1.002 1.018 | 4.3% | 9.5% |
| subquery-agg | 0.185 | 0.184 | 0.994 | -0.6% | 1.007 0.998 0.997 0.994 0.990 0.992 0.999 0.977 1.000 0.998 0.978 0.999 | 2.8% | 3.7% |
| values-star | 0.015 | 0.016 | 1.044 | +4.4% | 1.039 1.033 1.034 1.046 1.053 1.035 1.046 1.051 1.056 1.045 1.039 1.039 | 6.6% | 6.0% |
| **geometric mean** | | | 1.011 | +1.1% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| count-all | 0.974 | 0.974 | 1.000 | +0.0% | 0.997 0.998 0.980 1.000 1.002 1.000 0.999 1.001 1.001 1.000 0.999 1.000 | 4.2% | 4.1% |
| distinct-obj | 0.975 | 0.974 | 0.999 | -0.1% | 0.996 1.027 1.000 0.999 0.999 0.998 0.998 0.999 1.000 0.999 0.999 0.998 | 6.2% | 6.5% |
| path-plus | 0.976 | 0.975 | 0.999 | -0.1% | 0.985 0.996 0.999 1.000 1.000 0.999 0.999 0.998 1.000 0.999 1.000 0.999 | 4.8% | 1.1% |
| predicate-counts | 0.974 | 0.974 | 1.000 | +0.0% | 0.999 1.000 1.000 1.000 0.997 1.001 1.000 0.999 1.001 1.000 0.999 1.000 | 0.3% | 1.2% |
| root-limit | 2.034 | 2.034 | 1.000 | +0.0% | 1.000 0.999 1.000 1.000 1.000 1.000 1.000 1.000 1.001 1.000 1.000 1.000 | 0.4% | 0.3% |
| root-scan | 52.057 | 52.019 | 0.999 | -0.1% | 0.999 0.999 0.999 0.999 0.999 0.999 0.999 0.999 0.999 0.999 0.999 0.999 | 0.1% | 0.1% |
| star-lookup | 1.982 | 1.982 | 1.000 | +0.0% | 1.000 1.001 0.999 1.001 0.999 1.001 0.999 1.001 0.999 1.000 0.998 1.000 | 0.4% | 0.2% |
| subquery-agg | 1.971 | 1.971 | 1.000 | +0.0% | 1.001 1.000 0.999 1.001 0.999 1.000 1.000 1.001 0.998 1.001 0.999 1.001 | 0.3% | 0.3% |
| values-star | 1.976 | 1.976 | 1.000 | +0.0% | 1.001 1.000 0.998 1.001 1.001 1.002 1.001 1.000 1.000 1.001 0.999 0.987 | 3.6% | 1.7% |
| **geometric mean** | | | 1.000 | -0.0% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
