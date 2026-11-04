# Scale measurements

- baseline: commit 4ab93ecb/a199a8ae, UI /home/kclejeune/repositories/github.com/kclejeune/sparkles-lane-explore-scale-725-measure/ui/build, stanley, 153.0.8010.12, WebGL ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (Subzero) (0x0000C0DE)), SwiftShader driver), 2026-11-04T14:13:17.920Z, laptop, synthetic taxon, software WebGL
- after3: commit 4ab93ecb/a199a8ae, UI /home/kclejeune/lanes-local/explore-scale-725/ui-after3/ui/build, stanley, 153.0.8010.12, WebGL ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (Subzero) (0x0000C0DE)), SwiftShader driver), 2026-11-04T14:14:46.552Z, laptop, synthetic taxon, software WebGL

#### explore-1000

| Measure | baseline | after3 | after3 / baseline |
|---|---|---|---|
| Nodes explored | 1,000 | 1,000 | 1.00x |
| Nodes drawn | 1,000 | 1,000 | 1.00x |
| Blocked at (nodes) | n/a | n/a | n/a |
| First graph (ms) | 416 | 453 | 1.09x |
| Expansions | 10 | 10 | 1.00x |
| Expand latency, median (ms) | 138 | 64 | 0.46x |
| Expand latency, max (ms) | 315 | 256 | 0.81x |
| Pan and zoom frame p50 (ms) | 83.3 | 66.6 | 0.80x |
| Pan and zoom frame p95 (ms) | 150 | 66.8 | 0.45x |
| Pan and zoom frame max (ms) | 166.7 | 100 | 0.60x |
| Pan and zoom frames in 5 s | 62 | 82 | 1.32x |
| Idle frame p50 (ms) | 16.7 | 16.7 | 1.00x |
| API requests | 34 | 56 | 1.65x |
| Long tasks (total ms) | 6,582 | 6,388 | 0.97x |
| JS heap (MB) | 23 | 23.3 | 1.01x |
| DOM nodes | 2,326 | 2,348 | 1.01x |

#### explore-20000

| Measure | baseline | after3 | after3 / baseline |
|---|---|---|---|
| Nodes explored | 1,543 | 16,303 | 10.57x |
| Nodes drawn | 1,543 | 5,000 | 3.24x |
| Blocked at (nodes) | 1,543 | 16,303 | 10.57x |
| First graph (ms) | 370 | 532 | 1.44x |
| Expansions | 19 | 183 | 9.63x |
| Expand latency, median (ms) | 177 | 390 | 2.20x |
| Expand latency, max (ms) | 8,016 | 2,056 | 0.26x |
| Pan and zoom frame p50 (ms) | 100 | 150.1 | 1.50x |
| Pan and zoom frame p95 (ms) | 100.1 | 249.9 | 2.50x |
| Pan and zoom frame max (ms) | 149.9 | 283.4 | 1.89x |
| Pan and zoom frames in 5 s | 52 | 31 | 0.60x |
| Idle frame p50 (ms) | 16.7 | 16.7 | 1.00x |
| API requests | 49 | 752 | 15.35x |
| Long tasks (total ms) | 7,761 | 208,123 | 26.82x |
| JS heap (MB) | 30.9 | 85 | 2.75x |
| DOM nodes | 2,364 | 2,370 | 1.00x |

#### explore-5000

| Measure | baseline | after3 | after3 / baseline |
|---|---|---|---|
| Nodes explored | 1,543 | 5,053 | 3.27x |
| Nodes drawn | 1,543 | 5,000 | 3.24x |
| Blocked at (nodes) | 1,543 | n/a | n/a |
| First graph (ms) | 471 | 431 | 0.92x |
| Expansions | 19 | 55 | 2.89x |
| Expand latency, median (ms) | 175 | 164 | 0.94x |
| Expand latency, max (ms) | 8,018 | 380 | 0.047x |
| Pan and zoom frame p50 (ms) | 16.7 | 350 | 20.96x |
| Pan and zoom frame p95 (ms) | 99.9 | 450 | 4.50x |
| Pan and zoom frame max (ms) | 150 | 450 | 3.00x |
| Pan and zoom frames in 5 s | 221 | 15 | 0.068x |
| Idle frame p50 (ms) | 16.7 | 16.7 | 1.00x |
| API requests | 49 | 236 | 4.82x |
| Long tasks (total ms) | 4,679 | 27,751 | 5.93x |
| JS heap (MB) | 31.5 | 83.7 | 2.66x |
| DOM nodes | 2,364 | 2,354 | 1.00x |

