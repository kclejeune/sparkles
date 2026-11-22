# Interleaved A/B on Namespace

- binary: /home/kclejeune/lanes/forge-backlog/bin/sparkles-8baa5686-matrix
- binary sha256: 20a381e40aa22acf2b099436054025829a753a69ca618156342d8af9b1f59d03
- binary commit: 8baa5686
- version: sparkles 0.1.0
- compilers: GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- build config: {"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1},"sparkles-owl":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{"CARGO_INCREMENTAL":"0"},"mbx_policy":"779a99e845a5ac3f1c4d64a3c95bcc74b33f47418e829bf008542ec2dbbcd151","profile":"release"}
- runtime env: clean
- scripts commit: unknown
- scripts dirty: 0
- host: forge
- started: 2026-11-18T09:27:30Z
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners on, execution eager,streaming
- A: /home/kclejeune/lanes/forge-backlog/bin/sparkles-8baa5686-matrix
- B: /home/kclejeune/lanes/forge-backlog/bin/sparkles-8baa5686-matrix
- data sha256: c2d387f07a0717fdcd1d001081507b5ac09608961f562f0102d971c36e36edd8
- kernel: 7.2.9
- cpu: 13th Gen Intel(R) Core(TM) i5-13500
- cpus: 20
- memory: 15620 MiB
- smt siblings: 0-1 10-11 12 13 14 15 16 17 18 19 2-3 4-5 6-7 8-9 
- loader: ld.so (GNU libc) stable release version 2.44.
- steal during the timed part: 0.00%

A and B returned the same sorted TSV answers to all 31 queries.

## Server execution time (execMs, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 0.929 | 0.925 | 0.996 | -0.4% | 0.993 0.997 | 0.9% | 0.7% |
| class-predicates | 0.938 | 0.938 | 1.000 | +0.0% | 0.996 1.003 | 2.2% | 0.7% |
| contains | 2.548 | 2.548 | 1.000 | +0.0% | 0.997 1.002 | 0.6% | 0.4% |
| count-all | 0.005 | 0.006 | 1.371 | +37.1% | 1.307 1.389 | 27.3% | 16.7% |
| distinct-join | 3.593 | 3.837 | 1.068 | +6.8% | 1.071 1.067 | 0.4% | 0.9% |
| distinct-obj | 0.004 | 0.005 | 1.324 | +32.4% | 1.314 1.332 | 15.4% | 4.1% |
| employee-docs | 0.037 | 0.040 | 1.081 | +8.1% | 1.144 1.082 | 7.4% | 12.6% |
| exists-join | 1.049 | 1.043 | 0.994 | -0.6% | 0.986 0.996 | 3.4% | 2.3% |
| export-500k | 2.096 | 0.395 | 0.188 | -81.2% | 0.188 0.189 | 1.2% | 1.0% |
| expr-agg-arg | 2.545 | 1.922 | 0.755 | -24.5% | 0.754 0.754 | 1.8% | 0.9% |
| expr-bind-group | 0.601 | 0.370 | 0.616 | -38.4% | 0.619 0.614 | 1.1% | 0.2% |
| expr-order-key | 1.159 | 1.147 | 0.989 | -1.1% | 0.989 0.993 | 1.7% | 1.8% |
| group-avg | 3.890 | 3.200 | 0.822 | -17.8% | 0.830 0.816 | 3.1% | 0.3% |
| knows-reach | 11.498 | 11.329 | 0.985 | -1.5% | 0.990 0.984 | 1.3% | 0.5% |
| lang-filter | 1.833 | 1.839 | 1.004 | +0.4% | 1.003 1.004 | 0.4% | 1.0% |
| minus | 0.412 | 0.262 | 0.635 | -36.5% | 0.639 0.629 | 2.8% | 3.6% |
| not-exists | 0.806 | 0.741 | 0.920 | -8.0% | 0.923 0.916 | 1.3% | 2.1% |
| optional-chain | 2.017 | 2.061 | 1.022 | +2.2% | 1.017 1.033 | 1.0% | 2.4% |
| optional-count | 0.484 | 0.544 | 1.123 | +12.3% | 1.122 1.126 | 2.4% | 1.0% |
| order-by-full | 41.096 | 41.536 | 1.011 | +1.1% | 1.014 1.005 | 1.3% | 1.6% |
| path-plus | 0.011 | 0.013 | 1.181 | +18.1% | 1.218 1.128 | 11.7% | 6.6% |
| predicate-counts | 0.005 | 0.007 | 1.514 | +51.4% | 1.436 1.536 | 16.7% | 6.8% |
| predicate-subjects | 0.007 | 0.009 | 1.366 | +36.6% | 1.408 1.312 | 9.0% | 9.2% |
| range-topk | 0.543 | 0.537 | 0.989 | -1.1% | 0.992 0.987 | 0.5% | 0.5% |
| regex-iri | 3.805 | 3.800 | 0.999 | -0.1% | 1.001 0.997 | 1.4% | 0.4% |
| star-join | 1.414 | 1.439 | 1.018 | +1.8% | 1.022 1.015 | 0.4% | 1.0% |
| star-lookup | 0.062 | 0.065 | 1.039 | +3.9% | 1.064 1.005 | 14.9% | 7.2% |
| subquery-agg | 0.186 | 0.184 | 0.986 | -1.4% | 0.977 1.001 | 1.4% | 3.5% |
| two-hop-count | 2.622 | 2.616 | 0.998 | -0.2% | 0.994 1.001 | 0.6% | 0.4% |
| types-grouped | 0.008 | 0.013 | 1.580 | +58.0% | 1.606 1.560 | 9.5% | 5.6% |
| values-star | 0.015 | 0.020 | 1.320 | +32.0% | 1.346 1.293 | 4.8% | 8.1% |
| **geometric mean** | | | 0.979 | -2.1% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 2.795 | 2.796 | 1.000 | +0.0% | 1.001 1.000 | 0.1% | 6.4% |
| class-predicates | 2.428 | 2.800 | 1.153 | +15.3% | 1.153 1.169 | 30.0% | 0.3% |
| contains | 3.716 | 3.761 | 1.012 | +1.2% | 1.015 1.008 | 2.2% | 3.5% |
| count-all | 1.577 | 1.944 | 1.233 | +23.3% | 1.243 0.996 | 21.1% | 37.8% |
| distinct-join | 5.244 | 5.978 | 1.140 | +14.0% | 1.136 1.142 | 1.4% | 0.4% |
| distinct-obj | 1.633 | 1.971 | 1.207 | +20.7% | 1.207 1.118 | 19.3% | 20.5% |
| employee-docs | 1.979 | 1.981 | 1.001 | +0.1% | 1.001 1.001 | 0.2% | 0.1% |
| exists-join | 2.803 | 2.801 | 1.000 | -0.0% | 0.999 1.000 | 0.1% | 0.1% |
| export-500k | 233.596 | 356.007 | 1.524 | +52.4% | 1.522 1.525 | 0.4% | 0.3% |
| expr-agg-arg | 3.803 | 3.238 | 0.851 | -14.9% | 0.850 0.852 | 2.7% | 0.8% |
| expr-bind-group | 1.973 | 1.981 | 1.004 | +0.4% | 1.004 1.005 | 4.3% | 0.2% |
| expr-order-key | 2.802 | 2.804 | 1.001 | +0.1% | 1.000 1.002 | 0.2% | 0.2% |
| group-avg | 5.200 | 4.558 | 0.877 | -12.3% | 0.921 0.874 | 15.3% | 6.5% |
| knows-reach | 12.760 | 12.636 | 0.990 | -1.0% | 0.994 0.988 | 1.4% | 1.5% |
| lang-filter | 2.981 | 3.056 | 1.025 | +2.5% | 1.026 1.025 | 4.2% | 1.5% |
| minus | 1.974 | 1.973 | 1.000 | -0.0% | 1.000 0.997 | 0.3% | 2.6% |
| not-exists | 2.020 | 1.976 | 0.978 | -2.2% | 0.976 0.989 | 9.8% | 24.0% |
| optional-chain | 15.035 | 22.037 | 1.466 | +46.6% | 1.466 1.466 | 2.1% | 0.0% |
| optional-count | 1.972 | 1.974 | 1.001 | +0.1% | 1.001 1.001 | 0.1% | 0.0% |
| order-by-full | 76.213 | 97.216 | 1.276 | +27.6% | 1.276 1.269 | 1.3% | 0.7% |
| path-plus | 1.888 | 1.974 | 1.046 | +4.6% | 1.086 1.013 | 23.1% | 0.4% |
| predicate-counts | 1.435 | 1.981 | 1.380 | +38.0% | 1.110 1.400 | 36.0% | 12.9% |
| predicate-subjects | 1.946 | 1.980 | 1.018 | +1.8% | 1.034 0.992 | 15.1% | 37.3% |
| range-topk | 1.973 | 1.975 | 1.001 | +0.1% | 1.001 1.000 | 0.2% | 0.1% |
| regex-iri | 4.970 | 5.003 | 1.007 | +0.7% | 1.028 0.998 | 2.2% | 3.0% |
| star-join | 5.044 | 7.007 | 1.389 | +38.9% | 1.404 1.368 | 4.2% | 7.2% |
| star-lookup | 1.985 | 1.987 | 1.001 | +0.1% | 1.001 1.002 | 1.5% | 2.0% |
| subquery-agg | 1.973 | 1.972 | 0.999 | -0.1% | 0.999 0.999 | 1.0% | 2.1% |
| two-hop-count | 3.853 | 3.902 | 1.013 | +1.3% | 1.010 1.018 | 0.8% | 2.1% |
| types-grouped | 1.972 | 1.972 | 1.000 | +0.0% | 0.985 1.000 | 13.5% | 15.2% |
| values-star | 1.980 | 1.982 | 1.001 | +0.1% | 1.000 1.002 | 0.5% | 11.8% |
| **geometric mean** | | | 1.073 | +7.3% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
