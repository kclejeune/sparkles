# Per-request instruction counts

- A: .artifacts/lanes/forge-backlog/bin/sparkles-ffb18866-matrix, sha256 c829b36f4cbe97c8, GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: .artifacts/lanes/forge-backlog/bin/sparkles-1cbc23cd-matrix, sha256 dd3ae65a8b980bd8, GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- host stanley, Intel(R) Core(TM) Ultra X7 358H, kernel 7.3.3, perf_event_paranoid 2, kernel instructions not available
- 4 rounds of A,B,B,A, 5 warm-up and 50 counted requests per query and process, server CPU 2, client CPU 3, RAYON_NUM_THREADS=1, spinners on
- load before 10.78 12.38 10.89 7/2333 1813632, after 21.70 20.01 14.36 9/2356 1830759

Flag: the 95% interval of the instruction ratio does not lie within 1 +- 1% below 2M instructions per request or 1 +- 0.25% above. Spread is (max - min) / median of the per-process counts. Bimodal marks a side whose processes split into two groups or spread by more than 5%, which makes the query inconclusive until it is rerun under an explicit mode. A query with fewer than 3 rounds that hold both builds, or with a window that was not fully counted, is incomplete.

| query | mode | A instr/req | B instr/req | B/A | 95% interval | per-round min-max | A spread | B spread | bimodal | cpu-time B/A | ctx-sw/req A, B | idle share | flag |
|---|---|--:|--:|--:|---|---|--:|--:|---|--:|---|--:|---|
| export-500k | streaming | 6.131G | 6.161G | 1.0047 | 1.0047-1.0051 | 1.0047-1.0051 | 0.04% | 0.04% | - | 1.015 | 1176.00, 1160.70 | 0.00% | FLAG |
| lang-filter | streaming | 32.155M | 32.154M | 1.0000 | 0.9999-1.0000 | 0.9999-1.0000 | 0.01% | 0.02% | - | 0.999 | 12.26, 13.59 | 0.00% |  |
| optional-chain | streaming | 313.191M | 314.908M | 1.0055 | 1.0053-1.0056 | 1.0053-1.0056 | 0.03% | 0.06% | - | 1.176 | 60.63, 65.42 | 0.00% | FLAG |
| optional-count | streaming | 5.808M | 5.807M | 1.0000 | 0.9994-1.0003 | 0.9994-1.0003 | 0.09% | 0.12% | - | 1.017 | 8.66, 8.47 | 0.00% |  |
| regex-iri | streaming | 76.702M | 76.700M | 1.0000 | 1.0000-1.0000 | 1.0000-1.0000 | 0.01% | 0.00% | - | 1.003 | 12.51, 13.65 | 0.00% |  |

2 of 5 flagged: export-500k streaming, optional-chain streaming
verdict: flagged
