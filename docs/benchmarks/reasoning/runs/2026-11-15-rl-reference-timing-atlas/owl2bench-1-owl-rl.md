# OWL 2 RL materialization, owl2bench(1)

54815 base triples, 1400688 derived triples, equal on both engines in every run. The change of the incremental runs has 462 triples. nemo-cli 0.10.1 and sparkles 0.1.0 at faaddcdb on atlas.

| Run | Threads | Status | Median seconds | Nemo reasoning seconds | Peak RSS (MiB) | Derived triples |
|---|---:|---|---:|---:|---:|---:|
| nemo-full | 1 | ok | 19.62 | 17.39 | 129 | 1400688 |
| sparkles-full | 1 | ok | 1.36 |  | 492 | 1400688 |
| sparkles-full | 20 | ok | 1.11 |  | 792 | 1400688 |
