# Interleaved A/B on Namespace

- A: commit 7e272f4dabc6b1b3e5f001690573d90ba07afcba (7e272f4dabc6b1b3e5f001690573d90ba07afcba), built on forge
- A sha256: 7d61a0f0bb0e3eccac9ab4fdff5a57bd574cf545679b283d720824def161344f
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit 75a70587acbbbc026f91d279dbabaad1003ab278 (75a70587acbbbc026f91d279dbabaad1003ab278), built on forge
- B sha256: b7bde5647399858f47d190a1c8df51eeeadf2c53195d2bf99e4220b075e3b08f
- B compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- data: 100000 people (scripts/gen-data.py), 6.0M compressed
- host: forge (ssh, nsc-bench-remote.sh run directly)
- settings: 4 queries, 5 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners 1, execution auto
- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

A and B returned the same sorted TSV answers to all 4 queries.

Load: A 0.63 s, B 0.55 s (B/A 0.875). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| distinct-join | 3.592 | 3.569 | 0.994 | -0.6% | 0.991 0.998 0.991 0.994 0.998 | 1.1% | 1.5% |
| employee-docs | 0.038 | 0.038 | 1.002 | +0.2% | 1.010 0.995 0.986 1.004 1.035 | 7.9% | 8.6% |
| lang-filter | 1.793 | 1.845 | 1.029 | +2.9% | 1.031 1.023 1.029 1.029 1.032 | 2.0% | 1.1% |
| str-filter-two | 24.600 | 25.109 | 1.021 | +2.1% | 1.021 1.020 1.019 1.020 1.020 | 0.8% | 0.3% |
| **geometric mean** | | | 1.011 | +1.1% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| distinct-join | 5.225 | 5.239 | 1.003 | +0.3% | 0.992 1.004 1.000 1.010 1.015 | 4.7% | 2.8% |
| employee-docs | 1.974 | 1.973 | 0.999 | -0.1% | 0.813 1.007 1.027 0.969 1.145 | 35.4% | 24.7% |
| lang-filter | 2.943 | 2.987 | 1.015 | +1.5% | 1.011 0.925 1.028 1.064 1.000 | 11.8% | 8.9% |
| str-filter-two | 26.002 | 26.463 | 1.018 | +1.8% | 1.021 1.017 1.020 1.014 1.018 | 1.1% | 0.6% |
| **geometric mean** | | | 1.009 | +0.9% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
