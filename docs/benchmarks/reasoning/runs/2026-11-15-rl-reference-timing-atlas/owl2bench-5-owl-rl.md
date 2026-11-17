# OWL 2 RL materialization, owl2bench(5)

349607 base triples, 52966139 derived triples, equal on both engines in every run. The change of the incremental runs has 3004 triples. nemo-cli 0.10.1 and sparkles 0.1.0 at faaddcdb on atlas.

| Run | Threads | Status | Median seconds | Nemo reasoning seconds | Peak RSS (MiB) | Derived triples |
|---|---:|---|---:|---:|---:|---:|
| nemo-full | 1 | ok | 752.78 | 680.03 | 2933 | 52966139 |
| sparkles-full | 1 | ok | 72.01 |  | 9979 | 52966139 |
| sparkles-full | 20 | ok | 55.02 |  | 9808 | 52966139 |
