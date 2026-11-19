# Interleaved A/B on Namespace

- A: commit c0948014705b4bb40aaa7fe4c86c3c671444b7e9 (c0948014705b4bb40aaa7fe4c86c3c671444b7e9), built on forge
- A sha256: 73f156418609aab1ac7468db99ee284a19cf865957afcf3354587b9a9691c1fe
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit 174de1f893fbe8c1b76c9fdd6f738d44286a6cb7 (174de1f893fbe8c1b76c9fdd6f738d44286a6cb7), built on forge
- B sha256: 8fd86cc10678d79b2340931a304a5955a27f9f98e213b5accae9fd1e21005cfa
- B compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- data: 100000 people (scripts/gen-data.py), 6.0M compressed
- host: forge (ssh, nsc-bench-remote.sh run directly)
- settings: 6 queries, 5 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server, RAYON_NUM_THREADS=12, server CPU 0-11, client CPU 12, spinners 0, execution streaming
- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

A and B returned the same sorted TSV answers to all 6 queries.

Load: A 0.92 s, B 0.79 s (B/A 0.860). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| bind-two | 64.148 | 64.348 | 1.003 | +0.3% | 1.036 0.992 0.992 1.009 1.020 | 7.2% | 7.4% |
| order-lcase | 22.187 | 21.857 | 0.985 | -1.5% | 0.807 1.007 0.974 1.043 1.014 | 34.2% | 15.2% |
| str-filter-two | 13.616 | 13.871 | 1.019 | +1.9% | 1.025 1.020 1.031 1.011 0.997 | 3.9% | 3.9% |
| str-pair-colleague | 4.558 | 5.098 | 1.118 | +11.8% | 1.171 1.100 1.094 1.116 1.113 | 11.8% | 7.6% |
| str-pair-lt | 6.183 | 5.808 | 0.939 | -6.1% | 0.936 0.929 0.940 0.960 0.928 | 3.4% | 4.2% |
| str-pair-names | 31.117 | 31.116 | 1.000 | -0.0% | 1.064 1.012 0.998 0.995 0.990 | 7.9% | 15.5% |
| **geometric mean** | | | 1.009 | +0.9% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| bind-two | 72.366 | 72.336 | 1.000 | -0.0% | 1.034 0.993 0.995 1.003 1.014 | 6.7% | 6.7% |
| order-lcase | 63.086 | 63.039 | 0.999 | -0.1% | 0.943 1.023 0.982 1.025 1.004 | 11.9% | 9.2% |
| str-filter-two | 16.675 | 17.016 | 1.020 | +2.0% | 1.031 1.011 1.038 1.012 1.002 | 5.4% | 4.1% |
| str-pair-colleague | 5.460 | 6.043 | 1.107 | +10.7% | 1.145 1.096 1.095 1.133 1.102 | 10.8% | 7.3% |
| str-pair-lt | 6.986 | 6.682 | 0.956 | -4.4% | 0.958 0.939 0.951 0.973 0.946 | 3.7% | 3.5% |
| str-pair-names | 32.045 | 32.139 | 1.003 | +0.3% | 1.068 1.017 1.002 0.999 0.994 | 7.5% | 15.2% |
| **geometric mean** | | | 1.013 | +1.3% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
