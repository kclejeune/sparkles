# Per-request instruction counts

- A: ~/.cache/sparkles-nsc/bin/06ca04ef57d92b04f91b0f6206897ae774585010/sparkles, sha256 703d6a8f5cd28911, GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 21.1.8; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: ~/.cache/sparkles-nsc/bin/8baa5686fae1fe235870c119e283a254c5e8372d/sparkles, sha256 54062dddfca7b861, GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 21.1.8; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- host stanley, Intel(R) Core(TM) Ultra X7 358H, kernel 7.3.3, perf_event_paranoid 2, kernel instructions not available
- 4 rounds of A,B,B,A, 5 warm-up and 50 counted requests per query and process, server CPU 2, client CPU 3, RAYON_NUM_THREADS=1, spinners on
- load before 13.11 15.77 14.20 7/2288 1858876, after 11.29 14.13 13.81 36/2387 1868568

Flag: the 95% interval of the instruction ratio does not lie within 1 +- 1% below 2M instructions per request or 1 +- 0.25% above. Spread is (max - min) / median of the per-process counts. Bimodal marks a side whose processes split into two groups or spread by more than 5%, which makes the query inconclusive until it is rerun under an explicit mode. A query with fewer than 3 rounds that hold both builds, or with a window that was not fully counted, is incomplete.

| query | mode | A instr/req | B instr/req | B/A | 95% interval | per-round min-max | A spread | B spread | bimodal | cpu-time B/A | ctx-sw/req A, B | idle share | flag |
|---|---|--:|--:|--:|---|---|--:|--:|---|--:|---|--:|---|
| export-500k | streaming | 6.173G | 6.119G | 0.9912 | 0.9911-0.9913 | 0.9911-0.9913 | 0.02% | 0.03% | - | 0.958 | 1122.71, 1125.83 | 0.00% | FLAG |
| lang-filter | streaming | 31.571M | 32.130M | 1.0177 | 1.0176-1.0177 | 1.0176-1.0177 | 0.02% | 0.01% | - | 0.976 | 11.61, 11.39 | 0.00% | FLAG |
| optional-chain | streaming | 314.391M | 312.340M | 0.9935 | 0.9933-0.9936 | 0.9933-0.9936 | 0.04% | 0.05% | - | 0.932 | 53.14, 52.22 | 0.00% | FLAG |
| optional-count | streaming | 5.766M | 5.778M | 1.0020 | 1.0017-1.0025 | 1.0017-1.0025 | 0.06% | 0.09% | - | 1.025 | 7.84, 7.92 | 0.00% |  |
| regex-iri | streaming | 75.554M | 76.667M | 1.0147 | 1.0147-1.0147 | 1.0147-1.0147 | 0.01% | 0.00% | - | 1.028 | 12.21, 12.52 | 0.00% | FLAG |

4 of 5 flagged: export-500k streaming, lang-filter streaming, optional-chain streaming, regex-iri streaming
verdict: flagged
