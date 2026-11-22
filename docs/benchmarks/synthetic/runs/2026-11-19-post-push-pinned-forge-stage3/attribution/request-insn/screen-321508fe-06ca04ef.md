# Per-request instruction counts

- A: ~/.cache/sparkles-nsc/bin/321508fe3fee83062160207a5843e04f0556d689/sparkles, sha256 ce4a8d8322963377, GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 21.1.8; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: ~/.cache/sparkles-nsc/bin/06ca04ef57d92b04f91b0f6206897ae774585010/sparkles, sha256 703d6a8f5cd28911, GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 21.1.8; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- host stanley, Intel(R) Core(TM) Ultra X7 358H, kernel 7.3.3, perf_event_paranoid 2, kernel instructions not available
- 4 rounds of A,B,B,A, 5 warm-up and 50 counted requests per query and process, server CPU 2, client CPU 3, RAYON_NUM_THREADS=1, spinners on
- load before 13.27 18.03 14.58 4/2345 1850783, after 11.85 15.68 14.15 10/2311 1857381

Flag: the 95% interval of the instruction ratio does not lie within 1 +- 1% below 2M instructions per request or 1 +- 0.25% above. Spread is (max - min) / median of the per-process counts. Bimodal marks a side whose processes split into two groups or spread by more than 5%, which makes the query inconclusive until it is rerun under an explicit mode. A query with fewer than 3 rounds that hold both builds, or with a window that was not fully counted, is incomplete.

| query | mode | A instr/req | B instr/req | B/A | 95% interval | per-round min-max | A spread | B spread | bimodal | cpu-time B/A | ctx-sw/req A, B | idle share | flag |
|---|---|--:|--:|--:|---|---|--:|--:|---|--:|---|--:|---|
| export-500k | streaming | 6.173G | 6.173G | 1.0000 | 1.0000-1.0001 | 1.0000-1.0001 | 0.03% | 0.02% | - | 0.990 | 1156.01, 1094.50 | 0.00% |  |
| lang-filter | streaming | 31.571M | 31.572M | 1.0000 | 1.0000-1.0001 | 1.0000-1.0001 | 0.02% | 0.01% | - | 1.085 | 11.76, 11.55 | 0.00% |  |
| optional-chain | streaming | 312.917M | 314.401M | 1.0048 | 1.0047-1.0049 | 1.0047-1.0049 | 0.04% | 0.02% | - | 1.048 | 56.96, 54.32 | 0.00% | FLAG |
| optional-count | streaming | 5.526M | 5.766M | 1.0435 | 1.0433-1.0436 | 1.0433-1.0436 | 0.05% | 0.05% | - | 1.157 | 7.94, 7.55 | 0.00% | FLAG |
| regex-iri | streaming | 75.551M | 75.553M | 1.0000 | 1.0000-1.0000 | 1.0000-1.0000 | 0.00% | 0.00% | - | 1.087 | 12.30, 12.16 | 0.00% |  |

2 of 5 flagged: optional-chain streaming, optional-count streaming
verdict: flagged
