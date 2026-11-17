# OWL 2 RL materialization, lubm(10)

1272868 base triples, 1038100 derived triples, equal on both engines in every run. The change of the incremental runs has 10528 triples. nemo-cli 0.10.1 and sparkles 0.1.0 at faaddcdb on atlas.

| Run | Threads | Status | Median seconds | Nemo reasoning seconds | Peak RSS (MiB) | Derived triples |
|---|---:|---|---:|---:|---:|---:|
| nemo-full | 1 | ok | 27.34 | 18.71 | 437 | 1038100 |
| sparkles-full | 1 | ok | 2.14 |  | 652 | 1038100 |
| sparkles-full | 20 | ok | 1.46 |  | 921 | 1038100 |
