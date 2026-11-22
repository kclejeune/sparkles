# Interleaved A/B on Namespace

- A: commit 174de1f893fbe8c1b76c9fdd6f738d44286a6cb7 (174de1f893fbe8c1b76c9fdd6f738d44286a6cb7), built on forge
- A sha256: 8fd86cc10678d79b2340931a304a5955a27f9f98e213b5accae9fd1e21005cfa
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit 3a8b7adbaac80b56db8fddc48c9e27d7ae471c10 (3a8b7adbaac80b56db8fddc48c9e27d7ae471c10), built on forge
- B sha256: e35fb61ff4ef6227511007774e8a41afab8367751cf37e17247221327fed0d3e
- B compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- data: 100000 people (scripts/gen-data.py), 6.0M compressed
- host: forge (ssh, nsc-bench-remote.sh run directly)
- settings: 3 queries, 5 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners 1, execution streaming
- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

A and B returned the same sorted TSV answers to all 3 queries.

Load: A 0.76 s, B 0.61 s (B/A 0.809). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| str-pair-colleague | 9.910 | 9.438 | 0.952 | -4.8% | 0.943 0.951 0.949 0.957 0.956 | 3.9% | 1.0% |
| str-pair-lt | 11.863 | 10.761 | 0.907 | -9.3% | 0.896 0.904 0.904 0.911 0.910 | 2.7% | 0.4% |
| str-pair-names | 28.523 | 30.041 | 1.053 | +5.3% | 1.050 1.059 1.056 1.050 1.060 | 2.2% | 1.9% |
| **geometric mean** | | | 0.969 | -3.1% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| str-pair-colleague | 11.288 | 10.791 | 0.956 | -4.4% | 0.931 0.954 0.960 0.962 0.964 | 7.2% | 2.4% |
| str-pair-lt | 13.085 | 11.953 | 0.913 | -8.7% | 0.898 0.906 0.912 0.925 0.923 | 4.7% | 1.7% |
| str-pair-names | 29.958 | 31.547 | 1.053 | +5.3% | 1.052 1.056 1.053 1.063 1.056 | 2.3% | 3.2% |
| **geometric mean** | | | 0.972 | -2.8% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
