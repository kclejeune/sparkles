# RDFS materialization, lubm(10)

1272868 base triples, 526316 derived triples, equal on both engines in every run. The change of the incremental runs has 10528 triples. nemo-cli 0.10.1 and sparkles 0.1.0 at faaddcdb on atlas.

| Run | Threads | Status | Median seconds | Nemo reasoning seconds | Peak RSS (MiB) | Derived triples |
|---|---:|---|---:|---:|---:|---:|
| nemo-full | 1 | ok | 8.33 | 3.60 | 229 | 526316 |
| sparkles-full | 1 | ok | 2.55 |  | 587 | 526316 |
| sparkles-full | 20 | ok | 2.31 |  | 812 | 526316 |
