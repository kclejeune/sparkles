# Interleaved A/B on Namespace

- A: commit 971717d245d6722b180cc58f310f5ab8d1fe90e0 (971717d245d6722b180cc58f310f5ab8d1fe90e0), built on forge
- A sha256: e209ce2c5653d4ffba4fcff4f9ceb341be2fee7d381b5bef236faa1afe67f478
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit 174de1f893fbe8c1b76c9fdd6f738d44286a6cb7 (174de1f893fbe8c1b76c9fdd6f738d44286a6cb7), built on forge
- B sha256: 8fd86cc10678d79b2340931a304a5955a27f9f98e213b5accae9fd1e21005cfa
- B compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- data: 100000 people (scripts/gen-data.py), 6.0M compressed
- host: forge (ssh, nsc-bench-remote.sh run directly)
- settings: 4 queries, 5 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners 1, execution eager
- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

A and B returned the same sorted TSV answers to all 4 queries.

Load: A 1.79 s, B 0.72 s (B/A 0.402). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| str-filter-two | 24.865 | 24.963 | 1.004 | +0.4% | 1.011 1.003 1.002 1.003 1.004 | 0.8% | 1.8% |
| str-pair-colleague | 9.009 | 9.283 | 1.030 | +3.0% | 1.032 1.028 1.032 1.029 1.031 | 0.9% | 1.4% |
| str-pair-lt | 13.295 | 11.334 | 0.852 | -14.8% | 0.847 0.849 0.853 0.857 0.849 | 3.8% | 3.7% |
| str-pair-names | 57.174 | 26.285 | 0.460 | -54.0% | 0.461 0.456 0.461 0.460 0.461 | 1.1% | 1.2% |
| **geometric mean** | | | 0.798 | -20.2% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| str-filter-two | 26.151 | 26.328 | 1.007 | +0.7% | 1.013 1.003 1.004 1.008 1.006 | 0.7% | 2.0% |
| str-pair-colleague | 10.284 | 10.571 | 1.028 | +2.8% | 1.035 1.010 1.033 1.062 0.997 | 6.0% | 5.8% |
| str-pair-lt | 14.587 | 12.627 | 0.866 | -13.4% | 0.865 0.875 0.867 0.865 0.849 | 4.1% | 3.3% |
| str-pair-names | 58.582 | 27.632 | 0.472 | -52.8% | 0.475 0.467 0.475 0.474 0.474 | 1.2% | 2.2% |
| **geometric mean** | | | 0.806 | -19.4% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
