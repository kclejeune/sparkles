# Interleaved A/B on Namespace

- binary: /home/kclejeune/lanes/forge-backlog/bin/sparkles-eda22fce-matrix
- binary sha256: 915ba62e2bade5551f25d5b0c7acca90aa8578e21f180dfb7ccdbf6290fc023c
- binary commit: eda22fce
- version: sparkles 0.1.0
- compilers: GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- build config: {"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1},"sparkles-owl":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{"CARGO_INCREMENTAL":"0"},"mbx_policy":"779a99e845a5ac3f1c4d64a3c95bcc74b33f47418e829bf008542ec2dbbcd151","profile":"release"}
- runtime env: clean
- scripts commit: unknown
- scripts dirty: 0
- host: forge
- started: 2026-11-19T02:31:44Z
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners on, execution eager,streaming
- A: /home/kclejeune/lanes/forge-backlog/bin/sparkles-eda22fce-matrix
- B: /home/kclejeune/lanes/forge-backlog/bin/sparkles-eda22fce-matrix
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
| class-predicate-list | 0.935 | 0.928 | 0.993 | -0.7% | 0.992 0.990 | 1.8% | 1.2% |
| class-predicates | 0.940 | 0.937 | 0.997 | -0.3% | 0.991 1.001 | 0.8% | 1.7% |
| contains | 2.551 | 2.553 | 1.001 | +0.1% | 1.001 1.001 | 0.9% | 0.4% |
| count-all | 0.005 | 0.006 | 1.187 | +18.7% | 1.192 1.177 | 5.8% | 2.6% |
| distinct-join | 3.637 | 3.823 | 1.051 | +5.1% | 1.056 1.046 | 1.4% | 0.9% |
| distinct-obj | 0.004 | 0.005 | 1.259 | +25.9% | 1.213 1.275 | 13.5% | 5.8% |
| employee-docs | 0.038 | 0.042 | 1.098 | +9.8% | 1.078 1.108 | 5.2% | 8.5% |
| exists-join | 1.071 | 1.044 | 0.975 | -2.5% | 0.970 0.977 | 3.5% | 1.9% |
| export-500k | 2.305 | 0.393 | 0.171 | -82.9% | 0.168 0.175 | 6.2% | 1.4% |
| expr-agg-arg | 2.558 | 1.921 | 0.751 | -24.9% | 0.764 0.747 | 4.5% | 0.8% |
| expr-bind-group | 0.602 | 0.372 | 0.619 | -38.1% | 0.622 0.617 | 0.7% | 1.1% |
| expr-order-key | 1.154 | 1.139 | 0.987 | -1.3% | 0.986 0.987 | 0.3% | 2.4% |
| group-avg | 3.843 | 3.194 | 0.831 | -16.9% | 0.834 0.813 | 2.9% | 0.3% |
| knows-reach | 11.518 | 11.293 | 0.980 | -2.0% | 0.985 0.978 | 0.9% | 0.5% |
| lang-filter | 1.867 | 1.864 | 0.999 | -0.1% | 0.998 1.000 | 0.6% | 0.4% |
| minus | 0.412 | 0.261 | 0.634 | -36.6% | 0.626 0.642 | 3.0% | 2.0% |
| not-exists | 0.805 | 0.742 | 0.922 | -7.8% | 0.918 0.923 | 1.6% | 1.0% |
| optional-chain | 2.020 | 2.065 | 1.022 | +2.2% | 1.023 1.018 | 1.4% | 0.9% |
| optional-count | 0.484 | 0.542 | 1.120 | +12.0% | 1.118 1.122 | 0.7% | 1.8% |
| order-by-full | 41.226 | 41.550 | 1.008 | +0.8% | 1.007 1.008 | 0.9% | 2.0% |
| path-plus | 0.012 | 0.013 | 1.094 | +9.4% | 1.047 1.147 | 18.6% | 5.8% |
| predicate-counts | 0.005 | 0.007 | 1.441 | +44.1% | 1.341 1.476 | 19.1% | 4.3% |
| predicate-subjects | 0.008 | 0.009 | 1.222 | +22.2% | 1.229 1.200 | 10.7% | 8.6% |
| range-topk | 0.543 | 0.537 | 0.989 | -1.1% | 0.986 0.993 | 0.7% | 0.7% |
| regex-iri | 3.747 | 3.725 | 0.994 | -0.6% | 1.000 0.989 | 0.9% | 0.4% |
| star-join | 1.414 | 1.431 | 1.012 | +1.2% | 1.014 1.009 | 0.6% | 0.3% |
| star-lookup | 0.063 | 0.067 | 1.051 | +5.1% | 1.053 1.059 | 7.7% | 7.2% |
| subquery-agg | 0.187 | 0.186 | 0.992 | -0.8% | 0.993 0.982 | 1.2% | 4.5% |
| two-hop-count | 2.632 | 2.626 | 0.998 | -0.2% | 0.997 0.997 | 1.1% | 0.8% |
| types-grouped | 0.008 | 0.013 | 1.643 | +64.3% | 1.500 1.742 | 17.1% | 4.4% |
| values-star | 0.017 | 0.021 | 1.231 | +23.1% | 1.189 1.242 | 9.6% | 9.6% |
| **geometric mean** | | | 0.961 | -3.9% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 2.796 | 2.799 | 1.001 | +0.1% | 1.001 1.003 | 23.4% | 0.2% |
| class-predicates | 2.796 | 2.800 | 1.001 | +0.1% | 1.002 1.001 | 25.5% | 7.4% |
| contains | 3.696 | 3.780 | 1.023 | +2.3% | 1.062 1.014 | 7.1% | 4.2% |
| count-all | 1.909 | 1.973 | 1.033 | +3.3% | 1.003 1.842 | 95.8% | 0.1% |
| distinct-join | 5.311 | 5.978 | 1.126 | +12.6% | 1.133 1.120 | 1.6% | 0.1% |
| distinct-obj | 1.894 | 1.973 | 1.042 | +4.2% | 1.047 1.028 | 64.8% | 0.1% |
| employee-docs | 1.981 | 1.984 | 1.001 | +0.1% | 1.001 1.001 | 0.5% | 0.2% |
| exists-join | 2.801 | 2.803 | 1.001 | +0.1% | 1.001 1.001 | 0.2% | 0.1% |
| export-500k | 232.748 | 346.026 | 1.487 | +48.7% | 1.487 1.487 | 0.6% | 0.3% |
| expr-agg-arg | 3.827 | 3.240 | 0.846 | -15.4% | 0.852 0.838 | 5.3% | 1.1% |
| expr-bind-group | 1.971 | 1.982 | 1.005 | +0.5% | 1.006 1.004 | 0.3% | 1.6% |
| expr-order-key | 2.803 | 2.801 | 0.999 | -0.1% | 0.999 0.998 | 0.1% | 0.1% |
| group-avg | 5.144 | 4.372 | 0.850 | -15.0% | 0.877 0.830 | 9.8% | 4.5% |
| knows-reach | 12.745 | 12.641 | 0.992 | -0.8% | 0.998 0.991 | 0.4% | 1.9% |
| lang-filter | 2.955 | 3.062 | 1.036 | +3.6% | 1.022 1.040 | 3.2% | 4.2% |
| minus | 1.972 | 1.974 | 1.002 | +0.2% | 1.003 1.001 | 4.1% | 0.2% |
| not-exists | 2.012 | 2.025 | 1.006 | +0.6% | 1.005 1.009 | 22.7% | 6.6% |
| optional-chain | 15.046 | 21.032 | 1.398 | +39.8% | 1.398 1.398 | 0.0% | 0.1% |
| optional-count | 1.973 | 1.973 | 1.000 | +0.0% | 1.000 0.997 | 0.2% | 5.6% |
| order-by-full | 76.071 | 95.216 | 1.252 | +25.2% | 1.251 1.256 | 1.4% | 1.0% |
| path-plus | 1.599 | 1.976 | 1.235 | +23.5% | 1.047 1.573 | 78.4% | 0.2% |
| predicate-counts | 1.950 | 1.982 | 1.017 | +1.7% | 1.026 1.014 | 15.6% | 0.1% |
| predicate-subjects | 1.762 | 1.983 | 1.125 | +12.5% | 1.214 1.125 | 54.2% | 0.1% |
| range-topk | 1.976 | 1.976 | 1.000 | +0.0% | 1.001 0.998 | 1.4% | 2.1% |
| regex-iri | 4.877 | 4.928 | 1.010 | +1.0% | 1.023 1.005 | 4.3% | 1.7% |
| star-join | 5.047 | 6.977 | 1.382 | +38.2% | 1.213 1.374 | 2.4% | 14.8% |
| star-lookup | 1.985 | 1.987 | 1.001 | +0.1% | 1.002 1.000 | 0.4% | 0.2% |
| subquery-agg | 1.974 | 1.975 | 1.001 | +0.1% | 1.001 1.000 | 0.2% | 0.2% |
| two-hop-count | 3.877 | 3.930 | 1.014 | +1.4% | 1.014 1.011 | 1.1% | 1.4% |
| types-grouped | 1.972 | 1.975 | 1.002 | +0.2% | 1.001 1.020 | 19.6% | 0.2% |
| values-star | 1.981 | 1.982 | 1.001 | +0.1% | 0.999 1.002 | 0.3% | 9.5% |
| **geometric mean** | | | 1.052 | +5.2% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