#### explore-fanout-20000

| Measure | baseline | after3 | after3 / baseline |
|---|---|---|---|
| Nodes explored | 153 | 4 | 0.026x |
| Nodes drawn | 153 | 4 | 0.026x |
| Blocked at (nodes) | n/a | n/a | n/a |
| First graph (ms) | 566 | 320 | 0.57x |
| Expansions | n/a | n/a | n/a |
| Expand latency, median (ms) | n/a | n/a | n/a |
| Expand latency, max (ms) | n/a | n/a | n/a |
| Pan and zoom frame p50 (ms) | n/a | n/a | n/a |
| Pan and zoom frame p95 (ms) | n/a | n/a | n/a |
| Pan and zoom frame max (ms) | n/a | n/a | n/a |
| Pan and zoom frames in 5 s | n/a | n/a | n/a |
| Idle frame p50 (ms) | n/a | n/a | n/a |
| API requests | 14 | 16 | 1.14x |
| Long tasks (total ms) | 352 | 97 | 0.28x |
| JS heap (MB) | 11.7 | 8.5 | 0.73x |
| DOM nodes | 2,798 | 2,782 | 0.99x |

#### open-chebi

| Measure | baseline | after3 | after3 / baseline |
|---|---|---|---|
| First row visible (ms) | 9,892 | 902 | 0.091x |
| Interactive (ms) | 9,892 | 902 | 0.091x |
| Network quiet at (ms) | 10,398 | 8,454 | 0.81x |
| Summary request (ms) | 7,846 | 7,496 | 0.96x |
| API requests | 23 | 9 | 0.39x |
| API bytes, decoded (MB) | 68.31 | 0.17 | 0.002x |
| API bytes, transferred (MB) | 3.79 | 0.02 | 0.004x |
| Static requests | 63 | 63 | 1.00x |
| Long tasks (count) | 6 | 1 | 0.17x |
| Long tasks (total ms) | 925 | 71 | 0.077x |
| Longest task (ms) | 352 | 71 | 0.20x |
| JS heap after load (MB) | 163.7 | 5.9 | 0.036x |
| DOM nodes | 19,589 | 2,033 | 0.10x |
| Rows loaded | 1,001 | 1,001 | 1.00x |
| Rows in DOM | 1,001 | 30 | 0.030x |
| Twisty click latency (ms) | 39 | n/a | n/a |
| $/assistant: requests | 1 | 1 | 1.00x |
| $/assistant: MB decoded | 0 | 0 | 1.00x |
| $/auth: requests | 1 | 1 | 1.00x |
| $/auth: MB decoded | 0 | 0 | 1.00x |
| $/branches: requests | 1 | 1 | 1.00x |
| $/branches: MB decoded | 0 | 0 | 1.00x |
| $/datasets: requests | 1 | 1 | 1.00x |
| $/datasets: MB decoded | 0.01 | 0.01 | 1.00x |
| $/memory: requests | 1 | 1 | 1.00x |
| $/memory: MB decoded | 0 | 0 | 1.00x |
| $/ping: requests | 1 | 1 | 1.00x |
| $/ping: MB decoded | 0 | 0 | 1.00x |
| $/prefixes: requests | 1 | 1 | 1.00x |
| $/prefixes: MB decoded | 0 | 0 | 1.00x |
| $/whoami: requests | 1 | 1 | 1.00x |
| $/whoami: MB decoded | 0 | 0 | 1.00x |
| changes: requests | 1 | 1 | 1.00x |
| changes: MB decoded | 0 | 0 | n/a |
| ontology/axioms: requests | 6 | n/a | n/a |
| ontology/axioms: MB decoded | 14.64 | n/a | n/a |
| ontology/entities: requests | 5 | n/a | n/a |
| ontology/entities: MB decoded | 3.29 | n/a | n/a |
| ontology/hierarchy: requests | 4 | 1 | 0.25x |
| ontology/hierarchy: MB decoded | 0.17 | 0.16 | 0.96x |
| ontology/summary: requests | 1 | 1 | 1.00x |
| ontology/summary: MB decoded | 0 | 0 | 1.00x |
| sparql: requests | 3 | 3 | 1.00x |
| sparql: MB decoded | 50.21 | 0 | 0.000x |

