# RDFS materialization, owl2bench(1)

54815 base triples, 23335 derived triples, equal on both engines in every run. The change of the incremental runs has 462 triples. nemo-cli 0.10.1 and sparkles 0.1.0 at faaddcdb on atlas.

| Run | Threads | Status | Median seconds | Nemo reasoning seconds | Peak RSS (MiB) | Derived triples |
|---|---:|---|---:|---:|---:|---:|
| nemo-full | 1 | ok | 0.48 | 0.19 | 22 | 23335 |
| sparkles-full | 1 | ok | 0.11 |  | 124 | 23335 |
| sparkles-full | 20 | ok | 0.12 |  | 221 | 23335 |
