# Per-request instruction counts

- A: ~/.cache/sparkles-nsc/bin/06ca04ef57d92b04f91b0f6206897ae774585010/sparkles, sha256 703d6a8f5cd28911, GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 21.1.8; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: ~/.cache/sparkles-nsc/bin/c2b5d7cfaf7b4705bebe7664f09917f0334c3d6e/sparkles, sha256 bc9413dd6461152f, GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 21.1.8; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- host stanley, Intel(R) Core(TM) Ultra X7 358H, kernel 7.3.3, perf_event_paranoid 2, kernel instructions not available
- 4 rounds of A,B,B,A, 5 warm-up and 50 counted requests per query and process, server CPU 2, client CPU 3, RAYON_NUM_THREADS=1, spinners on
- load before 13.63 14.35 13.91 5/2319 1874312, after 13.62 14.32 13.90 7/2344 1875265

Flag: the 95% interval of the instruction ratio does not lie within 1 +- 1% below 2M instructions per request or 1 +- 0.25% above. Spread is (max - min) / median of the per-process counts. Bimodal marks a side whose processes split into two groups or spread by more than 5%, which makes the query inconclusive until it is rerun under an explicit mode. A query with fewer than 3 rounds that hold both builds, or with a window that was not fully counted, is incomplete.

| query | mode | A instr/req | B instr/req | B/A | 95% interval | per-round min-max | A spread | B spread | bimodal | cpu-time B/A | ctx-sw/req A, B | idle share | flag |
|---|---|--:|--:|--:|---|---|--:|--:|---|--:|---|--:|---|
| lang-filter | streaming | 31.569M | 31.523M | 0.9986 | 0.9984-0.9987 | 0.9984-0.9987 | 0.02% | 0.03% | - | 0.960 | 11.42, 11.58 | 0.00% |  |
| optional-count | streaming | 5.770M | 5.780M | 1.0019 | 1.0015-1.0022 | 1.0015-1.0022 | 0.17% | 0.10% | - | 0.988 | 7.65, 7.60 | 0.00% |  |
| regex-iri | streaming | 75.552M | 75.456M | 0.9988 | 0.9987-0.9988 | 0.9987-0.9988 | 0.01% | 0.01% | - | 0.982 | 11.98, 12.01 | 0.00% |  |

0 of 3 flagged
verdict: clean