#### open-taxon1m

| Measure | baseline | after3 | after3 / baseline |
|---|---|---|---|
| First row visible (ms) | n/a | 897 | n/a |
| Interactive (ms) | n/a | 897 | n/a |
| Network quiet at (ms) | n/a | 14,631 | n/a |
| Summary request (ms) | n/a | 13,706 | n/a |
| API requests | n/a | 10 | n/a |
| API bytes, decoded (MB) | n/a | 0.01 | n/a |
| API bytes, transferred (MB) | n/a | 0.01 | n/a |
| Static requests | n/a | 63 | n/a |
| Long tasks (count) | n/a | 1 | n/a |
| Long tasks (total ms) | n/a | 68 | n/a |
| Longest task (ms) | n/a | 68 | n/a |
| JS heap after load (MB) | n/a | 4.8 | n/a |
| DOM nodes | n/a | 1,287 | n/a |
| Rows loaded | n/a | 1 | n/a |
| Rows in DOM | n/a | 1 | n/a |
| Twisty click latency (ms) | n/a | 41 | n/a |
| $/assistant: requests | n/a | 1 | n/a |
| $/assistant: MB decoded | n/a | 0 | n/a |
| $/auth: requests | n/a | 1 | n/a |
| $/auth: MB decoded | n/a | 0 | n/a |
| $/branches: requests | n/a | 1 | n/a |
| $/branches: MB decoded | n/a | 0 | n/a |
| $/datasets: requests | n/a | 1 | n/a |
| $/datasets: MB decoded | n/a | 0.01 | n/a |
| $/memory: requests | n/a | 1 | n/a |
| $/memory: MB decoded | n/a | 0 | n/a |
| $/ping: requests | n/a | 1 | n/a |
| $/ping: MB decoded | n/a | 0 | n/a |
| $/prefixes: requests | n/a | 2 | n/a |
| $/prefixes: MB decoded | n/a | 0 | n/a |
| $/whoami: requests | n/a | 1 | n/a |
| $/whoami: MB decoded | n/a | 0 | n/a |
| changes: requests | n/a | 1 | n/a |
| changes: MB decoded | n/a | 0 | n/a |
| ontology/hierarchy: requests | n/a | 1 | n/a |
| ontology/hierarchy: MB decoded | n/a | 0 | n/a |
| ontology/summary: requests | n/a | 1 | n/a |
| ontology/summary: MB decoded | n/a | 0 | n/a |
| sparql: requests | n/a | 3 | n/a |
| sparql: MB decoded | n/a | 0 | n/a |

#### open-taxon300k

| Measure | baseline | after3 | after3 / baseline |
|---|---|---|---|
| First row visible (ms) | 6,117 | 415 | 0.068x |
| Interactive (ms) | 6,117 | 415 | 0.068x |
| Network quiet at (ms) | 6,724 | 4,648 | 0.69x |
| Summary request (ms) | 4,385 | 3,693 | 0.84x |
| API requests | 24 | 10 | 0.42x |
| API bytes, decoded (MB) | 74.17 | 0.01 | 0.000x |
| API bytes, transferred (MB) | 2.84 | 0.01 | 0.004x |
| Static requests | 63 | 63 | 1.00x |
| Long tasks (count) | 4 | 1 | 0.25x |
| Long tasks (total ms) | 664 | 72 | 0.11x |
| Longest task (ms) | 299 | 72 | 0.24x |
| JS heap after load (MB) | 161.6 | 4.8 | 0.030x |
| DOM nodes | 1,288 | 1,287 | 1.00x |
| Rows loaded | 1 | 1 | 1.00x |
| Rows in DOM | 1 | 1 | 1.00x |
| Twisty click latency (ms) | 32 | 43 | 1.34x |
| $/assistant: requests | 1 | 1 | 1.00x |
| $/assistant: MB decoded | 0 | 0 | 1.00x |
| $/auth: requests | 1 | 1 | 1.00x |
| $/auth: MB decoded | 0 | 0 | 1.00x |
| $/branches: requests | 1 | 1 | 1.00x |
| $/branches: MB decoded | 0 | 0 | 1.00x |
| $/datasets: requests | 1 | 1 | 1.00x |
| $/datasets: MB decoded | 0.01 | 0.01 | 1.00x |
| $/memory: requests | 1 | 1 | 1.00x |
| $/memory: MB decoded | 0 | 0 | 1.00x |
| $/ping: requests | 1 | 1 | 1.00x |
| $/ping: MB decoded | 0 | 0 | 1.00x |
| $/prefixes: requests | 2 | 2 | 1.00x |
| $/prefixes: MB decoded | 0 | 0 | 1.00x |
| $/whoami: requests | 1 | 1 | 1.00x |
| $/whoami: MB decoded | 0 | 0 | 1.00x |
| changes: requests | 1 | 1 | 1.00x |
| changes: MB decoded | 0 | 0 | n/a |
| ontology/axioms: requests | 6 | n/a | n/a |
| ontology/axioms: MB decoded | 14.84 | n/a | n/a |
| ontology/entities: requests | 5 | n/a | n/a |
| ontology/entities: MB decoded | 3.48 | n/a | n/a |
| ontology/hierarchy: requests | 4 | 1 | 0.25x |
| ontology/hierarchy: MB decoded | 0 | 0 | 0.36x |
| ontology/summary: requests | 1 | 1 | 1.00x |
| ontology/summary: MB decoded | 0 | 0 | 1.00x |
| sparql: requests | 3 | 3 | 1.00x |
| sparql: MB decoded | 55.86 | 0 | 0.000x |

