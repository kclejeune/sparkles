# Interleaved A/B on Namespace

- A: commit c0948014705b4bb40aaa7fe4c86c3c671444b7e9 (c0948014705b4bb40aaa7fe4c86c3c671444b7e9), built on forge
- A sha256: 73f156418609aab1ac7468db99ee284a19cf865957afcf3354587b9a9691c1fe
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit 174de1f893fbe8c1b76c9fdd6f738d44286a6cb7 (174de1f893fbe8c1b76c9fdd6f738d44286a6cb7), built on forge
- B sha256: 8fd86cc10678d79b2340931a304a5955a27f9f98e213b5accae9fd1e21005cfa
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

Load: A 0.56 s, B 0.56 s (B/A 1.009). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| bind-two | 72.556 | 73.314 | 1.010 | +1.0% | 1.048 0.995 0.989 1.051 1.012 | 9.5% | 8.4% |
| order-lcase | 23.385 | 22.871 | 0.978 | -2.2% | 1.100 0.936 1.054 0.981 0.963 | 23.4% | 40.2% |
| str-filter-two | 10.588 | 10.586 | 1.000 | -0.0% | 0.986 1.006 0.998 1.012 0.979 | 33.4% | 20.9% |
| str-pair-colleague | 5.180 | 4.560 | 0.880 | -12.0% | 0.881 0.912 0.871 0.857 0.889 | 8.2% | 8.5% |
| str-pair-lt | 5.401 | 4.566 | 0.846 | -15.4% | 0.896 0.835 0.834 0.856 0.805 | 11.9% | 11.1% |
| str-pair-names | 19.930 | 16.607 | 0.833 | -16.7% | 0.801 0.912 0.823 0.847 0.819 | 11.0% | 10.0% |
| **geometric mean** | | | 0.922 | -7.8% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| bind-two | 76.790 | 77.568 | 1.010 | +1.0% | 1.046 0.998 0.991 1.049 1.010 | 9.2% | 7.7% |
| order-lcase | 58.151 | 56.942 | 0.979 | -2.1% | 1.035 0.993 0.983 0.979 0.987 | 11.7% | 16.4% |
| str-filter-two | 11.651 | 11.646 | 1.000 | -0.0% | 0.978 1.003 1.001 1.019 0.970 | 30.4% | 19.7% |
| str-pair-colleague | 6.022 | 5.403 | 0.897 | -10.3% | 0.889 0.920 0.886 0.879 0.901 | 8.0% | 6.1% |
| str-pair-lt | 6.277 | 5.418 | 0.863 | -13.7% | 0.915 0.862 0.854 0.863 0.827 | 10.6% | 10.7% |
| str-pair-names | 21.160 | 17.814 | 0.842 | -15.8% | 0.810 0.917 0.836 0.855 0.830 | 9.8% | 9.7% |
| **geometric mean** | | | 0.929 | -7.1% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
