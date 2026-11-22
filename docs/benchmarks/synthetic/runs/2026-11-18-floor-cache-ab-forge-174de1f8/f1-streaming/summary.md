# Interleaved A/B on Namespace

- A: commit c0948014705b4bb40aaa7fe4c86c3c671444b7e9 (c0948014705b4bb40aaa7fe4c86c3c671444b7e9), built on forge
- A sha256: 73f156418609aab1ac7468db99ee284a19cf865957afcf3354587b9a9691c1fe
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit 174de1f893fbe8c1b76c9fdd6f738d44286a6cb7 (174de1f893fbe8c1b76c9fdd6f738d44286a6cb7), built on forge
- B sha256: 8fd86cc10678d79b2340931a304a5955a27f9f98e213b5accae9fd1e21005cfa
- B compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- data: 100000 people (scripts/gen-data.py), 6.0M compressed
- host: forge (ssh, nsc-bench-remote.sh run directly)
- settings: 4 queries, 5 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners 1, execution streaming
- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

A and B returned the same sorted TSV answers to all 4 queries.

Load: A 0.58 s, B 0.64 s (B/A 1.112). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| str-filter-two | 30.013 | 30.844 | 1.028 | +2.8% | 1.025 1.028 1.028 1.028 1.028 | 1.0% | 0.4% |
| str-pair-colleague | 9.012 | 9.800 | 1.087 | +8.7% | 1.082 1.090 1.091 1.082 1.090 | 2.0% | 0.8% |
| str-pair-lt | 13.161 | 11.830 | 0.899 | -10.1% | 0.894 0.899 0.903 0.896 0.902 | 1.2% | 1.6% |
| str-pair-names | 57.690 | 28.148 | 0.488 | -51.2% | 0.485 0.485 0.492 0.486 0.493 | 1.2% | 2.5% |
| **geometric mean** | | | 0.837 | -16.3% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| str-filter-two | 32.602 | 33.356 | 1.023 | +2.3% | 1.015 1.027 1.024 1.021 1.028 | 1.7% | 0.7% |
| str-pair-colleague | 10.366 | 11.175 | 1.078 | +7.8% | 1.066 1.077 1.074 1.076 1.099 | 4.4% | 1.1% |
| str-pair-lt | 14.465 | 13.159 | 0.910 | -9.0% | 0.909 0.899 0.915 0.908 0.912 | 1.2% | 1.6% |
| str-pair-names | 59.072 | 29.767 | 0.504 | -49.6% | 0.498 0.500 0.505 0.504 0.507 | 1.6% | 2.9% |
| **geometric mean** | | | 0.843 | -15.7% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