#### open-unesco-thesaurus

| Measure | baseline | after3 | after3 / baseline |
|---|---|---|---|
| First row visible (ms) | 432 | 355 | 0.82x |
| Interactive (ms) | 432 | 355 | 0.82x |
| Network quiet at (ms) | 1,038 | 1,018 | 0.98x |
| Summary request (ms) | n/a | n/a | n/a |
| API requests | 14 | 14 | 1.00x |
| API bytes, decoded (MB) | 0.8 | 0.8 | 1.00x |
| API bytes, transferred (MB) | 0.07 | 0.07 | 1.00x |
| Static requests | 67 | 67 | 1.00x |
| Long tasks (count) | 1 | 0 | 0.000x |
| Long tasks (total ms) | 137 | 0 | 0.000x |
| Longest task (ms) | 87 | 0 | 0.000x |
| JS heap after load (MB) | 11.6 | 4.9 | 0.42x |
| DOM nodes | 16,550 | 1,718 | 0.10x |
| Rows loaded | 680 | 680 | 1.00x |
| Rows in DOM | 680 | 23 | 0.034x |
| Twisty click latency (ms) | 71 | 49 | 0.69x |
| $/assistant: requests | 1 | 1 | 1.00x |
| $/assistant: MB decoded | 0 | 0 | 1.00x |
| $/auth: requests | 1 | 1 | 1.00x |
| $/auth: MB decoded | 0 | 0 | 1.00x |
| $/branches: requests | 1 | 1 | 1.00x |
| $/branches: MB decoded | 0 | 0 | 1.00x |
| $/datasets: requests | 1 | 1 | 1.00x |
| $/datasets: MB decoded | 0.01 | 0.01 | 1.00x |
| $/memory: requests | 1 | 1 | 1.00x |
| $/memory: MB decoded | 0 | 0 | 1.00x |
| $/ping: requests | 1 | 1 | 1.00x |
| $/ping: MB decoded | 0 | 0 | 1.00x |
| $/prefixes: requests | 2 | 2 | 1.00x |
| $/prefixes: MB decoded | 0 | 0 | 1.00x |
| $/whoami: requests | 1 | 1 | 1.00x |
| $/whoami: MB decoded | 0 | 0 | 1.00x |
| /unesco/data: requests | 1 | 1 | 1.00x |
| /unesco/data: MB decoded | 0 | 0 | n/a |
| changes: requests | 1 | 1 | 1.00x |
| changes: MB decoded | 0 | 0 | n/a |
| sparql: requests | 8 | 8 | 1.00x |
| sparql: MB decoded | 0.8 | 0.8 | 1.00x |

#### reveal-chebi-water

