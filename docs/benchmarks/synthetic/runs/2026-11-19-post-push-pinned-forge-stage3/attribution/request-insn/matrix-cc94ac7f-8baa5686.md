# Per-request instruction counts

- A: .artifacts/lanes/forge-backlog/bin/sparkles-cc94ac7f-matrix, sha256 9efa1cd4655ff0d7, GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: .artifacts/lanes/forge-backlog/bin/sparkles-8baa5686-matrix, sha256 20a381e40aa22acf, GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- host stanley, Intel(R) Core(TM) Ultra X7 358H, kernel 7.3.3, perf_event_paranoid 2, kernel instructions not available
- 4 rounds of A,B,B,A, 5 warm-up and 50 counted requests per query and process, server CPU 2, client CPU 3, RAYON_NUM_THREADS=1, spinners on
- load before 9.17 8.95 8.30 4/2259 1774261, after 17.77 12.50 9.74 32/2355 1784820

Flag: the 95% interval of the instruction ratio does not lie within 1 +- 1% below 2M instructions per request or 1 +- 0.25% above. Spread is (max - min) / median of the per-process counts. Bimodal marks a side whose processes split into two groups or spread by more than 5%, which makes the query inconclusive until it is rerun under an explicit mode. A query with fewer than 3 rounds that hold both builds, or with a window that was not fully counted, is incomplete.

| query | mode | A instr/req | B instr/req | B/A | 95% interval | per-round min-max | A spread | B spread | bimodal | cpu-time B/A | ctx-sw/req A, B | idle share | flag |
|---|---|--:|--:|--:|---|---|--:|--:|---|--:|---|--:|---|
| export-500k | streaming | 6.185G | 6.132G | 0.9913 | 0.9912-0.9915 | 0.9912-0.9915 | 0.03% | 0.04% | - | 0.999 | 1179.87, 1163.53 | 0.00% | FLAG |
| lang-filter | streaming | 31.596M | 32.157M | 1.0177 | 1.0177-1.0178 | 1.0177-1.0178 | 0.02% | 0.02% | - | 1.115 | 11.75, 11.61 | 0.00% | FLAG |
| optional-chain | streaming | 313.635M | 313.156M | 0.9984 | 0.9983-0.9985 | 0.9983-0.9985 | 0.04% | 0.04% | - | 1.168 | 53.32, 57.45 | 0.00% |  |
| optional-count | streaming | 5.552M | 5.803M | 1.0455 | 1.0448-1.0457 | 1.0448-1.0457 | 0.10% | 0.07% | - | 1.051 | 7.91, 8.32 | 0.00% | FLAG |
| regex-iri | streaming | 75.585M | 76.699M | 1.0147 | 1.0147-1.0148 | 1.0147-1.0148 | 0.00% | 0.01% | - | 1.057 | 12.69, 12.62 | 0.00% | FLAG |

4 of 5 flagged: export-500k streaming, lang-filter streaming, optional-count streaming, regex-iri streaming
verdict: flagged
