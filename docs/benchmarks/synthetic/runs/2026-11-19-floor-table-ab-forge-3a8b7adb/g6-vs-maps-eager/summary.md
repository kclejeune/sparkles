# Interleaved A/B on Namespace

- A: commit 174de1f893fbe8c1b76c9fdd6f738d44286a6cb7 (174de1f893fbe8c1b76c9fdd6f738d44286a6cb7), built on forge
- A sha256: 8fd86cc10678d79b2340931a304a5955a27f9f98e213b5accae9fd1e21005cfa
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit 3a8b7adbaac80b56db8fddc48c9e27d7ae471c10 (3a8b7adbaac80b56db8fddc48c9e27d7ae471c10), built on forge
- B sha256: e35fb61ff4ef6227511007774e8a41afab8367751cf37e17247221327fed0d3e
- B compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- data: 100000 people (scripts/gen-data.py), 6.0M compressed
- host: forge (ssh, nsc-bench-remote.sh run directly)
- settings: 3 queries, 5 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners 1, execution eager
- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

A and B returned the same sorted TSV answers to all 3 queries.

Load: A 1.02 s, B 0.97 s (B/A 0.951). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| str-pair-colleague | 9.603 | 8.937 | 0.931 | -6.9% | 0.937 0.928 0.938 0.942 0.933 | 6.8% | 5.1% |
| str-pair-lt | 11.281 | 10.243 | 0.908 | -9.2% | 0.897 0.900 0.910 0.912 0.905 | 6.2% | 0.9% |
| str-pair-names | 27.030 | 29.651 | 1.097 | +9.7% | 1.083 1.099 1.112 1.095 1.076 | 3.5% | 3.0% |
| **geometric mean** | | | 0.975 | -2.5% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| str-pair-colleague | 10.838 | 10.189 | 0.940 | -6.0% | 0.955 0.919 0.940 0.955 0.932 | 6.7% | 8.2% |
| str-pair-lt | 12.549 | 11.498 | 0.916 | -8.4% | 0.907 0.909 0.924 0.918 0.912 | 7.4% | 2.5% |
| str-pair-names | 28.492 | 31.063 | 1.090 | +9.0% | 1.076 1.093 1.110 1.085 1.075 | 3.2% | 2.6% |
| **geometric mean** | | | 0.979 | -2.1% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
