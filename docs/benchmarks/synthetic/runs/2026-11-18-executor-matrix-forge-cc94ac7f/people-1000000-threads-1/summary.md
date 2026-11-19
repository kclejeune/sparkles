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
- data sha256: 10daf6486c20f8dd4579d819ea73bb5e7836e054cd6d2d1b73f99f35233635f8
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
| class-predicate-list | 9.874 | 10.269 | 1.040 | +4.0% | 1.040 1.040 | 0.9% | 0.3% |
| class-predicates | 9.850 | 10.274 | 1.043 | +4.3% | 1.037 1.046 | 1.7% | 0.5% |
| contains | 24.418 | 24.460 | 1.002 | +0.2% | 1.002 1.001 | 0.2% | 0.2% |
| count-all | 0.006 | 0.007 | 1.154 | +15.4% | 1.184 1.131 | 21.6% | 15.3% |
| distinct-join | 38.489 | 37.485 | 0.974 | -2.6% | 0.976 0.972 | 0.4% | 0.7% |
| distinct-obj | 0.005 | 0.005 | 1.114 | +11.4% | 1.104 1.124 | 8.3% | 8.2% |
| employee-docs | 0.049 | 0.055 | 1.120 | +12.0% | 1.127 1.119 | 7.7% | 6.2% |
| exists-join | 8.484 | 8.472 | 0.999 | -0.1% | 1.000 1.003 | 4.6% | 1.3% |
| export-500k | 2.205 | 0.430 | 0.195 | -80.5% | 0.194 0.200 | 21.4% | 3.1% |
| expr-agg-arg | 29.223 | 22.778 | 0.779 | -22.1% | 0.778 0.785 | 1.4% | 0.7% |
| expr-bind-group | 5.753 | 3.298 | 0.573 | -42.7% | 0.573 0.573 | 1.9% | 0.1% |
| expr-order-key | 10.913 | 10.778 | 0.988 | -1.2% | 0.981 0.992 | 0.5% | 1.2% |
| group-avg | 45.314 | 36.706 | 0.810 | -19.0% | 0.808 0.812 | 1.8% | 0.3% |
| knows-reach | 139.616 | 136.913 | 0.981 | -1.9% | 0.982 0.978 | 0.8% | 0.5% |
| lang-filter | 16.005 | 15.980 | 0.998 | -0.2% | 0.999 0.997 | 0.5% | 0.3% |
| minus | 3.251 | 2.441 | 0.751 | -24.9% | 0.746 0.755 | 2.1% | 1.2% |
| not-exists | 9.440 | 8.644 | 0.916 | -8.4% | 0.905 0.926 | 1.9% | 2.7% |
| optional-chain | 27.693 | 22.519 | 0.813 | -18.7% | 0.796 0.811 | 4.4% | 3.6% |
| optional-count | 5.318 | 4.999 | 0.940 | -6.0% | 0.939 0.943 | 1.5% | 0.2% |
| order-by-full | 514.724 | 517.212 | 1.005 | +0.5% | 1.004 1.006 | 0.6% | 0.6% |
| path-plus | 0.010 | 0.009 | 0.985 | -1.5% | 1.012 0.986 | 8.9% | 14.6% |
| predicate-counts | 0.005 | 0.007 | 1.217 | +21.7% | 1.200 1.244 | 5.6% | 8.1% |
| predicate-subjects | 0.008 | 0.009 | 1.160 | +16.0% | 1.141 1.174 | 13.6% | 10.9% |
| range-topk | 5.080 | 5.042 | 0.992 | -0.8% | 0.991 0.998 | 0.7% | 1.8% |
| regex-iri | 34.496 | 34.519 | 1.001 | +0.1% | 1.001 1.001 | 0.1% | 0.3% |
| star-join | 14.067 | 14.800 | 1.052 | +5.2% | 1.053 1.051 | 0.3% | 1.8% |
| star-lookup | 0.057 | 0.061 | 1.065 | +6.5% | 1.064 1.072 | 5.6% | 5.5% |
| subquery-agg | 1.698 | 1.652 | 0.972 | -2.8% | 0.966 0.973 | 2.7% | 1.3% |
| two-hop-count | 27.739 | 27.841 | 1.004 | +0.4% | 1.004 1.003 | 0.3% | 0.3% |
| types-grouped | 0.009 | 0.013 | 1.434 | +43.4% | 1.409 1.462 | 9.7% | 10.9% |
| values-star | 0.016 | 0.021 | 1.284 | +28.4% | 1.245 1.317 | 7.5% | 2.3% |
| **geometric mean** | | | 0.941 | -5.9% | | | |

