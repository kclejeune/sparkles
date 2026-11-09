# OWL 2 RL materialization, owl2bench(1)

54815 base triples, 1400688 derived triples, equal on both engines. The change of the incremental runs has 462 triples. nemo-cli 0.10.1 on stanley.

| Run | Threads | Status | Median seconds | Nemo reasoning seconds | Peak RSS (MiB) |
|---|---:|---|---:|---:|---:|
| sparkles-full | 1 | ok | 1.44 | | 462 |
| sparkles-full | 16 | ok | 1.00 | | 768 |
| nemo-full | 1 | ok | 14.88 | 13.09 | 129 |
| nemo-minus | 1 | ok | 14.50 | 12.74 | 129 |
| sparkles-insert | 16 | ok | 0.53 | | 474 |
| sparkles-delete | 16 | oom |  | |  |
