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
- settings: 31 queries, 2 rounds of A,B,B,A, 3 warm-up and 10 timed requests per query and server process, RAYON_NUM_THREADS=12, server CPU 0-11, client CPU 12, spinners off, execution eager,streaming
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
| class-predicate-list | 0.986 | 1.012 | 1.026 | +2.6% | 1.013 1.032 | 3.7% | 2.4% |
| class-predicates | 0.996 | 1.020 | 1.024 | +2.4% | 1.019 1.040 | 1.0% | 2.8% |
| contains | 0.759 | 0.773 | 1.019 | +1.9% | 1.027 0.993 | 8.4% | 9.3% |
| count-all | 0.008 | 0.010 | 1.290 | +29.0% | 1.303 1.271 | 25.7% | 27.9% |
| distinct-join | 3.760 | 4.015 | 1.068 | +6.8% | 1.055 1.065 | 3.1% | 1.5% |
| distinct-obj | 0.007 | 0.009 | 1.269 | +26.9% | 1.383 1.204 | 25.5% | 27.1% |
| employee-docs | 0.050 | 0.054 | 1.074 | +7.4% | 1.189 1.022 | 16.0% | 10.9% |
| exists-join | 1.139 | 1.150 | 1.009 | +0.9% | 0.992 1.010 | 4.1% | 0.8% |
| export-500k | 2.389 | 0.374 | 0.156 | -84.4% | 0.158 0.155 | 13.8% | 1.4% |
| expr-agg-arg | 2.901 | 2.046 | 0.705 | -29.5% | 0.686 0.713 | 4.0% | 3.4% |
| expr-bind-group | 0.762 | 0.398 | 0.522 | -47.8% | 0.520 0.523 | 9.4% | 5.9% |
| expr-order-key | 1.242 | 1.225 | 0.986 | -1.4% | 0.993 0.980 | 6.9% | 1.9% |
| group-avg | 4.035 | 3.362 | 0.833 | -16.7% | 0.833 0.807 | 5.7% | 1.8% |
| knows-reach | 11.811 | 11.678 | 0.989 | -1.1% | 0.979 0.993 | 2.0% | 0.9% |
| lang-filter | 0.778 | 0.810 | 1.041 | +4.1% | 1.003 1.077 | 16.4% | 16.9% |
| minus | 0.481 | 0.281 | 0.583 | -41.7% | 0.588 0.582 | 15.4% | 0.6% |
| not-exists | 0.867 | 0.793 | 0.914 | -8.6% | 0.915 0.899 | 5.8% | 2.4% |
| optional-chain | 2.113 | 2.234 | 1.057 | +5.7% | 1.072 1.019 | 6.4% | 4.5% |
| optional-count | 0.548 | 0.575 | 1.050 | +5.0% | 1.058 1.054 | 8.4% | 3.4% |
| order-by-full | 8.768 | 9.033 | 1.030 | +3.0% | 1.057 1.024 | 3.9% | 2.8% |
| path-plus | 0.018 | 0.020 | 1.148 | +14.8% | 1.110 1.166 | 32.6% | 16.8% |
| predicate-counts | 0.007 | 0.011 | 1.455 | +45.5% | 1.509 1.397 | 28.4% | 17.6% |
| predicate-subjects | 0.008 | 0.015 | 1.770 | +77.0% | 1.885 1.641 | 39.8% | 26.2% |
| range-topk | 0.582 | 0.575 | 0.987 | -1.3% | 1.003 0.980 | 3.0% | 2.0% |
| regex-iri | 1.024 | 1.017 | 0.994 | -0.6% | 1.028 0.944 | 6.9% | 15.1% |
| star-join | 1.591 | 1.651 | 1.038 | +3.8% | 1.048 1.034 | 10.5% | 3.3% |
| star-lookup | 0.083 | 0.093 | 1.123 | +12.3% | 1.081 1.199 | 7.9% | 10.1% |
| subquery-agg | 0.218 | 0.209 | 0.959 | -4.1% | 1.338 0.876 | 38.5% | 43.3% |
| two-hop-count | 1.232 | 1.229 | 0.998 | -0.2% | 1.021 0.973 | 8.9% | 8.1% |
| types-grouped | 0.012 | 0.021 | 1.706 | +70.6% | 1.835 1.654 | 9.7% | 28.8% |
| values-star | 0.021 | 0.031 | 1.492 | +49.2% | 1.455 1.593 | 8.6% | 63.8% |
| **geometric mean** | | | 0.978 | -2.2% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 1.653 | 1.925 | 1.165 | +16.5% | 1.160 1.175 | 3.0% | 4.6% |
| class-predicates | 1.675 | 1.937 | 1.156 | +15.6% | 1.209 1.098 | 2.8% | 12.0% |
| contains | 1.480 | 1.673 | 1.131 | +13.1% | 1.169 1.104 | 9.2% | 2.4% |
| count-all | 0.504 | 0.768 | 1.523 | +52.3% | 1.560 1.506 | 11.1% | 12.0% |
| distinct-join | 4.978 | 5.625 | 1.130 | +13.0% | 1.099 1.145 | 3.7% | 8.4% |
| distinct-obj | 0.502 | 0.778 | 1.549 | +54.9% | 1.648 1.406 | 18.3% | 15.1% |
| employee-docs | 0.696 | 0.984 | 1.415 | +41.5% | 1.440 1.384 | 11.8% | 10.5% |
| exists-join | 1.899 | 2.216 | 1.166 | +16.6% | 1.156 1.177 | 3.4% | 1.6% |
| export-500k | 220.362 | 287.297 | 1.304 | +30.4% | 1.300 1.305 | 0.7% | 0.8% |
| expr-agg-arg | 3.674 | 3.078 | 0.838 | -16.2% | 0.833 0.824 | 2.4% | 5.6% |
| expr-bind-group | 1.489 | 1.352 | 0.908 | -9.2% | 0.912 0.908 | 8.4% | 7.9% |
| expr-order-key | 1.992 | 2.128 | 1.069 | +6.9% | 1.066 1.070 | 7.0% | 6.2% |
| group-avg | 4.902 | 4.382 | 0.894 | -10.6% | 0.908 0.868 | 3.1% | 4.8% |
| knows-reach | 12.820 | 12.756 | 0.995 | -0.5% | 0.997 0.991 | 2.1% | 0.7% |
| lang-filter | 1.493 | 1.657 | 1.110 | +11.0% | 1.130 1.169 | 9.3% | 11.4% |
| minus | 1.148 | 1.233 | 1.074 | +7.4% | 1.069 1.082 | 6.1% | 8.6% |
| not-exists | 1.579 | 1.722 | 1.091 | +9.1% | 1.101 1.083 | 3.3% | 2.2% |
| optional-chain | 14.244 | 18.607 | 1.306 | +30.6% | 1.322 1.297 | 2.2% | 3.6% |
| optional-count | 1.272 | 1.496 | 1.175 | +17.5% | 1.191 1.157 | 2.6% | 8.5% |
| order-by-full | 41.501 | 51.578 | 1.243 | +24.3% | 1.244 1.241 | 1.3% | 0.5% |
| path-plus | 0.492 | 0.786 | 1.595 | +59.5% | 1.456 1.626 | 21.0% | 32.1% |
| predicate-counts | 0.536 | 0.763 | 1.422 | +42.2% | 1.493 1.468 | 15.4% | 26.3% |
| predicate-subjects | 0.443 | 0.762 | 1.721 | +72.1% | 1.734 1.732 | 18.4% | 40.7% |
| range-topk | 1.371 | 1.619 | 1.182 | +18.2% | 1.194 1.174 | 0.4% | 8.3% |
| regex-iri | 1.761 | 1.929 | 1.095 | +9.5% | 1.092 1.082 | 4.8% | 6.9% |
| star-join | 4.941 | 5.976 | 1.210 | +21.0% | 1.222 1.194 | 6.8% | 2.0% |
| star-lookup | 0.813 | 1.065 | 1.309 | +30.9% | 1.379 1.266 | 4.8% | 13.6% |
| subquery-agg | 0.838 | 1.085 | 1.295 | +29.5% | 1.405 1.214 | 15.8% | 8.5% |
| two-hop-count | 2.010 | 2.267 | 1.128 | +12.8% | 1.161 1.096 | 6.5% | 7.7% |
| types-grouped | 0.558 | 0.782 | 1.402 | +40.2% | 1.495 1.243 | 6.3% | 20.8% |
| values-star | 0.680 | 0.931 | 1.368 | +36.8% | 1.331 1.428 | 5.5% | 9.7% |
| **geometric mean** | | | 1.208 | +20.8% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
