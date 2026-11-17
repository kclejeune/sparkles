# RDFS materialization, lubm(100)

13405674 base triples, 5527675 derived triples, equal on both engines in every run. The change of the incremental runs has 110966 triples. nemo-cli 0.10.1 and sparkles 0.1.0 at faaddcdb on atlas.

| Run | Threads | Status | Median seconds | Nemo reasoning seconds | Peak RSS (MiB) | Derived triples |
|---|---:|---|---:|---:|---:|---:|
| nemo-full | 1 | ok | 92.95 | 38.77 | 2234 | 5527675 |
| sparkles-full | 1 | ok | 19.54 |  | 2681 | 5527675 |
| sparkles-full | 20 | ok | 15.03 |  | 2861 | 5527675 |