| Measure | baseline | after3 | after3 / baseline |
|---|---|---|---|
| First row visible (ms) | 2,661 | 363 | 0.14x |
| Interactive (ms) | 2,661 | 363 | 0.14x |
| Network quiet at (ms) | 2,916 | 1,425 | 0.49x |
| Summary request (ms) | 47 | 73 | 1.55x |
| API requests | 63 | 34 | 0.54x |
| API bytes, decoded (MB) | 68.58 | 0.25 | 0.004x |
| API bytes, transferred (MB) | 3.83 | 0.05 | 0.013x |
| Static requests | 63 | 63 | 1.00x |
| Long tasks (count) | 7 | 1 | 0.14x |
| Long tasks (total ms) | 1,325 | 74 | 0.056x |
| Longest task (ms) | 749 | 74 | 0.099x |
| JS heap after load (MB) | 196.8 | 7.3 | 0.037x |
| DOM nodes | 226,955 | 3,421 | 0.015x |
| Rows loaded | 1,289 | 1,289 | 1.00x |
| Rows in DOM | 1,289 | 39 | 0.030x |
| Twisty click latency (ms) | 37 | 57 | 1.54x |
| $/assistant: requests | 1 | 1 | 1.00x |
| $/assistant: MB decoded | 0 | 0 | 1.00x |
| $/auth: requests | 1 | 1 | 1.00x |
| $/auth: MB decoded | 0 | 0 | 1.00x |
| $/branches: requests | 1 | 1 | 1.00x |
| $/branches: MB decoded | 0 | 0 | 1.00x |
| $/datasets: requests | 1 | 1 | 1.00x |
| $/datasets: MB decoded | 0.01 | 0.01 | 1.00x |
| $/memory: requests | 1 | 1 | 1.00x |
| $/memory: MB decoded | 0 | 0 | 1.00x |
| $/ping: requests | 1 | 1 | 1.00x |
| $/ping: MB decoded | 0 | 0 | 1.00x |
| $/prefixes: requests | 1 | 1 | 1.00x |
| $/prefixes: MB decoded | 0 | 0 | 1.00x |
| $/whoami: requests | 1 | 1 | 1.00x |
| $/whoami: MB decoded | 0 | 0 | 1.00x |
| changes: requests | 1 | 1 | 1.00x |
| changes: MB decoded | 0 | 0 | n/a |
| ontology/axioms: requests | 7 | 1 | 0.14x |
| ontology/axioms: MB decoded | 14.66 | 0.02 | 0.001x |
| ontology/entities: requests | 5 | 1 | 0.20x |
| ontology/entities: MB decoded | 3.29 | 0 | 0.001x |
| ontology/entity: requests | 1 | 1 | 1.00x |
| ontology/entity: MB decoded | 0.01 | 0.01 | 1.00x |
| ontology/hierarchy: requests | 43 | 21 | 0.49x |
| ontology/hierarchy: MB decoded | 0.42 | 0.21 | 0.51x |
| ontology/summary: requests | 1 | 1 | 1.00x |
| ontology/summary: MB decoded | 0 | 0 | 1.00x |
| sparql: requests | 2 | 5 | 2.50x |
| sparql: MB decoded | 50.21 | 0.01 | 0.000x |

#### reveal-taxon1m-deepest

| Measure | baseline | after3 | after3 / baseline |
|---|---|---|---|
| First row visible (ms) | n/a | 889 | n/a |
| Interactive (ms) | n/a | 889 | n/a |
| Network quiet at (ms) | n/a | 29,842 | n/a |
| Summary request (ms) | n/a | 16,397 | n/a |
| API requests | n/a | 57 | n/a |
| API bytes, decoded (MB) | n/a | 0.06 | n/a |
| API bytes, transferred (MB) | n/a | 0.07 | n/a |
| Static requests | n/a | 63 | n/a |
| Long tasks (count) | n/a | 1 | n/a |
| Long tasks (total ms) | n/a | 57 | n/a |
| Longest task (ms) | n/a | 57 | n/a |
| JS heap after load (MB) | n/a | 6.2 | n/a |
| DOM nodes | n/a | 2,473 | n/a |
| Rows loaded | n/a | 78 | n/a |
| Rows in DOM | n/a | 38 | n/a |
| Twisty click latency (ms) | n/a | 43 | n/a |
| $/assistant: requests | n/a | 1 | n/a |
| $/assistant: MB decoded | n/a | 0 | n/a |
| $/auth: requests | n/a | 1 | n/a |
| $/auth: MB decoded | n/a | 0 | n/a |
| $/branches: requests | n/a | 1 | n/a |
| $/branches: MB decoded | n/a | 0 | n/a |
| $/datasets: requests | n/a | 1 | n/a |
| $/datasets: MB decoded | n/a | 0.01 | n/a |
| $/memory: requests | n/a | 1 | n/a |
| $/memory: MB decoded | n/a | 0 | n/a |
| $/ping: requests | n/a | 1 | n/a |
| $/ping: MB decoded | n/a | 0 | n/a |
| $/prefixes: requests | n/a | 2 | n/a |
| $/prefixes: MB decoded | n/a | 0 | n/a |
| $/whoami: requests | n/a | 1 | n/a |
| $/whoami: MB decoded | n/a | 0 | n/a |
| changes: requests | n/a | 1 | n/a |
| changes: MB decoded | n/a | 0 | n/a |
| ontology/axioms: requests | n/a | 1 | n/a |
| ontology/axioms: MB decoded | n/a | 0 | n/a |
| ontology/entities: requests | n/a | 1 | n/a |
| ontology/entities: MB decoded | n/a | 0 | n/a |
| ontology/entity: requests | n/a | 1 | n/a |
| ontology/entity: MB decoded | n/a | 0 | n/a |
| ontology/hierarchy: requests | n/a | 43 | n/a |
| ontology/hierarchy: MB decoded | n/a | 0.05 | n/a |
| ontology/summary: requests | n/a | 1 | n/a |
| ontology/summary: MB decoded | n/a | 0 | n/a |
| sparql: requests | n/a | 5 | n/a |
| sparql: MB decoded | n/a | 0 | n/a |

