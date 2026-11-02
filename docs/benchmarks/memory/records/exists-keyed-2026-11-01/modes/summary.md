# Interleaved A/B on Namespace

- A: commit ec6cd3269fd0314245cfa333e29dc771a364e025 (ec6cd326)
- A sha256: a71374924e502753e3608b8c434d2465b340076a8422214b09a6a158febde660
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit ec6cd3269fd0314245cfa333e29dc771a364e025 (ec6cd326)
- B sha256: a71374924e502753e3608b8c434d2465b340076a8422214b09a6a158febde660
- B compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- data: 1000000 people (scripts/gen-data.py), 63M compressed
- settings: 6 queries, 3 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners on, execution eager,streaming
- instance: ojscsm6dlmjas (linux/amd64:8x16, capped at 90m), https://cloud.namespace.so/itohmrcedlsbs/instance/ojscsm6dlmjas
- phase create: 3.3 s
- phase upload: 11.4 s for 422M
- phase remote run: 803.5 s
- phase download: 1.4 s
- instance lifetime: 820.0 s, billed as 14 minutes x 8 vCPUs = 112 vCPU-minutes ($0.168 at the overage rate)
- kernel: 7.1.4
- cpu: AMD EPYC
- cpus: 8
- memory: 16052 MiB
- smt siblings: 0-1 2-3 4-5 6-7 
- loader: ld.so (glibc-2.44-r10) stable release version 2.44.
- steal during the timed part: 0.01%

A and B returned the same sorted TSV answers to all 6 queries.

Load: A 4.52 s, B 4.13 s (B/A 0.915). Store size: A 298 MiB, B 298 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| exists-bind-join | 7.907 | 7.286 | 0.921 | -7.9% | 0.778 0.975 0.945 | 40.8% | 18.0% |
| exists-bind | 8.787 | 7.724 | 0.879 | -12.1% | 0.742 0.863 0.976 | 50.2% | 45.9% |
| exists-if | 4245.130 | 4982.094 | 1.174 | +17.4% | 1.137 1.203 1.152 | 13.5% | 16.0% |
| exists-optional | 12.536 | 57.360 | 4.576 | +357.6% | 5.581 4.776 4.334 | 10.4% | 127.0% |
| not-exists | 7.369 | 7.656 | 1.039 | +3.9% | 1.527 1.070 0.921 | 17.3% | 175.5% |
| optional-filter | 12.285 | 45.852 | 3.732 | +273.2% | 4.848 4.123 3.352 | 10.6% | 108.1% |
| **geometric mean** | | | 1.601 | +60.1% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| exists-bind-join | 8.984 | 8.425 | 0.938 | -6.2% | 0.768 0.985 0.977 | 44.9% | 21.8% |
| exists-bind | 9.718 | 8.726 | 0.898 | -10.2% | 0.744 0.868 0.994 | 55.1% | 49.0% |
| exists-if | 4246.830 | 4994.255 | 1.176 | +17.6% | 1.139 1.205 1.154 | 13.6% | 16.1% |
| exists-optional | 13.571 | 58.565 | 4.315 | +331.5% | 5.249 4.502 4.081 | 13.3% | 125.1% |
| not-exists | 8.257 | 8.824 | 1.069 | +6.9% | 1.579 1.103 0.933 | 19.9% | 163.0% |
| optional-filter | 13.401 | 47.035 | 3.510 | +251.0% | 4.603 3.882 3.172 | 12.3% | 106.1% |
| **geometric mean** | | | 1.588 | +58.8% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
