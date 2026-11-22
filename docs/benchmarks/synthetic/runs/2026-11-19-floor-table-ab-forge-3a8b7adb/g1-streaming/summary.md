# Interleaved A/B on Namespace

- A: commit c0948014705b4bb40aaa7fe4c86c3c671444b7e9 (c0948014705b4bb40aaa7fe4c86c3c671444b7e9), built on forge
- A sha256: 73f156418609aab1ac7468db99ee284a19cf865957afcf3354587b9a9691c1fe
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit 3a8b7adbaac80b56db8fddc48c9e27d7ae471c10 (3a8b7adbaac80b56db8fddc48c9e27d7ae471c10), built on forge
- B sha256: e35fb61ff4ef6227511007774e8a41afab8367751cf37e17247221327fed0d3e
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

Load: A 1.67 s, B 0.56 s (B/A 0.336). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| str-filter-two | 30.034 | 30.022 | 1.000 | -0.0% | 0.989 1.000 0.999 1.001 1.001 | 2.6% | 0.3% |
| str-pair-colleague | 9.012 | 9.394 | 1.042 | +4.2% | 1.030 1.044 1.047 1.041 1.044 | 2.8% | 1.4% |
| str-pair-lt | 13.148 | 10.720 | 0.815 | -18.5% | 0.813 0.817 0.818 0.813 0.816 | 1.8% | 0.7% |
| str-pair-names | 57.749 | 29.832 | 0.517 | -48.3% | 0.511 0.515 0.520 0.516 0.517 | 1.8% | 1.5% |
| **geometric mean** | | | 0.814 | -18.6% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| str-filter-two | 32.550 | 32.441 | 0.997 | -0.3% | 0.983 0.999 0.996 1.005 1.000 | 2.6% | 1.1% |
| str-pair-colleague | 10.377 | 10.745 | 1.036 | +3.6% | 1.027 1.045 1.022 1.036 1.039 | 5.7% | 1.9% |
| str-pair-lt | 14.448 | 11.895 | 0.823 | -17.7% | 0.814 0.837 0.824 0.824 0.822 | 2.7% | 2.1% |
| str-pair-names | 59.110 | 31.248 | 0.529 | -47.1% | 0.523 0.528 0.531 0.528 0.530 | 1.7% | 1.7% |
| **geometric mean** | | | 0.819 | -18.1% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
