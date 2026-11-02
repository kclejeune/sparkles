# Interleaved A/B on Namespace

- A: commit 4bd708458020f7db0fea5783953fc50f313109fe (4bd70845)
- A sha256: b5b63440c3c389066c0ccb9cf4f216cc8bbb792317bf3fbfbaf2641935996892
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit ec6cd3269fd0314245cfa333e29dc771a364e025 (ec6cd326)
- B sha256: a71374924e502753e3608b8c434d2465b340076a8422214b09a6a158febde660
- B compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- data: 1000000 people (scripts/gen-data.py), 63M compressed
- settings: 6 queries, 3 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners on
- instance: luul2vjeqr2co (linux/amd64:8x16, capped at 90m), https://cloud.namespace.so/itohmrcedlsbs/instance/luul2vjeqr2co
- phase create: 3.2 s
- phase upload: 9.3 s for 422M
- phase remote run: 1075.1 s
- phase download: 1.2 s
- instance lifetime: 1089.2 s, billed as 19 minutes x 8 vCPUs = 152 vCPU-minutes ($0.228 at the overage rate)
- kernel: 7.1.4
- cpu: AMD EPYC
- cpus: 8
- memory: 16052 MiB
- smt siblings: 0-1 2-3 4-5 6-7 
- loader: ld.so (glibc-2.44-r10) stable release version 2.44.
- steal during the timed part: 0.01%

A and B returned the same sorted TSV answers to all 6 queries.

Load: A 3.93 s, B 3.64 s (B/A 0.927). Store size: A 298 MiB, B 298 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| exists-bind-join | 2003.353 | 7.420 | 0.004 | -99.6% | 0.004 0.004 0.004 | 15.5% | 27.8% |
| exists-bind | 860.138 | 8.376 | 0.010 | -99.0% | 0.009 0.011 0.010 | 9.8% | 41.0% |
| exists-if | 4353.854 | 4363.939 | 1.002 | +0.2% | 0.983 1.038 1.012 | 17.2% | 15.1% |
| exists-optional | 1078.716 | 12.850 | 0.012 | -98.8% | 0.012 0.013 0.010 | 75.4% | 18.3% |
| not-exists | 7.885 | 7.490 | 0.950 | -5.0% | 0.959 1.070 0.729 | 98.4% | 32.0% |
| optional-filter | 41.250 | 40.828 | 0.990 | -1.0% | 0.971 1.313 0.819 | 180.8% | 108.6% |
| **geometric mean** | | | 0.086 | -91.4% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| exists-bind-join | 2004.630 | 8.322 | 0.004 | -99.6% | 0.004 0.005 0.004 | 15.5% | 31.6% |
| exists-bind | 861.578 | 9.306 | 0.011 | -98.9% | 0.010 0.012 0.011 | 9.8% | 43.6% |
| exists-if | 4355.035 | 4364.500 | 1.002 | +0.2% | 0.983 1.038 1.012 | 17.3% | 15.1% |
| exists-optional | 1080.190 | 13.985 | 0.013 | -98.7% | 0.013 0.014 0.010 | 75.6% | 19.6% |
| not-exists | 8.971 | 8.422 | 0.939 | -6.1% | 0.957 1.064 0.694 | 117.7% | 37.8% |
| optional-filter | 42.386 | 42.022 | 0.991 | -0.9% | 0.971 1.302 0.822 | 178.5% | 107.2% |
| **geometric mean** | | | 0.090 | -91.0% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
