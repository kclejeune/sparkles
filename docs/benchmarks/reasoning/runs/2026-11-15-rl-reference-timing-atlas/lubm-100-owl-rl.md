# OWL 2 RL materialization, lubm(100)

13405674 base triples, 10907379 derived triples, equal on both engines in every run. The change of the incremental runs has 110966 triples. nemo-cli 0.10.1 and sparkles 0.1.0 at faaddcdb on atlas.

| Run | Threads | Status | Median seconds | Nemo reasoning seconds | Peak RSS (MiB) | Derived triples |
|---|---:|---|---:|---:|---:|---:|
| nemo-full | 1 | ok | 304.34 | 202.41 | 4230 | 10907379 |
| sparkles-full | 1 | ok | 32.77 |  | 3571 | 10907379 |
| sparkles-full | 20 | ok | 21.11 |  | 3713 | 10907379 |
