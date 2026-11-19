# Interleaved A/B on Namespace

- A: commit 7e272f4dabc6b1b3e5f001690573d90ba07afcba (7e272f4dabc6b1b3e5f001690573d90ba07afcba), built on forge
- A sha256: 7d61a0f0bb0e3eccac9ab4fdff5a57bd574cf545679b283d720824def161344f
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit 75a70587acbbbc026f91d279dbabaad1003ab278 (75a70587acbbbc026f91d279dbabaad1003ab278), built on forge
- B sha256: b7bde5647399858f47d190a1c8df51eeeadf2c53195d2bf99e4220b075e3b08f
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

Load: A 0.74 s, B 0.56 s (B/A 0.755). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| distinct-join | 3.794 | 3.866 | 1.019 | +1.9% | 1.018 1.022 1.019 1.018 1.019 | 0.8% | 0.4% |
| employee-docs | 0.038 | 0.039 | 1.013 | +1.3% | 0.995 1.031 0.969 1.018 0.979 | 14.9% | 10.6% |
| lang-filter | 1.791 | 1.839 | 1.027 | +2.7% | 1.031 1.030 1.029 1.032 1.013 | 1.9% | 1.5% |
| str-filter-two | 29.737 | 30.284 | 1.018 | +1.8% | 1.020 1.020 1.019 1.017 1.015 | 0.6% | 0.7% |
| **geometric mean** | | | 1.019 | +1.9% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| distinct-join | 5.977 | 5.977 | 1.000 | +0.0% | 1.000 1.000 1.000 0.999 1.000 | 0.3% | 0.1% |
| employee-docs | 1.980 | 1.979 | 0.999 | -0.1% | 1.000 0.987 1.015 1.000 1.001 | 3.6% | 2.4% |
| lang-filter | 2.958 | 2.975 | 1.006 | +0.6% | 1.010 1.006 1.025 1.027 0.991 | 7.1% | 8.0% |
| str-filter-two | 32.951 | 32.986 | 1.001 | +0.1% | 1.000 1.030 1.000 1.010 1.003 | 1.3% | 3.1% |
| **geometric mean** | | | 1.002 | +0.2% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