#### reveal-taxon1m-past-first-page

| Measure | baseline | after3 | after3 / baseline |
|---|---|---|---|
| First row visible (ms) | n/a | 7,353 | n/a |
| Interactive (ms) | n/a | 15,740 | n/a |
| Network quiet at (ms) | n/a | 27,429 | n/a |
| Summary request (ms) | n/a | 14,125 | n/a |
| API requests | n/a | 67 | n/a |
| API bytes, decoded (MB) | n/a | 8.37 | n/a |
| API bytes, transferred (MB) | n/a | 0.53 | n/a |
| Static requests | n/a | 63 | n/a |
| Long tasks (count) | n/a | 40 | n/a |
| Long tasks (total ms) | n/a | 4,263 | n/a |
| Longest task (ms) | n/a | 282 | n/a |
| JS heap after load (MB) | n/a | 38 | n/a |
| DOM nodes | n/a | 2,277 | n/a |
| Rows loaded | n/a | 47,038 | n/a |
| Rows in DOM | n/a | 39 | n/a |
| Twisty click latency (ms) | n/a | 340 | n/a |
| $/assistant: requests | n/a | 1 | n/a |
| $/assistant: MB decoded | n/a | 0 | n/a |
| $/auth: requests | n/a | 1 | n/a |
| $/auth: MB decoded | n/a | 0 | n/a |
| $/branches: requests | n/a | 1 | n/a |
| $/branches: MB decoded | n/a | 0 | n/a |
| $/datasets: requests | n/a | 1 | n/a |
| $/datasets: MB decoded | n/a | 0.01 | n/a |
| $/memory: requests | n/a | 1 | n/a |
| $/memory: MB decoded | n/a | 0 | n/a |
| $/ping: requests | n/a | 1 | n/a |
| $/ping: MB decoded | n/a | 0 | n/a |
| $/prefixes: requests | n/a | 2 | n/a |
| $/prefixes: MB decoded | n/a | 0 | n/a |
| $/whoami: requests | n/a | 1 | n/a |
| $/whoami: MB decoded | n/a | 0 | n/a |
| changes: requests | n/a | 1 | n/a |
| changes: MB decoded | n/a | 0 | n/a |
| ontology/axioms: requests | n/a | 1 | n/a |
| ontology/axioms: MB decoded | n/a | 0 | n/a |
| ontology/entities: requests | n/a | 1 | n/a |
| ontology/entities: MB decoded | n/a | 0 | n/a |
| ontology/entity: requests | n/a | 1 | n/a |
| ontology/entity: MB decoded | n/a | 0 | n/a |
| ontology/hierarchy: requests | n/a | 53 | n/a |
| ontology/hierarchy: MB decoded | n/a | 8.36 | n/a |
| ontology/summary: requests | n/a | 1 | n/a |
| ontology/summary: MB decoded | n/a | 0 | n/a |
| sparql: requests | n/a | 5 | n/a |
| sparql: MB decoded | n/a | 0 | n/a |

#### reveal-taxon300k-deepest

