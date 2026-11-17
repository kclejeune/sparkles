# RDFS materialization, owl2bench(5)

349607 base triples, 148189 derived triples, equal on both engines in every run. The change of the incremental runs has 3004 triples. nemo-cli 0.10.1 and sparkles 0.1.0 at faaddcdb on atlas.

| Run | Threads | Status | Median seconds | Nemo reasoning seconds | Peak RSS (MiB) | Derived triples |
|---|---:|---|---:|---:|---:|---:|
| nemo-full | 1 | ok | 2.83 | 1.12 | 75 | 148189 |
| sparkles-full | 1 | ok | 0.54 |  | 323 | 148189 |
| sparkles-full | 20 | ok | 0.47 |  | 560 | 148189 |
