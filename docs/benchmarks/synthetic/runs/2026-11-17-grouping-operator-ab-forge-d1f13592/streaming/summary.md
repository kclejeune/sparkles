# Interleaved A/B on Namespace

- A: commit 7e272f4dabc6b1b3e5f001690573d90ba07afcba (7e272f4d), built on wally
- A sha256: f0090fa03425eb22765931c552c74ab41d35448e95aebdf4f64f1100e2984e8f
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit d1f13592c5b2dbc3bcb4f560ce7235505fbc2af8 (d1f13592), built on wally
- B sha256: c94b4df89f5a4a16b2f5c02bc9fe67e9e87356c75ac0628e186bf417feb03697
- B compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- data: 100000 people (scripts/gen-data.py)
- host: forge (ssh, nsc-bench-remote.sh run directly)
- settings: 6 queries, 12 rounds of A,B,B,A, 3 warm-up and 200 timed requests per query and server, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners on, execution streaming
- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

A and B returned the same sorted TSV answers to all 6 queries.

Load: A 0.63 s, B 0.53 s (B/A 0.843). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| expr-agg-arg | 1.927 | 1.934 | 1.004 | +0.4% | 1.009 1.006 0.999 1.001 1.007 1.005 1.003 1.006 1.004 1.007 1.003 0.975 | 4.5% | 2.2% |
| expr-bind-group | 0.369 | 0.360 | 0.975 | -2.5% | 0.979 0.973 0.965 0.979 0.976 0.972 0.981 0.976 0.975 0.979 0.975 0.900 | 10.7% | 3.3% |
| group-avg | 3.244 | 3.178 | 0.980 | -2.0% | 0.981 0.978 0.979 0.977 0.982 0.978 0.980 0.980 0.981 0.983 0.982 0.960 | 2.9% | 1.5% |
| predicate-counts | 0.007 | 0.007 | 1.021 | +2.1% | 1.065 1.013 0.973 1.120 1.131 1.020 1.020 1.020 1.012 0.989 1.038 0.675 | 84.5% | 26.4% |
| subquery-agg | 0.183 | 0.182 | 0.995 | -0.5% | 1.010 0.989 0.960 0.992 1.004 0.996 0.999 1.012 0.983 0.992 1.011 0.839 | 25.5% | 9.6% |
| types-grouped | 0.013 | 0.013 | 0.987 | -1.3% | 1.034 0.977 0.939 1.050 1.061 0.976 0.992 0.993 0.943 0.981 0.999 0.698 | 77.4% | 59.5% |
| **geometric mean** | | | 0.993 | -0.7% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| expr-agg-arg | 3.067 | 3.064 | 0.999 | -0.1% | 1.009 1.037 0.972 0.998 1.005 1.004 1.001 1.003 1.000 1.005 1.002 0.950 | 12.1% | 8.4% |
| expr-bind-group | 1.977 | 1.977 | 1.000 | +0.0% | 0.997 1.000 1.001 1.001 0.999 1.001 0.999 1.000 0.999 1.001 1.001 0.999 | 0.9% | 1.2% |
| group-avg | 4.598 | 4.538 | 0.987 | -1.3% | 0.987 0.983 0.986 1.051 0.987 0.987 0.986 0.987 0.987 0.987 0.986 0.954 | 4.9% | 7.9% |
| predicate-counts | 1.909 | 1.820 | 0.953 | -4.7% | 1.002 1.003 0.954 1.015 1.807 1.002 0.946 0.982 0.805 0.790 0.948 0.691 | 100.7% | 89.9% |
| subquery-agg | 1.970 | 1.970 | 1.000 | +0.0% | 0.999 1.000 1.000 1.000 1.001 0.999 1.001 0.999 0.999 1.000 0.999 1.003 | 1.4% | 1.0% |
| types-grouped | 1.969 | 1.970 | 1.001 | +0.1% | 0.999 0.999 1.001 1.001 1.000 1.001 1.001 0.990 1.226 1.000 0.999 0.999 | 24.7% | 3.3% |
| **geometric mean** | | | 0.990 | -1.0% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
