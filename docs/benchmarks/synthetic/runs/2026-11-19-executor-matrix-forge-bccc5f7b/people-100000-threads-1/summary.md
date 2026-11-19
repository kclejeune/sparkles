# Interleaved A/B on Namespace

- binary: /home/kclejeune/lanes/forge-backlog/bin/sparkles-bccc5f7b-matrix
- binary sha256: 27cf6f507e4ad5ad12f65605fe4f6c4961d95c43dcf4affb7aa4ca90d5703cd6
- binary commit: bccc5f7b
- version: sparkles 0.1.0
- compilers: GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- build config: {"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1},"sparkles-owl":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{"CARGO_INCREMENTAL":"0"},"mbx_policy":"779a99e845a5ac3f1c4d64a3c95bcc74b33f47418e829bf008542ec2dbbcd151","profile":"release"}
- runtime env: clean
- scripts commit: unknown
- scripts dirty: 0
- host: forge
- started: 2026-11-19T00:08:03Z
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners on, execution eager,streaming
- A: /home/kclejeune/lanes/forge-backlog/bin/sparkles-bccc5f7b-matrix
- B: /home/kclejeune/lanes/forge-backlog/bin/sparkles-bccc5f7b-matrix
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
| class-predicate-list | 0.943 | 0.934 | 0.990 | -1.0% | 0.992 0.992 | 1.1% | 0.6% |
| class-predicates | 0.954 | 0.945 | 0.990 | -1.0% | 0.991 0.998 | 2.2% | 1.6% |
| contains | 2.534 | 2.536 | 1.001 | +0.1% | 1.001 1.000 | 0.4% | 0.8% |
| count-all | 0.005 | 0.006 | 1.224 | +22.4% | 1.224 1.231 | 16.2% | 13.6% |
| distinct-join | 3.556 | 3.741 | 1.052 | +5.2% | 1.054 1.050 | 1.1% | 1.2% |
| distinct-obj | 0.004 | 0.005 | 1.230 | +23.0% | 1.190 1.327 | 17.2% | 7.2% |
| employee-docs | 0.038 | 0.040 | 1.049 | +4.9% | 1.071 1.030 | 4.7% | 7.4% |
| exists-join | 1.060 | 1.038 | 0.979 | -2.1% | 0.985 0.976 | 1.0% | 1.2% |
| export-500k | 2.128 | 0.403 | 0.189 | -81.1% | 0.180 0.192 | 9.8% | 1.2% |
| expr-agg-arg | 2.287 | 1.930 | 0.844 | -15.6% | 0.846 0.825 | 10.2% | 0.8% |
| expr-bind-group | 0.507 | 0.364 | 0.717 | -28.3% | 0.715 0.716 | 1.1% | 1.7% |
| expr-order-key | 1.143 | 1.142 | 0.999 | -0.1% | 0.999 0.996 | 1.5% | 1.2% |
| group-avg | 3.580 | 3.186 | 0.890 | -11.0% | 0.892 0.882 | 2.1% | 0.1% |
| knows-reach | 11.339 | 11.394 | 1.005 | +0.5% | 1.006 1.005 | 0.7% | 0.5% |
| lang-filter | 1.785 | 1.785 | 1.000 | +0.0% | 0.993 1.005 | 1.6% | 0.6% |
| minus | 0.365 | 0.263 | 0.720 | -28.0% | 0.723 0.719 | 2.8% | 1.3% |
| not-exists | 0.764 | 0.742 | 0.971 | -2.9% | 0.971 0.970 | 1.5% | 0.8% |
| optional-chain | 2.008 | 2.061 | 1.026 | +2.6% | 1.025 1.026 | 1.5% | 0.7% |
| optional-count | 0.416 | 0.541 | 1.300 | +30.0% | 1.296 1.305 | 2.5% | 1.6% |
| order-by-full | 41.781 | 41.544 | 0.994 | -0.6% | 0.997 0.996 | 2.0% | 1.4% |
| path-plus | 0.013 | 0.013 | 1.001 | +0.1% | 0.985 0.988 | 2.3% | 4.0% |
| predicate-counts | 0.005 | 0.007 | 1.334 | +33.4% | 1.290 1.384 | 15.2% | 6.3% |
| predicate-subjects | 0.007 | 0.009 | 1.258 | +25.8% | 1.278 1.242 | 5.9% | 10.3% |
| range-topk | 0.542 | 0.535 | 0.988 | -1.2% | 0.986 0.991 | 0.8% | 0.4% |
| regex-iri | 3.754 | 3.767 | 1.003 | +0.3% | 1.001 1.010 | 0.5% | 1.5% |
| star-join | 1.398 | 1.417 | 1.013 | +1.3% | 1.007 1.020 | 1.3% | 1.2% |
| star-lookup | 0.062 | 0.063 | 1.015 | +1.5% | 1.014 1.020 | 7.2% | 6.5% |
| subquery-agg | 0.185 | 0.185 | 1.002 | +0.2% | 1.008 1.013 | 3.1% | 6.6% |
| two-hop-count | 2.635 | 2.625 | 0.996 | -0.4% | 0.994 0.998 | 1.5% | 0.7% |
| types-grouped | 0.008 | 0.013 | 1.635 | +63.5% | 1.684 1.586 | 2.8% | 10.0% |
| values-star | 0.016 | 0.019 | 1.210 | +21.0% | 1.204 1.216 | 4.2% | 2.0% |
| **geometric mean** | | | 0.979 | -2.1% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 2.791 | 2.798 | 1.002 | +0.2% | 1.003 1.001 | 17.1% | 5.8% |
| class-predicates | 2.796 | 2.800 | 1.001 | +0.1% | 1.003 1.001 | 0.3% | 0.2% |
| contains | 3.519 | 3.760 | 1.068 | +6.8% | 1.026 1.073 | 5.3% | 3.9% |
| count-all | 1.970 | 1.973 | 1.002 | +0.2% | 1.002 1.195 | 30.9% | 2.3% |
| distinct-join | 5.056 | 5.979 | 1.183 | +18.3% | 1.157 1.188 | 4.2% | 0.1% |
| distinct-obj | 1.929 | 1.976 | 1.024 | +2.4% | 1.028 1.014 | 13.5% | 0.2% |
| employee-docs | 1.981 | 1.981 | 1.000 | +0.0% | 0.999 1.000 | 0.3% | 3.1% |
| exists-join | 2.801 | 2.802 | 1.000 | +0.0% | 1.001 1.000 | 0.1% | 0.1% |
| export-500k | 229.041 | 363.044 | 1.585 | +58.5% | 1.585 1.585 | 0.3% | 0.4% |
| expr-agg-arg | 3.513 | 3.280 | 0.934 | -6.6% | 0.974 0.926 | 20.8% | 11.0% |
| expr-bind-group | 1.975 | 1.980 | 1.003 | +0.3% | 1.003 1.003 | 0.2% | 0.1% |
| expr-order-key | 2.802 | 2.803 | 1.000 | +0.0% | 1.000 1.001 | 0.2% | 0.2% |
| group-avg | 4.893 | 4.374 | 0.894 | -10.6% | 0.893 0.896 | 2.6% | 4.2% |
| knows-reach | 12.569 | 12.691 | 1.010 | +1.0% | 1.010 1.006 | 0.8% | 0.5% |
| lang-filter | 2.800 | 2.969 | 1.060 | +6.0% | 1.062 1.030 | 24.1% | 7.1% |
| minus | 1.974 | 1.976 | 1.001 | +0.1% | 1.002 1.002 | 0.2% | 0.3% |
| not-exists | 1.974 | 1.978 | 1.002 | +0.2% | 1.003 1.003 | 0.4% | 1.7% |
| optional-chain | 15.050 | 22.042 | 1.465 | +46.5% | 1.465 1.464 | 0.1% | 1.0% |
| optional-count | 1.973 | 1.974 | 1.001 | +0.1% | 0.999 1.010 | 11.2% | 0.4% |
| order-by-full | 77.044 | 98.041 | 1.273 | +27.3% | 1.277 1.272 | 1.4% | 1.6% |
| path-plus | 1.970 | 1.974 | 1.002 | +0.2% | 1.001 1.003 | 18.3% | 0.1% |
| predicate-counts | 1.927 | 1.983 | 1.029 | +2.9% | 1.211 1.005 | 35.3% | 0.2% |
| predicate-subjects | 1.923 | 1.982 | 1.030 | +3.0% | 1.903 1.011 | 100.5% | 0.2% |
| range-topk | 1.974 | 1.974 | 1.000 | +0.0% | 0.998 1.001 | 0.2% | 0.2% |
| regex-iri | 4.884 | 4.971 | 1.018 | +1.8% | 1.016 1.038 | 3.9% | 17.2% |
| star-join | 4.944 | 7.005 | 1.417 | +41.7% | 1.408 1.423 | 1.2% | 0.0% |
| star-lookup | 1.984 | 1.987 | 1.002 | +0.2% | 1.001 1.003 | 0.7% | 1.5% |
| subquery-agg | 1.972 | 1.975 | 1.002 | +0.2% | 1.002 1.001 | 0.2% | 0.2% |
| two-hop-count | 3.861 | 3.904 | 1.011 | +1.1% | 1.001 1.015 | 3.5% | 9.3% |
| types-grouped | 1.396 | 1.976 | 1.415 | +41.5% | 1.467 1.044 | 40.7% | 1.1% |
| values-star | 1.979 | 1.982 | 1.001 | +0.1% | 1.001 1.001 | 0.2% | 0.1% |
| **geometric mean** | | | 1.068 | +6.8% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
