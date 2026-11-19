# Per-request instruction counts

- A: .artifacts/lanes/forge-backlog/bin/sparkles-eda22fce-matrix, sha256 915ba62e2bade555, GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: .artifacts/lanes/forge-backlog/bin/sparkles-bccc5f7b-matrix, sha256 27cf6f507e4ad5ad, GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- host stanley, Intel(R) Core(TM) Ultra X7 358H, kernel 7.3.3, perf_event_paranoid 2, kernel instructions not available
- 4 rounds of A,B,B,A, 5 warm-up and 50 counted requests per query and process, server CPU 2, client CPU 3, RAYON_NUM_THREADS=1, spinners on
- load before 14.89 12.73 10.25 6/2300 1791737, after 12.98 12.89 10.68 9/2218 1802780

Flag: the 95% interval of the instruction ratio does not lie within 1 +- 1% below 2M instructions per request or 1 +- 0.25% above. Spread is (max - min) / median of the per-process counts. Bimodal marks a side whose processes split into two groups or spread by more than 5%, which makes the query inconclusive until it is rerun under an explicit mode. A query with fewer than 3 rounds that hold both builds, or with a window that was not fully counted, is incomplete.

| query | mode | A instr/req | B instr/req | B/A | 95% interval | per-round min-max | A spread | B spread | bimodal | cpu-time B/A | ctx-sw/req A, B | idle share | flag |
|---|---|--:|--:|--:|---|---|--:|--:|---|--:|---|--:|---|
| export-500k | streaming | 6.131G | 6.131G | 0.9999 | 0.9998-1.0001 | 0.9998-1.0001 | 0.03% | 0.03% | - | 1.031 | 1149.79, 1151.42 | 0.00% |  |
| lang-filter | streaming | 32.156M | 32.155M | 1.0000 | 0.9999-1.0000 | 0.9999-1.0000 | 0.02% | 0.02% | - | 0.947 | 11.65, 11.55 | 0.00% |  |
| optional-chain | streaming | 313.151M | 313.160M | 1.0001 | 0.9999-1.0002 | 0.9999-1.0002 | 0.03% | 0.04% | - | 0.966 | 55.53, 52.30 | 0.00% |  |
| optional-count | streaming | 5.802M | 5.806M | 1.0007 | 1.0005-1.0009 | 1.0005-1.0009 | 0.04% | 0.07% | - | 0.928 | 8.10, 7.85 | 0.00% |  |
| regex-iri | streaming | 76.700M | 76.700M | 1.0000 | 1.0000-1.0000 | 1.0000-1.0000 | 0.00% | 0.00% | - | 0.971 | 12.38, 12.03 | 0.00% |  |

0 of 5 flagged
verdict: clean
