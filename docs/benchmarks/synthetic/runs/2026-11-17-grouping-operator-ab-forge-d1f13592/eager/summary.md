# Interleaved A/B on Namespace

- A: commit 7e272f4dabc6b1b3e5f001690573d90ba07afcba (7e272f4d), built on wally
- A sha256: f0090fa03425eb22765931c552c74ab41d35448e95aebdf4f64f1100e2984e8f
- A compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: commit d1f13592c5b2dbc3bcb4f560ce7235505fbc2af8 (d1f13592), built on wally
- B sha256: c94b4df89f5a4a16b2f5c02bc9fe67e9e87356c75ac0628e186bf417feb03697
- B compilers: clang version 21.1.8; GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- data: 100000 people (scripts/gen-data.py)
- host: forge (ssh, nsc-bench-remote.sh run directly)
- settings: 6 queries, 12 rounds of A,B,B,A, 3 warm-up and 200 timed requests per query and server, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners on, execution eager
- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

A and B returned the same sorted TSV answers to all 6 queries.

Load: A 0.66 s, B 0.60 s (B/A 0.901). Store size: A 28 MiB, B 28 MiB.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| expr-agg-arg | 2.545 | 2.260 | 0.888 | -11.2% | 0.885 0.887 0.886 0.890 0.888 0.889 0.887 0.886 0.891 0.887 0.890 0.889 | 2.0% | 2.4% |
| expr-bind-group | 0.599 | 0.503 | 0.839 | -16.1% | 0.822 0.846 0.842 0.842 0.835 0.851 0.836 0.836 0.836 0.846 0.838 0.856 | 3.4% | 5.2% |
| group-avg | 3.873 | 3.508 | 0.906 | -9.4% | 0.906 0.906 0.904 0.910 0.903 0.908 0.902 0.904 0.908 0.904 0.905 0.901 | 2.0% | 1.9% |
| predicate-counts | 0.005 | 0.005 | 0.955 | -4.5% | 0.992 0.955 0.960 0.917 0.983 0.980 0.949 0.974 0.926 0.952 0.926 0.941 | 17.2% | 19.0% |
| subquery-agg | 0.182 | 0.183 | 1.009 | +0.9% | 1.016 1.000 1.013 1.019 1.009 1.015 1.001 1.016 1.012 1.003 1.004 0.990 | 8.2% | 5.1% |
| types-grouped | 0.008 | 0.008 | 1.003 | +0.3% | 1.002 0.986 1.008 1.010 1.014 1.022 1.025 1.002 0.979 1.009 0.956 0.986 | 14.7% | 15.6% |
| **geometric mean** | | | 0.931 | -6.9% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| expr-agg-arg | 3.807 | 3.523 | 0.925 | -7.5% | 0.913 0.919 0.928 0.934 0.935 0.921 0.935 0.939 0.920 0.929 0.915 0.920 | 5.4% | 13.7% |
| expr-bind-group | 1.970 | 1.970 | 1.000 | +0.0% | 1.000 1.001 1.001 1.000 0.999 0.999 1.001 0.999 0.999 1.000 1.000 1.001 | 0.3% | 1.0% |
| group-avg | 5.054 | 4.812 | 0.952 | -4.8% | 0.958 0.957 0.962 0.965 0.951 0.918 0.930 0.951 0.962 0.937 0.931 0.923 | 5.6% | 4.7% |
| predicate-counts | 0.976 | 0.977 | 1.001 | +0.1% | 1.017 1.001 1.001 0.974 1.011 1.005 0.998 1.022 1.012 1.001 1.000 1.000 | 6.0% | 5.9% |
| subquery-agg | 1.971 | 1.971 | 1.000 | +0.0% | 1.001 1.000 1.001 1.000 1.000 1.001 1.001 1.000 1.001 1.000 1.000 1.000 | 0.4% | 0.5% |
| types-grouped | 0.976 | 0.977 | 1.001 | +0.1% | 1.049 1.026 1.013 0.966 1.018 1.003 1.000 0.986 1.002 0.999 1.002 0.999 | 5.9% | 7.6% |
| **geometric mean** | | | 0.979 | -2.1% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
