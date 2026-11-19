# Interleaved A/B on Namespace

- A: commit 4e9185f15e1397660651dad90c7817be1beef91e (4e9185f1), built on wally
- A sha256: 3ba8fd687308e297e5056a7d69e7cda65b0264b5a57e7e5d2a4a1c284bcc9dc3
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit c0948014705b4bb40aaa7fe4c86c3c671444b7e9 (c0948014), built on wally
- B sha256: 1b04d43f6bb91343a5de3204f366d973b976851dab1a44f94350bd0bd065edff
- B compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- data: 100000 people (scripts/gen-data.py), 6.0M compressed
- host: atlas (ssh, nsc-bench-remote.sh run directly)
- settings: 1 queries, 12 rounds of A,B,B,A, 3 warm-up and 20 timed requests per query and server, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners 1, execution auto
- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

A and B returned the same sorted TSV answers to all 1 queries.

Load: A 0.69 s, B 0.65 s (B/A 0.948). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| export-500k | 0.394 | 0.401 | 1.018 | +1.8% | 1.018 1.027 1.021 1.024 1.016 1.015 1.011 1.024 1.016 1.014 1.010 1.022 | 2.1% | 3.6% |
| **geometric mean** | | | 1.018 | +1.8% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| export-500k | 358.995 | 358.053 | 0.997 | -0.3% | 1.000 0.997 1.000 0.995 1.005 1.000 0.997 0.997 0.997 0.999 1.000 0.997 | 0.4% | 0.9% |
| **geometric mean** | | | 0.997 | -0.3% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
