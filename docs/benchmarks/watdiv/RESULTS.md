# WatDiv results

This report was imported from the manually maintained benchmark page. Its tables and
qualifications are preserved, and are not regenerated from the JSON records. Read the
[reference setup and answer rules](../reference/README.md) with each result.

## WatDiv

WatDiv v0.6 basic testing at scale 100 has 10,973,381 triples and 20 templates with
five instances each, except C1 to C3, which have one, for 88 instances. Times are
geometric means over each template's ten-run instance means after two warm-up requests,
and category means weight templates equally. Every engine's answers agreed by RDF value
multiset for every instance. Agreement is against the other engines, rather than an
independent reference oracle. `mise run bench:watdiv 100` reproduces the run.

| template | instances | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|---:|
| L1 | 5 | **3.59** | 7.59 | 9.39 | 3.62 | 4.44 |
| L2 | 5 | **3.70** | 12.4 | 8.94 | 5.18 | 14.5 |
| L3 | 5 | **3.55** | 6.68 | 6.88 | 3.84 | 4.16 |
| L4 | 5 | **3.71** | 8.04 | 6.84 | 4.09 | 7.33 |
| L5 | 5 | **3.76** | 11.0 | 11.1 | 4.64 | 13.3 |
| S1 | 5 | **4.08** | 6.74 | 92.6 | 4.29 | 6.72 |
| S2 | 5 | **4.01** | 6.55 | 7.84 | 4.36 | 6.53 |
| S3 | 5 | **3.78** | 10.4 | 11.0 | 5.14 | 14.8 |
| S4 | 5 | **3.61** | 13.1 | 7.62 | 5.01 | 28.4 |
| S5 | 5 | **3.67** | 6.90 | 10.2 | 4.35 | 13.4 |
| S6 | 5 | **3.63** | 6.61 | 6.99 | 3.90 | 6.19 |
| S7 | 5 | 3.56 | 5.39 | 9.48 | **3.56** | 3.65 |
| F1 | 5 | **3.85** | 8.00 | 16.0 | 4.12 | 14.1 |
| F2 | 5 | **3.80** | 7.09 | 33.0 | 4.20 | 8.42 |
| F3 | 5 | **3.78** | 6.31 | 17.3 | 27.7 | 6.03 |
| F4 | 5 | **4.42** | 7.35 | 59.0 | 28.2 | 8.86 |
| F5 | 5 | **4.17** | 6.53 | 19.3 | 4.32 | 7.21 |
| C1 | 1 | **6.89** | 17.6 | 36.2 | 7.95 | 28.6 |
| C2 | 1 | **8.07** | 149 | 54.5 | 41.7 | 362 |
| C3 | 1 | **50.9** | 11866 | 303 | 94.8 | 23094 |

| category | templates | sparkles (ms) | jena-fuseki (ms) | qlever (ms) | fluree (ms) | oxigraph (ms) |
|---|---:|---:|---:|---:|---:|---:|
| **linear (L)** | 5 of 5 | **3.66** | 8.89 | 8.47 | 4.24 | 7.64 |
| **star (S)** | 7 of 7 | **3.76** | 7.60 | 12.2 | 4.34 | 9.21 |
| **snowflake (F)** | 5 of 5 | **4.00** | 7.03 | 25.3 | 8.99 | 8.56 |
| **complex (C)** | 3 of 3 | **14.1** | 314 | 84.3 | 31.6 | 621 |
| **all templates** | 20 of 20 | **4.63** | 13.6 | 17.9 | 6.97 | 16.2 |

*Sparkles was measured on forge on 2026-10-23 at commit 6699f27d, and the other engines on
2026-10-20. The [WatDiv record](../2026-10-23-watdiv-100-forge.json) has the raw
instance timings, answer fingerprints and load measurements.*

Sparkles has the lowest mean on 19 of the 20 templates and on 81 of 88 instances.
Fluree's S7 mean is 0.04% lower, a tie. Fluree has the lower mean on L1-1, L1-5, F2-1,
F5-1, S6-5, S7-1 and S7-2, and Oxigraph on S7-1 and S7-2. The L1-1 difference puts
Sparkles 11% slower, while the others are within 5%. C3 returns 419,866 rows and includes
the complete response.

WatDiv was created by G. Aluç, O. Hartig, M. T. Özsu and K. Daudjee, “Diversified Stress
Testing of RDF Data Management Systems,” ISWC 2014.
