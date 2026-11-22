# Per-request instruction counts

- A: ~/.cache/sparkles-nsc/bin/c2b5d7cfaf7b4705bebe7664f09917f0334c3d6e/sparkles, sha256 bc9413dd6461152f, GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 21.1.8; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: ~/.cache/sparkles-nsc/bin/adbe757c2a4cdc411a1edc9c6025d2c068120830/sparkles, sha256 5db11d6fd432a8bb, GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 21.1.8; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- host stanley, Intel(R) Core(TM) Ultra X7 358H, kernel 7.3.3, perf_event_paranoid 2, kernel instructions not available
- 4 rounds of A,B,B,A, 5 warm-up and 50 counted requests per query and process, server CPU 2, client CPU 3, RAYON_NUM_THREADS=1, spinners on
- load before 14.29 14.45 13.95 4/2306 1875878, after 13.04 14.15 13.86 7/2319 1877822

Flag: the 95% interval of the instruction ratio does not lie within 1 +- 1% below 2M instructions per request or 1 +- 0.25% above. Spread is (max - min) / median of the per-process counts. Bimodal marks a side whose processes split into two groups or spread by more than 5%, which makes the query inconclusive until it is rerun under an explicit mode. A query with fewer than 3 rounds that hold both builds, or with a window that was not fully counted, is incomplete.

| query | mode | A instr/req | B instr/req | B/A | 95% interval | per-round min-max | A spread | B spread | bimodal | cpu-time B/A | ctx-sw/req A, B | idle share | flag |
|---|---|--:|--:|--:|---|---|--:|--:|---|--:|---|--:|---|
| lang-filter | streaming | 31.524M | 31.524M | 1.0000 | 0.9999-1.0001 | 0.9999-1.0001 | 0.02% | 0.02% | - | 1.013 | 11.63, 11.74 | 0.00% |  |
| optional-count | streaming | 5.780M | 5.781M | 1.0005 | 0.9995-1.0007 | 0.9995-1.0007 | 0.20% | 0.10% | - | 0.996 | 7.92, 7.99 | 0.00% |  |
| regex-iri | streaming | 75.458M | 75.460M | 1.0000 | 0.9999-1.0001 | 0.9999-1.0001 | 0.01% | 0.01% | - | 1.005 | 12.41, 12.48 | 0.00% |  |

0 of 3 flagged
verdict: clean
