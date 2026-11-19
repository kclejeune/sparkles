# Interleaved A/B on Namespace

- A: commit 971717d245d6722b180cc58f310f5ab8d1fe90e0 (971717d245d6722b180cc58f310f5ab8d1fe90e0), built on forge
- A sha256: e209ce2c5653d4ffba4fcff4f9ceb341be2fee7d381b5bef236faa1afe67f478
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit 3a8b7adbaac80b56db8fddc48c9e27d7ae471c10 (3a8b7adbaac80b56db8fddc48c9e27d7ae471c10), built on forge
- B sha256: e35fb61ff4ef6227511007774e8a41afab8367751cf37e17247221327fed0d3e
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

Load: A 0.57 s, B 0.60 s (B/A 1.038). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| str-filter-two | 25.003 | 24.982 | 0.999 | -0.1% | 0.998 1.002 0.996 1.000 0.999 | 1.1% | 1.1% |
| str-pair-colleague | 8.986 | 8.904 | 0.991 | -0.9% | 0.992 0.989 0.991 0.989 0.986 | 1.1% | 1.2% |
| str-pair-lt | 13.252 | 10.313 | 0.778 | -22.2% | 0.771 0.777 0.783 0.781 0.780 | 0.9% | 2.6% |
| str-pair-names | 57.326 | 29.368 | 0.512 | -48.8% | 0.515 0.513 0.511 0.512 0.511 | 0.5% | 1.4% |
| **geometric mean** | | | 0.793 | -20.7% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| str-filter-two | 26.415 | 26.368 | 0.998 | -0.2% | 0.997 0.997 1.000 1.001 0.997 | 1.1% | 0.9% |
| str-pair-colleague | 10.271 | 10.163 | 0.990 | -1.0% | 0.987 0.983 0.996 0.988 0.987 | 7.4% | 5.1% |
| str-pair-lt | 14.537 | 11.677 | 0.803 | -19.7% | 0.792 0.816 0.799 0.808 0.804 | 2.8% | 3.7% |
| str-pair-names | 58.738 | 30.747 | 0.523 | -47.7% | 0.527 0.525 0.521 0.522 0.522 | 0.8% | 1.4% |
| **geometric mean** | | | 0.803 | -19.7% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
