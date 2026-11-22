# Per-request instruction counts

- A: ~/.cache/sparkles-nsc/bin/adbe757c2a4cdc411a1edc9c6025d2c068120830/sparkles, sha256 5db11d6fd432a8bb, GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 21.1.8; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: ~/.cache/sparkles-nsc/bin/65c570db95f6b1219d3e23b17232a7e5acfcc672/sparkles, sha256 af789ad39869ff55, GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 21.1.8; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- host stanley, Intel(R) Core(TM) Ultra X7 358H, kernel 7.3.3, perf_event_paranoid 2, kernel instructions not available
- 4 rounds of A,B,B,A, 5 warm-up and 50 counted requests per query and process, server CPU 2, client CPU 3, RAYON_NUM_THREADS=1, spinners on
- load before 16.39 14.85 14.09 5/2321 1878430, after 13.99 14.41 13.96 5/2265 1879339

Flag: the 95% interval of the instruction ratio does not lie within 1 +- 1% below 2M instructions per request or 1 +- 0.25% above. Spread is (max - min) / median of the per-process counts. Bimodal marks a side whose processes split into two groups or spread by more than 5%, which makes the query inconclusive until it is rerun under an explicit mode. A query with fewer than 3 rounds that hold both builds, or with a window that was not fully counted, is incomplete.

| query | mode | A instr/req | B instr/req | B/A | 95% interval | per-round min-max | A spread | B spread | bimodal | cpu-time B/A | ctx-sw/req A, B | idle share | flag |
|---|---|--:|--:|--:|---|---|--:|--:|---|--:|---|--:|---|
| lang-filter | streaming | 31.529M | 32.109M | 1.0184 | 1.0183-1.0186 | 1.0183-1.0186 | 0.02% | 0.02% | - | 1.012 | 12.53, 12.31 | 0.00% | FLAG |
| optional-count | streaming | 5.780M | 5.779M | 0.9997 | 0.9996-1.0000 | 0.9996-1.0000 | 0.03% | 0.07% | - | 1.003 | 8.57, 8.73 | 0.00% |  |
| regex-iri | streaming | 75.460M | 76.648M | 1.0157 | 1.0157-1.0157 | 1.0157-1.0157 | 0.01% | 0.01% | - | 0.965 | 13.52, 13.43 | 0.00% | FLAG |

2 of 3 flagged: lang-filter streaming, regex-iri streaming
verdict: flagged