| Measure | baseline | after3 | after3 / baseline |
|---|---|---|---|
| First row visible (ms) | 2,494 | 401 | 0.16x |
| Interactive (ms) | 2,494 | 401 | 0.16x |
| Network quiet at (ms) | 2,747 | 1,368 | 0.50x |
| Summary request (ms) | 23 | 31 | 1.35x |
| API requests | 108 | 57 | 0.53x |
| API bytes, decoded (MB) | 74.25 | 0.06 | 0.001x |
| API bytes, transferred (MB) | 2.93 | 0.07 | 0.024x |
| Static requests | 63 | 63 | 1.00x |
| Long tasks (count) | 4 | 1 | 0.25x |
| Long tasks (total ms) | 967 | 75 | 0.078x |
| Longest task (ms) | 614 | 75 | 0.12x |
| JS heap after load (MB) | 196.2 | 6.2 | 0.032x |
| DOM nodes | 203,665 | 2,514 | 0.012x |
| Rows loaded | 78 | 78 | 1.00x |
| Rows in DOM | 77 | 38 | 0.49x |
| Twisty click latency (ms) | n/a | 41 | n/a |
| $/assistant: requests | 1 | 1 | 1.00x |
| $/assistant: MB decoded | 0 | 0 | 1.00x |
| $/auth: requests | 1 | 1 | 1.00x |
| $/auth: MB decoded | 0 | 0 | 1.00x |
| $/branches: requests | 1 | 1 | 1.00x |
| $/branches: MB decoded | 0 | 0 | 1.00x |
| $/datasets: requests | 1 | 1 | 1.00x |
| $/datasets: MB decoded | 0.01 | 0.01 | 1.00x |
| $/memory: requests | 1 | 1 | 1.00x |
| $/memory: MB decoded | 0 | 0 | 1.00x |
| $/ping: requests | 1 | 1 | 1.00x |
| $/ping: MB decoded | 0 | 0 | 1.00x |
| $/prefixes: requests | 2 | 2 | 1.00x |
| $/prefixes: MB decoded | 0 | 0 | 1.00x |
| $/whoami: requests | 1 | 1 | 1.00x |
| $/whoami: MB decoded | 0 | 0 | 1.00x |
| changes: requests | 1 | 1 | 1.00x |
| changes: MB decoded | 0 | 0 | n/a |
| ontology/axioms: requests | 7 | 1 | 0.14x |
| ontology/axioms: MB decoded | 14.84 | 0 | 0.000x |
| ontology/entities: requests | 5 | 1 | 0.20x |
| ontology/entities: MB decoded | 3.48 | 0 | 0.000x |
| ontology/entity: requests | 1 | 1 | 1.00x |
| ontology/entity: MB decoded | 0 | 0 | 1.00x |
| ontology/hierarchy: requests | 87 | 43 | 0.49x |
| ontology/hierarchy: MB decoded | 0.08 | 0.05 | 0.64x |
| ontology/summary: requests | 1 | 1 | 1.00x |
| ontology/summary: MB decoded | 0 | 0 | 1.00x |
| sparql: requests | 2 | 5 | 2.50x |
| sparql: MB decoded | 55.86 | 0 | 0.000x |

#### reveal-taxon300k-past-first-page

