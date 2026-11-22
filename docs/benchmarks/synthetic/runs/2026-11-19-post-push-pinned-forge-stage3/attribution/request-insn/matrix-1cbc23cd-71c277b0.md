# Per-request instruction counts

- A: .artifacts/lanes/forge-backlog/bin/sparkles-1cbc23cd-matrix, sha256 dd3ae65a8b980bd8, GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: .artifacts/lanes/forge-backlog/bin/sparkles-71c277b0-matrix, sha256 4fde46bc9f96cbb4, GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- host stanley, Intel(R) Core(TM) Ultra X7 358H, kernel 7.3.3, perf_event_paranoid 2, kernel instructions not available
- 4 rounds of A,B,B,A, 5 warm-up and 50 counted requests per query and process, server CPU 2, client CPU 3, RAYON_NUM_THREADS=1, spinners on
- load before 20.98 19.92 14.39 7/2373 1831426, after 15.09 18.60 14.70 9/2327 1850117

Flag: the 95% interval of the instruction ratio does not lie within 1 +- 1% below 2M instructions per request or 1 +- 0.25% above. Spread is (max - min) / median of the per-process counts. Bimodal marks a side whose processes split into two groups or spread by more than 5%, which makes the query inconclusive until it is rerun under an explicit mode. A query with fewer than 3 rounds that hold both builds, or with a window that was not fully counted, is incomplete.

| query | mode | A instr/req | B instr/req | B/A | 95% interval | per-round min-max | A spread | B spread | bimodal | cpu-time B/A | ctx-sw/req A, B | idle share | flag |
|---|---|--:|--:|--:|---|---|--:|--:|---|--:|---|--:|---|
| export-500k | streaming | 6.161G | 2.816G | 0.4572 | 0.4569-0.4572 | 0.4569-0.4572 | 0.03% | 0.08% | - | 0.552 | 1123.80, 1068.90 | 0.00% | FLAG |
| lang-filter | streaming | 32.154M | 32.091M | 0.9980 | 0.9980-0.9981 | 0.9980-0.9981 | 0.01% | 0.02% | - | 1.001 | 11.59, 11.68 | 0.00% |  |
| optional-chain | streaming | 314.950M | 156.024M | 0.4954 | 0.4953-0.4956 | 0.4953-0.4956 | 0.03% | 0.05% | - | 0.617 | 53.64, 49.91 | 0.00% | FLAG |
| optional-count | streaming | 5.805M | 5.693M | 0.9806 | 0.9804-0.9809 | 0.9804-0.9809 | 0.07% | 0.08% | - | 1.029 | 8.23, 8.48 | 0.00% | FLAG |
| regex-iri | streaming | 76.700M | 76.637M | 0.9992 | 0.9992-0.9992 | 0.9992-0.9992 | 0.01% | 0.01% | - | 1.021 | 12.51, 12.37 | 0.00% |  |

3 of 5 flagged: export-500k streaming, optional-chain streaming, optional-count streaming
verdict: flagged
