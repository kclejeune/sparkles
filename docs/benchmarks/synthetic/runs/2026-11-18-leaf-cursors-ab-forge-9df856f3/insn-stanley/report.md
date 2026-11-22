# Per-request instruction counts

- A: base-bccc5f7b, sha256 af1f600cafe20755, GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 21.1.8; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- B: tip-9df856f3, sha256 5e384d0f108d17bc, GCC: (GNU) 16.2.0; Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5ff9bbe7634aead059efe6b11a7eeba145); clang version 21.1.8; rustc version 1.99.1 (58cf8f9c3 2026-10-13)
- host stanley, Intel(R) Core(TM) Ultra X7 358H, kernel 7.3.3, perf_event_paranoid 2, kernel instructions not available
- 8 rounds of A,B,B,A, 5 warm-up and 50 counted requests per query and process, server CPU 2, client CPU 3, RAYON_NUM_THREADS=1, spinners on
- load before 8.72 7.06 5.77 7/2103 2471244, after 10.02 9.33 7.22 5/2097 2491363

Flag: the 95% interval of the instruction ratio does not lie within 1 +- 1% below 2M instructions per request or 1 +- 0.25% above. Spread is (max - min) / median of the per-process counts. Bimodal marks a side whose processes split into two groups or spread by more than 5%, which makes the query inconclusive until it is rerun under an explicit mode. A query with fewer than 3 rounds that hold both builds, or with a window that was not fully counted, is incomplete.