| Measure | baseline | after3 | after3 / baseline |
|---|---|---|---|
| First row visible (ms) | 4,837 | 917 | 0.19x |
| Interactive (ms) | 4,837 | 917 | 0.19x |
| Network quiet at (ms) | 5,242 | 1,573 | 0.30x |
| Summary request (ms) | 19 | 32 | 1.68x |
| API requests | 56 | 31 | 0.55x |
| API bytes, decoded (MB) | 78.08 | 1.96 | 0.025x |
| API bytes, transferred (MB) | 3 | 0.16 | 0.055x |
| Static requests | 63 | 63 | 1.00x |
| Long tasks (count) | 20 | 2 | 0.10x |
| Long tasks (total ms) | 3,416 | 115 | 0.034x |
| Longest task (ms) | 1,685 | 63 | 0.037x |
| JS heap after load (MB) | 302.9 | 16.2 | 0.053x |
| DOM nodes | 432,437 | 2,388 | 0.006x |
| Rows loaded | 11,038 | 11,038 | 1.00x |
| Rows in DOM | 11,038 | 39 | 0.004x |
| Twisty click latency (ms) | n/a | 136 | n/a |
| $/assistant: requests | 1 | 1 | 1.00x |
| $/assistant: MB decoded | 0 | 0 | 1.00x |
| $/auth: requests | 1 | 1 | 1.00x |
| $/auth: MB decoded | 0 | 0 | 1.00x |
| $/branches: requests | 1 | 1 | 1.00x |
| $/branches: MB decoded | 0 | 0 | 1.00x |
| $/datasets: requests | 1 | 1 | 1.00x |
| $/datasets: MB decoded | 0.01 | 0.01 | 1.00x |
| $/memory: requests | 1 | 1 | 1.00x |
| $/memory: MB decoded | 0 | 0 | 1.00x |
| $/ping: requests | 1 | 1 | 1.00x |
| $/ping: MB decoded | 0 | 0 | 1.00x |
| $/prefixes: requests | 2 | 2 | 1.00x |
| $/prefixes: MB decoded | 0 | 0 | 1.00x |
| $/whoami: requests | 1 | 1 | 1.00x |
| $/whoami: MB decoded | 0 | 0 | 1.00x |
| changes: requests | 1 | 1 | 1.00x |
| changes: MB decoded | 0 | 0 | n/a |
| ontology/axioms: requests | 7 | 1 | 0.14x |
| ontology/axioms: MB decoded | 14.84 | 0 | 0.000x |
| ontology/entities: requests | 5 | 1 | 0.20x |
| ontology/entities: MB decoded | 3.48 | 0 | 0.000x |
| ontology/entity: requests | 1 | 1 | 1.00x |
| ontology/entity: MB decoded | 0 | 0 | 1.00x |
| ontology/hierarchy: requests | 35 | 17 | 0.49x |
| ontology/hierarchy: MB decoded | 3.91 | 1.96 | 0.50x |
| ontology/summary: requests | 1 | 1 | 1.00x |
| ontology/summary: MB decoded | 0 | 0 | 1.00x |
| sparql: requests | 2 | 5 | 2.50x |
| sparql: MB decoded | 55.86 | 0 | 0.000x |

#### scroll-taxon1m-10k

| Measure | baseline | after3 | after3 / baseline |
|---|---|---|---|
| Rows loaded | n/a | 10,038 | n/a |
| Rows in DOM before scroll | n/a | 38 | n/a |
| Rows in DOM after scroll | n/a | 30 | n/a |
| Scroll height (px) | n/a | 260,998 | n/a |
| scrollDown: frames | n/a | 372 | n/a |
| scrollDown: frame p50 (ms) | n/a | 16.7 | n/a |
| scrollDown: frame p95 (ms) | n/a | 16.7 | n/a |
| scrollDown: frame max (ms) | n/a | 16.8 | n/a |
| scrollUp: frames | n/a | 36 | n/a |
| scrollUp: frame p50 (ms) | n/a | 16.7 | n/a |
| scrollUp: frame p95 (ms) | n/a | 16.7 | n/a |
| scrollUp: frame max (ms) | n/a | 50 | n/a |
| JS heap before scroll (MB) | n/a | 14.7 | n/a |
| JS heap after scroll (MB) | n/a | 12 | n/a |
| DOM nodes after scroll | n/a | 1,740 | n/a |

#### scroll-taxon300k-10k

| Measure | baseline | after3 | after3 / baseline |
|---|---|---|---|
| Rows loaded | 10,038 | 10,038 | 1.00x |
| Rows in DOM before scroll | 10,038 | 38 | 0.004x |
| Rows in DOM after scroll | 10,038 | 30 | 0.003x |
| Scroll height (px) | 260,998 | 260,998 | 1.00x |
| scrollDown: frames | 372 | 372 | 1.00x |
| scrollDown: frame p50 (ms) | 33.3 | 16.7 | 0.50x |
| scrollDown: frame p95 (ms) | 50 | 16.7 | 0.33x |
| scrollDown: frame max (ms) | 66.7 | 16.8 | 0.25x |
| scrollUp: frames | 372 | 372 | 1.00x |
| scrollUp: frame p50 (ms) | 33.3 | 16.7 | 0.50x |
| scrollUp: frame p95 (ms) | 50 | 16.7 | 0.33x |
| scrollUp: frame max (ms) | 50.1 | 16.8 | 0.34x |
| JS heap before scroll (MB) | 293 | 14.5 | 0.049x |
| JS heap after scroll (MB) | 293.1 | 16.3 | 0.056x |
| DOM nodes after scroll | 411,869 | 2,557 | 0.006x |
