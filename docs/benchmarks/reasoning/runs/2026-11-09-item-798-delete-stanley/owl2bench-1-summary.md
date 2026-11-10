# OWL 2 RL materialization, owl2bench(1)

54815 base triples, 1400688 derived triples, equal on both engines. The change of the incremental runs has 462 triples. nemo-cli 0.10.1 on stanley.

| Run | Threads | Status | Median seconds | Nemo reasoning seconds | Peak RSS (MiB) |
|---|---:|---|---:|---:|---:|
| sparkles-full | 1 | ok | 4.23 | | 545 |
| sparkles-full | 16 | ok | 3.75 | | 640 |
| nemo-full | 1 | ok | 37.86 | 33.87 | 128 |
| nemo-minus | 1 | ok | 31.34 | 27.93 | 127 |
| sparkles-insert | 16 | ok | 1.49 | | 469 |
| sparkles-delete | 16 | ok | 2.91 | | 865 |
