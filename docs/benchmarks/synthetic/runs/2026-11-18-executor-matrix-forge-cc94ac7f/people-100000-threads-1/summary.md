# Interleaved A/B on Namespace

- binary: /home/kclejeune/lanes/forge-backlog/bin/sparkles-cc94ac7f-matrix
- binary sha256: 9efa1cd4655ff0d75af758bf0bb0f54ff5950f152b61b3e0afb765ff63e0fcba
- binary commit: cc94ac7f
- version: sparkles 0.1.0
- compilers: GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- build config: {"Cargo.toml":{"release":{"codegen-units":4,"debug":1,"lto":"thin","package":{"sparkles-core":{"codegen-units":1},"sparkles-owl":{"codegen-units":1}}}},"cargo":"build --release -p sparkles-server --bin sparkles","env":{"CARGO_INCREMENTAL":"0"},"mbx_policy":"779a99e845a5ac3f1c4d64a3c95bcc74b33f47418e829bf008542ec2dbbcd151","profile":"release"}
- runtime env: clean
- scripts commit: unknown
- scripts dirty: 0
- host: forge
- started: 2026-11-18T07:04:20Z
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=1, server CPU 2, client CPU 4, spinners on, execution eager,streaming
- A: /home/kclejeune/lanes/forge-backlog/bin/sparkles-cc94ac7f-matrix
- B: /home/kclejeune/lanes/forge-backlog/bin/sparkles-cc94ac7f-matrix
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
| class-predicate-list | 0.928 | 0.916 | 0.988 | -1.2% | 0.993 0.986 | 1.2% | 2.8% |
| class-predicates | 0.934 | 0.929 | 0.994 | -0.6% | 0.988 1.001 | 1.6% | 1.2% |
| contains | 2.510 | 2.514 | 1.001 | +0.1% | 1.000 1.001 | 0.7% | 0.6% |
| count-all | 0.005 | 0.006 | 1.135 | +13.5% | 1.146 1.122 | 10.2% | 9.5% |
| distinct-join | 3.459 | 3.700 | 1.070 | +7.0% | 1.068 1.071 | 1.6% | 1.2% |
| distinct-obj | 0.004 | 0.005 | 1.112 | +11.2% | 1.122 1.105 | 12.8% | 12.1% |
| employee-docs | 0.038 | 0.038 | 1.005 | +0.5% | 1.008 1.008 | 5.0% | 5.1% |
| exists-join | 1.027 | 1.032 | 1.005 | +0.5% | 1.002 1.007 | 1.2% | 0.6% |
| export-500k | 2.153 | 0.414 | 0.192 | -80.8% | 0.178 0.195 | 11.6% | 0.8% |
| expr-agg-arg | 2.598 | 1.895 | 0.729 | -27.1% | 0.732 0.728 | 1.8% | 0.9% |
| expr-bind-group | 0.604 | 0.371 | 0.614 | -38.6% | 0.621 0.610 | 1.0% | 1.3% |
| expr-order-key | 1.146 | 1.142 | 0.996 | -0.4% | 1.001 0.993 | 1.0% | 1.0% |
| group-avg | 3.952 | 3.170 | 0.802 | -19.8% | 0.815 0.794 | 3.1% | 0.6% |
| knows-reach | 11.514 | 11.339 | 0.985 | -1.5% | 0.984 0.985 | 0.8% | 0.9% |
| lang-filter | 1.640 | 1.641 | 1.001 | +0.1% | 1.001 0.999 | 0.6% | 0.7% |
| minus | 0.312 | 0.257 | 0.823 | -17.7% | 0.826 0.825 | 2.8% | 1.5% |
| not-exists | 0.800 | 0.740 | 0.925 | -7.5% | 0.925 0.924 | 2.1% | 2.2% |
| optional-chain | 2.002 | 1.951 | 0.974 | -2.6% | 0.971 0.975 | 1.4% | 1.4% |
| optional-count | 0.488 | 0.516 | 1.058 | +5.8% | 1.073 1.046 | 2.8% | 1.7% |
| order-by-full | 41.168 | 41.505 | 1.008 | +0.8% | 1.005 1.018 | 1.9% | 1.6% |
| path-plus | 0.012 | 0.012 | 1.026 | +2.6% | 1.057 1.033 | 10.7% | 8.1% |
| predicate-counts | 0.005 | 0.006 | 1.219 | +21.9% | 1.241 1.192 | 11.2% | 14.4% |
| predicate-subjects | 0.007 | 0.009 | 1.216 | +21.6% | 1.255 1.165 | 10.3% | 3.1% |
| range-topk | 0.535 | 0.529 | 0.988 | -1.2% | 0.991 0.983 | 0.8% | 1.1% |
| regex-iri | 3.481 | 3.480 | 1.000 | -0.0% | 1.000 1.000 | 0.4% | 0.2% |
| star-join | 1.411 | 1.425 | 1.010 | +1.0% | 1.010 1.009 | 2.0% | 0.2% |
| star-lookup | 0.063 | 0.064 | 1.012 | +1.2% | 0.990 1.052 | 5.0% | 8.2% |
| subquery-agg | 0.187 | 0.186 | 0.996 | -0.4% | 0.979 1.024 | 2.9% | 3.4% |
| two-hop-count | 2.646 | 2.628 | 0.993 | -0.7% | 0.996 0.992 | 1.0% | 0.4% |
| types-grouped | 0.009 | 0.013 | 1.510 | +51.0% | 1.524 1.512 | 5.7% | 1.9% |
| values-star | 0.015 | 0.020 | 1.296 | +29.6% | 1.336 1.279 | 5.0% | 5.6% |
| **geometric mean** | | | 0.951 | -4.9% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 2.429 | 2.795 | 1.151 | +15.1% | 1.069 1.062 | 27.0% | 26.6% |
| class-predicates | 2.640 | 2.800 | 1.061 | +6.1% | 1.071 1.060 | 9.7% | 0.2% |
| contains | 3.671 | 3.727 | 1.015 | +1.5% | 1.025 1.014 | 6.0% | 4.9% |
| count-all | 1.954 | 1.970 | 1.008 | +0.8% | 1.017 1.008 | 8.5% | 35.7% |
| distinct-join | 5.032 | 5.975 | 1.187 | +18.7% | 1.173 1.206 | 3.6% | 6.5% |
| distinct-obj | 1.495 | 1.970 | 1.318 | +31.8% | 1.013 1.312 | 41.6% | 40.7% |
| employee-docs | 1.976 | 1.982 | 1.003 | +0.3% | 1.017 1.002 | 14.7% | 12.8% |
| exists-join | 2.801 | 2.800 | 0.999 | -0.1% | 1.000 0.999 | 0.2% | 5.7% |
| export-500k | 250.029 | 338.984 | 1.356 | +35.6% | 1.351 1.352 | 0.8% | 0.3% |
| expr-agg-arg | 3.862 | 3.204 | 0.830 | -17.0% | 0.832 0.828 | 2.7% | 0.6% |
| expr-bind-group | 1.971 | 1.978 | 1.004 | +0.4% | 1.005 1.002 | 11.7% | 16.8% |
| expr-order-key | 2.799 | 2.801 | 1.001 | +0.1% | 1.001 0.999 | 0.3% | 0.1% |
| group-avg | 5.320 | 4.510 | 0.848 | -15.2% | 0.831 0.836 | 3.6% | 2.4% |
| knows-reach | 12.800 | 12.659 | 0.989 | -1.1% | 0.992 0.988 | 0.6% | 0.9% |
| lang-filter | 2.802 | 2.837 | 1.012 | +1.2% | 1.014 1.012 | 0.4% | 0.5% |
| minus | 1.968 | 1.972 | 1.002 | +0.2% | 1.002 1.000 | 17.6% | 14.5% |
| not-exists | 1.974 | 1.974 | 1.000 | -0.0% | 0.999 1.000 | 1.0% | 1.7% |
| optional-chain | 15.129 | 21.047 | 1.391 | +39.1% | 1.395 1.390 | 0.6% | 0.2% |
| optional-count | 1.972 | 1.974 | 1.001 | +0.1% | 1.002 0.999 | 0.3% | 0.3% |
| order-by-full | 77.299 | 95.005 | 1.229 | +22.9% | 1.222 1.228 | 0.8% | 0.8% |
| path-plus | 1.346 | 1.975 | 1.467 | +46.7% | 1.627 1.468 | 21.7% | 0.2% |
| predicate-counts | 1.461 | 1.982 | 1.357 | +35.7% | 1.381 1.326 | 11.5% | 1.7% |
| predicate-subjects | 1.628 | 1.980 | 1.216 | +21.6% | 1.288 1.120 | 32.2% | 44.6% |
| range-topk | 1.973 | 1.974 | 1.001 | +0.1% | 0.999 1.002 | 0.2% | 33.6% |
| regex-iri | 4.654 | 4.702 | 1.010 | +1.0% | 1.005 1.049 | 10.0% | 8.5% |
| star-join | 5.069 | 6.111 | 1.205 | +20.5% | 1.207 1.192 | 5.4% | 14.7% |
| star-lookup | 1.984 | 1.986 | 1.001 | +0.1% | 1.001 1.001 | 0.2% | 17.7% |
| subquery-agg | 1.973 | 1.974 | 1.001 | +0.1% | 1.001 1.001 | 11.0% | 0.2% |
| two-hop-count | 3.873 | 3.921 | 1.012 | +1.2% | 1.015 1.007 | 4.1% | 0.6% |
| types-grouped | 1.601 | 1.972 | 1.232 | +23.2% | 1.345 1.004 | 40.1% | 38.0% |
| values-star | 1.977 | 1.980 | 1.002 | +0.2% | 1.050 1.001 | 16.3% | 0.1% |
| **geometric mean** | | | 1.083 | +8.3% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
