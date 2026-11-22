# Interleaved A/B on Namespace

- A: commit c0948014705b4bb40aaa7fe4c86c3c671444b7e9 (c0948014705b4bb40aaa7fe4c86c3c671444b7e9), built on forge
- A sha256: 73f156418609aab1ac7468db99ee284a19cf865957afcf3354587b9a9691c1fe
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit 3a8b7adbaac80b56db8fddc48c9e27d7ae471c10 (3a8b7adbaac80b56db8fddc48c9e27d7ae471c10), built on forge
- B sha256: e35fb61ff4ef6227511007774e8a41afab8367751cf37e17247221327fed0d3e
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

Load: A 0.78 s, B 0.59 s (B/A 0.761). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| bind-two | 65.778 | 65.789 | 1.000 | +0.0% | 0.993 1.107 1.000 0.980 0.997 | 23.6% | 20.2% |
| order-lcase | 23.065 | 23.588 | 1.023 | +2.3% | 1.029 0.987 0.943 0.965 1.186 | 51.6% | 93.7% |
| str-filter-two | 13.760 | 13.949 | 1.014 | +1.4% | 1.025 1.021 1.007 1.015 1.011 | 4.0% | 4.4% |
| str-pair-colleague | 4.634 | 4.935 | 1.065 | +6.5% | 1.086 1.038 1.016 1.088 1.076 | 8.3% | 15.9% |
| str-pair-lt | 6.164 | 5.762 | 0.935 | -6.5% | 0.949 0.949 0.934 0.942 0.924 | 4.5% | 5.4% |
| str-pair-names | 31.573 | 24.537 | 0.777 | -22.3% | 0.817 0.756 0.783 0.757 0.774 | 18.7% | 9.6% |
| **geometric mean** | | | 0.964 | -3.6% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| bind-two | 73.924 | 73.849 | 0.999 | -0.1% | 0.993 1.096 1.000 0.985 0.998 | 22.0% | 18.3% |
| order-lcase | 65.130 | 67.572 | 1.038 | +3.8% | 1.033 1.027 0.991 1.020 1.050 | 17.9% | 30.6% |
| str-filter-two | 16.746 | 17.041 | 1.018 | +1.8% | 1.020 1.014 1.016 1.007 1.021 | 4.1% | 4.4% |
| str-pair-colleague | 5.518 | 5.846 | 1.059 | +5.9% | 1.076 1.028 1.013 1.090 1.070 | 6.9% | 13.6% |
| str-pair-lt | 6.944 | 6.574 | 0.947 | -5.3% | 0.959 0.954 0.938 0.958 0.935 | 5.1% | 5.7% |
| str-pair-names | 32.559 | 25.683 | 0.789 | -21.1% | 0.828 0.770 0.795 0.767 0.788 | 18.5% | 8.9% |
| **geometric mean** | | | 0.970 | -3.0% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