| query | mode | A instr/req | B instr/req | B/A | 95% interval | per-round min-max | A spread | B spread | bimodal | cpu-time B/A | ctx-sw/req A, B | idle share | flag |
|---|---|--:|--:|--:|---|---|--:|--:|---|--:|---|--:|---|
| cj-filter | eager | 512.150M | 512.152M | 1.0000 | 0.9998-1.0002 | 0.9998-1.0003 | 0.09% | 0.10% | - | 1.005 | 39.68, 40.26 | 0.00% |  |
| cj-filter | streaming | 142.007M | 142.070M | 1.0004 | 1.0004-1.0005 | 1.0004-1.0005 | 0.01% | 0.01% | - | 1.038 | 10.28, 10.73 | 0.00% |  |
| cj-two-var | eager | 63.587M | 63.587M | 1.0000 | 1.0000-1.0000 | 0.9999-1.0000 | 0.01% | 0.01% | - | 1.006 | 5.30, 5.51 | 0.00% |  |
| cj-two-var | streaming | 63.828M | 144.104M | 2.2577 | 2.2576-2.2577 | 2.2576-2.2577 | 0.01% | 0.00% | - | 1.756 | 10.28, 10.20 | 0.00% | FLAG |
| contains | eager | 48.162M | 48.162M | 1.0000 | 1.0000-1.0000 | 1.0000-1.0000 | 0.01% | 0.01% | - | 1.071 | 7.71, 8.12 | 0.00% |  |
| contains | streaming | 48.326M | 48.326M | 1.0000 | 1.0000-1.0000 | 1.0000-1.0001 | 0.01% | 0.01% | - | 1.102 | 11.79, 11.87 | 0.00% |  |
| count-all | eager | 303.4k | 303.6k | 1.0011 | 0.9986-1.0026 | 0.9969-1.0030 | 1.08% | 1.01% | - | 1.177 | 3.40, 3.41 | 0.02% |  |
| count-all | streaming | 445.7k | 445.7k | 1.0003 | 0.9964-1.0029 | 0.9925-1.0045 | 1.37% | 1.81% | - | 1.140 | 7.26, 7.26 | 0.01% |  |
| distinct-join | eager | 41.718M | 41.717M | 1.0000 | 0.9999-1.0000 | 0.9999-1.0001 | 0.02% | 0.02% | - | 1.150 | 4.69, 4.81 | 0.00% |  |
| distinct-join | streaming | 49.610M | 50.145M | 1.0108 | 1.0107-1.0109 | 1.0107-1.0109 | 0.03% | 0.02% | - | 1.029 | 10.85, 10.60 | 0.00% | FLAG |
| distinct-obj | eager | 313.7k | 313.4k | 0.9988 | 0.9984-1.0024 | 0.9979-1.0034 | 0.52% | 1.10% | - | 1.086 | 3.39, 3.41 | 0.02% |  |
| distinct-obj | streaming | 456.5k | 457.1k | 1.0016 | 1.0005-1.0062 | 0.9992-1.0136 | 2.81% | 1.18% | - | 1.095 | 7.19, 7.21 | 0.02% |  |
| employee-docs | eager | 706.7k | 706.3k | 0.9993 | 0.9986-1.0021 | 0.9985-1.0022 | 0.39% | 0.51% | - | 1.087 | 3.43, 3.39 | 0.01% |  |
| employee-docs | streaming | 1.042M | 1.062M | 1.0186 | 1.0182-1.0193 | 1.0175-1.0204 | 0.88% | 0.46% | - | 1.052 | 7.23, 7.34 | 0.01% | FLAG |
| expr-bind-group | eager | 9.239M | 9.239M | 0.9999 | 0.9999-1.0000 | 0.9998-1.0000 | 0.05% | 0.04% | - | 1.047 | 10.10, 9.98 | 0.00% |  |
| expr-bind-group | streaming | 7.306M | 7.403M | 1.0133 | 1.0132-1.0133 | 1.0130-1.0134 | 0.07% | 0.03% | - | 1.029 | 7.36, 7.58 | 0.00% | FLAG |
| lang-filter | eager | 31.972M | 31.972M | 1.0000 | 0.9999-1.0000 | 0.9999-1.0001 | 0.02% | 0.01% | - | 1.038 | 7.63, 7.56 | 0.00% |  |
| lang-filter | streaming | 32.129M | 32.129M | 1.0000 | 1.0000-1.0001 | 1.0000-1.0001 | 0.01% | 0.02% | - | 1.040 | 11.70, 11.61 | 0.00% |  |
| minus | eager | 3.796M | 3.796M | 1.0000 | 0.9999-1.0001 | 0.9997-1.0004 | 0.10% | 0.10% | - | 1.027 | 3.44, 3.46 | 0.00% |  |
| minus | streaming | 3.314M | 3.316M | 1.0005 | 1.0001-1.0008 | 0.9999-1.0009 | 0.11% | 0.14% | - | 0.951 | 7.33, 7.35 | 0.00% |  |
| path-plus | eager | 322.3k | 322.3k | 1.0000 | 0.9993-1.0037 | 0.9941-1.0053 | 1.18% | 1.11% | B | 0.967 | 3.42, 3.43 | 0.02% | SPLIT |
| path-plus | streaming | 493.1k | 493.3k | 0.9996 | 0.9976-1.0028 | 0.9917-1.0052 | 1.13% | 1.74% | - | 0.979 | 7.28, 7.15 | 0.01% |  |
| predicate-counts | eager | 418.3k | 418.5k | 1.0008 | 0.9994-1.0024 | 0.9964-1.0033 | 0.96% | 0.65% | B | 0.981 | 3.38, 3.44 | 0.02% | SPLIT |
| predicate-counts | streaming | 662.6k | 663.0k | 1.0000 | 0.9980-1.0016 | 0.9952-1.0066 | 1.91% | 1.99% | - | 0.991 | 7.26, 7.24 | 0.01% |  |
| predicate-subjects | eager | 403.4k | 403.1k | 0.9990 | 0.9974-1.0006 | 0.9958-1.0009 | 0.74% | 0.35% | - | 0.892 | 3.42, 3.36 | 0.01% |  |
| predicate-subjects | streaming | 646.5k | 646.9k | 1.0001 | 0.9971-1.0027 | 0.9948-1.0088 | 1.97% | 1.33% | - | 0.894 | 7.22, 7.16 | 0.01% |  |
| regex-iri | eager | 76.500M | 76.500M | 1.0000 | 1.0000-1.0000 | 1.0000-1.0000 | 0.01% | 0.01% | - | 0.988 | 8.95, 8.72 | 0.00% |  |
| regex-iri | streaming | 76.666M | 76.667M | 1.0000 | 1.0000-1.0000 | 1.0000-1.0000 | 0.00% | 0.01% | - | 0.953 | 12.19, 12.33 | 0.00% |  |
| star-join | eager | 39.457M | 39.460M | 1.0000 | 0.9999-1.0002 | 0.9999-1.0003 | 0.06% | 0.08% | - | 1.021 | 9.45, 9.65 | 0.00% |  |
| star-join | streaming | 67.405M | 67.465M | 1.0009 | 1.0007-1.0012 | 1.0006-1.0013 | 0.10% | 0.09% | - | 0.980 | 20.06, 20.32 | 0.00% |  |
| star-lookup | eager | 1.193M | 1.192M | 0.9998 | 0.9990-1.0006 | 0.9990-1.0012 | 0.31% | 0.37% | - | 1.020 | 3.37, 3.35 | 0.01% |  |
| star-lookup | streaming | 1.751M | 1.762M | 1.0064 | 1.0057-1.0072 | 1.0046-1.0094 | 0.64% | 0.21% | - | 1.021 | 7.20, 7.37 | 0.00% |  |
| subquery-agg | eager | 1.785M | 1.785M | 1.0000 | 0.9993-1.0006 | 0.9988-1.0013 | 0.20% | 0.27% | - | 1.003 | 7.29, 7.26 | 0.00% |  |
| subquery-agg | streaming | 1.992M | 1.993M | 1.0002 | 1.0000-1.0007 | 1.0000-1.0010 | 0.19% | 0.18% | - | 1.008 | 11.37, 11.42 | 0.00% |  |
| types-grouped | eager | 445.3k | 445.3k | 0.9996 | 0.9987-1.0001 | 0.9974-1.0010 | 0.59% | 0.26% | - | 1.022 | 3.37, 3.36 | 0.02% |  |
| types-grouped | streaming | 682.6k | 682.9k | 1.0009 | 0.9992-1.0031 | 0.9979-1.0040 | 1.09% | 0.51% | - | 1.028 | 7.19, 7.28 | 0.01% |  |
| values-star | eager | 769.7k | 768.8k | 0.9991 | 0.9977-0.9997 | 0.9972-1.0005 | 0.46% | 0.41% | - | 0.993 | 3.43, 3.33 | 0.01% |  |
| values-star | streaming | 1.068M | 1.079M | 1.0103 | 1.0092-1.0126 | 1.0078-1.0140 | 0.90% | 0.57% | - | 1.052 | 7.23, 7.24 | 0.01% | FLAG |

7 of 38 flagged: cj-two-var streaming, distinct-join streaming, employee-docs streaming, expr-bind-group streaming, path-plus eager (split), predicate-counts eager (split), values-star streaming
verdict: flagged
