# OWL 2 RL materialization, lubm(1)

100836 base triples, 85098 derived triples, equal on both engines. The change of the incremental runs has 824 triples. nemo-cli 0.10.1 on stanley.

| Run | Threads | Status | Median seconds | Nemo reasoning seconds | Peak RSS (MiB) |
|---|---:|---|---:|---:|---:|
| sparkles-full | 1 | ok | 0.25 | | 176 |
| sparkles-full | 16 | ok | 0.20 | | 333 |
| nemo-full | 1 | ok | 1.83 | 1.18 | 51 |
| nemo-minus | 1 | ok | 1.78 | 1.12 | 50 |
| sparkles-insert | 16 | ok | 0.17 | | 215 |
| sparkles-delete | 16 | ok | 0.18 | | 223 |
