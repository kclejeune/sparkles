# Per-request instruction counts

- A: .artifacts/lanes/forge-backlog/bin/sparkles-8baa5686-matrix, sha256 20a381e40aa22acf, GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: .artifacts/lanes/forge-backlog/bin/sparkles-eda22fce-matrix, sha256 915ba62e2bade555, GCC: (GNU) 15.3.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 23.1.0git; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- host stanley, Intel(R) Core(TM) Ultra X7 358H, kernel 7.3.3, perf_event_paranoid 2, kernel instructions not available
- 4 rounds of A,B,B,A, 5 warm-up and 50 counted requests per query and process, server CPU 2, client CPU 3, RAYON_NUM_THREADS=1, spinners on
- load before 18.84 13.12 10.01 4/2313 1785798, after 11.48 12.00 9.97 9/2305 1791014

Flag: the 95% interval of the instruction ratio does not lie within 1 +- 1% below 2M instructions per request or 1 +- 0.25% above. Spread is (max - min) / median of the per-process counts. Bimodal marks a side whose processes split into two groups or spread by more than 5%, which makes the query inconclusive until it is rerun under an explicit mode. A query with fewer than 3 rounds that hold both builds, or with a window that was not fully counted, is incomplete.

| query | mode | A instr/req | B instr/req | B/A | 95% interval | per-round min-max | A spread | B spread | bimodal | cpu-time B/A | ctx-sw/req A, B | idle share | flag |
|---|---|--:|--:|--:|---|---|--:|--:|---|--:|---|--:|---|
| export-500k | streaming | 6.131G | 6.130G | 0.9999 | 0.9998-1.0001 | 0.9998-1.0001 | 0.04% | 0.04% | - | 0.998 | 1131.71, 1139.36 | 0.00% |  |
| lang-filter | streaming | 32.155M | 32.156M | 1.0000 | 1.0000-1.0000 | 1.0000-1.0000 | 0.01% | 0.01% | - | 0.990 | 11.50, 11.57 | 0.00% |  |
| optional-chain | streaming | 313.120M | 313.150M | 1.0001 | 0.9999-1.0003 | 0.9999-1.0003 | 0.06% | 0.03% | - | 1.014 | 48.33, 48.16 | 0.00% |  |
| optional-count | streaming | 5.803M | 5.802M | 1.0001 | 0.9996-1.0002 | 0.9996-1.0002 | 0.10% | 0.11% | - | 1.004 | 7.52, 7.63 | 0.00% |  |
| regex-iri | streaming | 76.700M | 76.700M | 1.0000 | 1.0000-1.0000 | 1.0000-1.0000 | 0.00% | 0.00% | - | 1.025 | 12.26, 12.09 | 0.00% |  |

0 of 5 flagged
verdict: clean
