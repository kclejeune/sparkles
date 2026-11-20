# Interleaved A/B on Namespace

- A: commit c0948014705b4bb40aaa7fe4c86c3c671444b7e9 (c0948014705b4bb40aaa7fe4c86c3c671444b7e9), built on forge
- A sha256: 73f156418609aab1ac7468db99ee284a19cf865957afcf3354587b9a9691c1fe
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit 3a8b7adbaac80b56db8fddc48c9e27d7ae471c10 (3a8b7adbaac80b56db8fddc48c9e27d7ae471c10), built on forge
- B sha256: e35fb61ff4ef6227511007774e8a41afab8367751cf37e17247221327fed0d3e
- B compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- data: 100000 people (scripts/gen-data.py), 6.0M compressed
- host: forge (ssh, nsc-bench-remote.sh run directly)
- settings: 6 queries, 5 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server, RAYON_NUM_THREADS=12, server CPU 0-11, client CPU 12, spinners 0, execution eager
- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

A and B returned the same sorted TSV answers to all 6 queries.

Load: A 0.61 s, B 0.62 s (B/A 1.013). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| bind-two | 73.214 | 72.611 | 0.992 | -0.8% | 1.001 1.020 0.949 0.974 0.979 | 9.9% | 8.3% |
| order-lcase | 22.705 | 23.555 | 1.037 | +3.7% | 0.964 1.157 1.039 1.094 0.962 | 35.2% | 33.9% |
| str-filter-two | 10.523 | 10.434 | 0.991 | -0.9% | 1.012 0.989 1.012 0.954 0.997 | 24.4% | 9.1% |
| str-pair-colleague | 5.296 | 4.249 | 0.802 | -19.8% | 0.787 0.818 0.827 0.816 0.775 | 7.7% | 7.8% |
| str-pair-lt | 5.406 | 4.557 | 0.843 | -15.7% | 0.858 0.839 0.811 0.876 0.844 | 11.2% | 4.1% |
| str-pair-names | 19.658 | 12.502 | 0.636 | -36.4% | 0.633 0.680 0.640 0.641 0.604 | 29.1% | 10.9% |
| **geometric mean** | | | 0.872 | -12.8% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| bind-two | 77.598 | 76.894 | 0.991 | -0.9% | 0.998 1.018 0.956 0.973 0.980 | 9.3% | 8.0% |
| order-lcase | 56.666 | 57.816 | 1.020 | +2.0% | 1.001 1.064 1.027 1.036 0.907 | 30.6% | 13.6% |
| str-filter-two | 11.601 | 11.526 | 0.994 | -0.6% | 1.006 0.991 1.012 0.958 1.002 | 23.1% | 8.4% |
| str-pair-colleague | 6.170 | 5.071 | 0.822 | -17.8% | 0.801 0.834 0.843 0.840 0.793 | 7.1% | 7.5% |
| str-pair-lt | 6.250 | 5.365 | 0.858 | -14.2% | 0.868 0.852 0.828 0.890 0.854 | 9.5% | 3.5% |
| str-pair-names | 20.916 | 13.589 | 0.650 | -35.0% | 0.645 0.696 0.657 0.653 0.618 | 28.9% | 8.9% |
| **geometric mean** | | | 0.879 | -12.1% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