## Client wall time (curl time_total, ms)

| query | A | B | B/A | change | per-round B/A | A spread | B spread |
|---|--:|--:|--:|--:|---|--:|--:|
| class-predicate-list | 11.252 | 11.602 | 1.031 | +3.1% | 1.033 1.006 | 6.2% | 1.5% |
| class-predicates | 11.244 | 11.657 | 1.037 | +3.7% | 1.034 1.040 | 2.3% | 1.6% |
| contains | 25.625 | 25.700 | 1.003 | +0.3% | 1.003 1.002 | 0.2% | 0.2% |
| count-all | 1.889 | 1.974 | 1.045 | +4.5% | 1.021 1.169 | 35.5% | 0.2% |
| distinct-join | 43.716 | 44.942 | 1.028 | +2.8% | 1.030 1.026 | 0.7% | 0.5% |
| distinct-obj | 1.827 | 1.976 | 1.082 | +8.2% | 1.020 1.209 | 20.4% | 0.1% |
| employee-docs | 1.981 | 1.984 | 1.002 | +0.2% | 1.003 1.001 | 0.3% | 0.3% |
| exists-join | 9.870 | 9.873 | 1.000 | +0.0% | 1.000 0.996 | 2.8% | 1.3% |
| export-500k | 256.046 | 336.033 | 1.312 | +31.2% | 1.312 1.312 | 0.4% | 0.1% |
| expr-agg-arg | 30.605 | 23.742 | 0.776 | -22.4% | 0.773 0.781 | 2.0% | 2.0% |
| expr-bind-group | 6.980 | 4.579 | 0.656 | -34.4% | 0.658 0.655 | 1.1% | 4.4% |
| expr-order-key | 12.907 | 12.301 | 0.953 | -4.7% | 0.954 0.953 | 0.7% | 2.6% |
| group-avg | 46.930 | 38.327 | 0.817 | -18.3% | 0.816 0.818 | 1.8% | 0.3% |
| knows-reach | 140.928 | 138.226 | 0.981 | -1.9% | 0.982 0.977 | 0.7% | 0.5% |
| lang-filter | 17.261 | 17.263 | 1.000 | +0.0% | 1.001 0.997 | 0.5% | 1.0% |
| minus | 4.533 | 3.689 | 0.814 | -18.6% | 0.791 0.832 | 16.7% | 1.4% |
| not-exists | 10.807 | 9.956 | 0.921 | -7.9% | 0.907 0.932 | 1.5% | 2.3% |
| optional-chain | 142.572 | 207.165 | 1.453 | +45.3% | 1.444 1.458 | 1.8% | 0.5% |
| optional-count | 6.627 | 6.277 | 0.947 | -5.3% | 0.947 0.952 | 1.4% | 1.0% |
| order-by-full | 865.074 | 1029.990 | 1.191 | +19.1% | 1.189 1.192 | 0.4% | 0.4% |
| path-plus | 1.973 | 1.976 | 1.001 | +0.1% | 1.002 1.001 | 0.9% | 0.3% |
| predicate-counts | 1.969 | 1.984 | 1.008 | +0.8% | 1.038 1.005 | 33.7% | 0.1% |
| predicate-subjects | 1.969 | 1.982 | 1.007 | +0.7% | 1.006 1.012 | 24.1% | 1.3% |
| range-topk | 6.423 | 6.430 | 1.001 | +0.1% | 1.000 1.000 | 1.1% | 4.1% |
| regex-iri | 35.725 | 35.728 | 1.000 | +0.0% | 1.001 0.999 | 0.3% | 0.5% |
| star-join | 37.048 | 51.108 | 1.380 | +38.0% | 1.380 1.379 | 0.2% | 0.3% |
| star-lookup | 1.986 | 1.986 | 1.000 | +0.0% | 1.001 1.011 | 3.2% | 0.4% |
| subquery-agg | 2.907 | 2.915 | 1.002 | +0.2% | 0.999 1.005 | 3.4% | 2.3% |
| two-hop-count | 29.109 | 29.316 | 1.007 | +0.7% | 1.007 1.015 | 0.3% | 1.8% |
| types-grouped | 1.972 | 1.976 | 1.002 | +0.2% | 1.003 1.002 | 2.7% | 0.1% |
| values-star | 1.981 | 1.982 | 1.001 | +0.1% | 1.001 1.001 | 0.2% | 0.2% |
| **geometric mean** | | | 1.003 | +0.3% | | | |

The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.
