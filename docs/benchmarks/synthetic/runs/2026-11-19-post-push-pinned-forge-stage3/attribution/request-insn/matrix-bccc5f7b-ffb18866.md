# Per-request instruction counts

- A: .artifacts/lanes/forge-backlog/bin/sparkles-bccc5f7b-matrix, sha256 27cf6f507e4ad5ad, GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: .artifacts/lanes/forge-backlog/bin/sparkles-ffb18866-matrix, sha256 c829b36f4cbe97c8, GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- host stanley, Intel(R) Core(TM) Ultra X7 358H, kernel 7.3.3, perf_event_paranoid 2, kernel instructions not available
- 4 rounds of A,B,B,A, 5 warm-up and 50 counted requests per query and process, server CPU 2, client CPU 3, RAYON_NUM_THREADS=1, spinners on
- load before 13.48 13.01 10.74 4/2235 1804436, after 10.75 12.42 10.89 5/2374 1812562

Flag: the 95% interval of the instruction ratio does not lie within 1 +- 1% below 2M instructions per request or 1 +- 0.25% above. Spread is (max - min) / median of the per-process counts. Bimodal marks a side whose processes split into two groups or spread by more than 5%, which makes the query inconclusive until it is rerun under an explicit mode. A query with fewer than 3 rounds that hold both builds, or with a window that was not fully counted, is incomplete.

| query | mode | A instr/req | B instr/req | B/A | 95% interval | per-round min-max | A spread | B spread | bimodal | cpu-time B/A | ctx-sw/req A, B | idle share | flag |
|---|---|--:|--:|--:|---|---|--:|--:|---|--:|---|--:|---|
| export-500k | streaming | 6.131G | 6.131G | 1.0000 | 0.9998-1.0000 | 0.9998-1.0000 | 0.04% | 0.04% | - | 0.991 | 1126.43, 1142.50 | 0.00% |  |
| lang-filter | streaming | 32.157M | 32.157M | 1.0000 | 1.0000-1.0000 | 1.0000-1.0000 | 0.01% | 0.01% | - | 0.966 | 11.65, 11.67 | 0.00% |  |
| optional-chain | streaming | 313.174M | 313.152M | 0.9999 | 0.9997-1.0002 | 0.9997-1.0002 | 0.03% | 0.04% | - | 0.959 | 52.00, 51.52 | 0.00% |  |
| optional-count | streaming | 5.807M | 5.807M | 1.0000 | 0.9997-1.0001 | 0.9997-1.0001 | 0.04% | 0.07% | - | 0.886 | 7.93, 7.58 | 0.00% |  |
| regex-iri | streaming | 76.701M | 76.700M | 1.0000 | 1.0000-1.0000 | 1.0000-1.0000 | 0.00% | 0.01% | - | 0.903 | 12.36, 12.26 | 0.00% |  |

0 of 5 flagged
verdict: clean
