# G08: Converting a Fuseki configuration

> **Status:** implemented
>
> **Phases:** One phase, shipped on 2026-10-03: `sparkles fuseki-config convert`, its
> `--check` report, and `serve --fuseki-config`.
>
> **User docs:** [Usage: Migrating from Fuseki](../USAGE.md#migrating-from-fuseki) ·
> [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui) ·
> [Comparison with Jena](../COMPARISON.md#vs-apache-jena--fuseki)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This spec draws on the Sparkles code and on Apache Jena's sources (Apache-2.0): Fuseki's
configuration reader (`FusekiConfig`, `FusekiServer.Builder`, `FusekiVocab`, `Auth`,
`FMod_Shiro`), the assemblers of TDB2, text indexes (`TextVocab`), GeoSPARQL
(`GeoAssembler`, `VocabGeoSPARQL`) and graph access control (`VocabSecurity`), the
example configurations in `jena-fuseki2/examples` and the test configurations in
`jena-fuseki2/jena-fuseki-main/testing`. Apache Shiro's documentation of `shiro.ini` and
Eclipse Jetty's documentation of its realm properties file describe the user files.

## 1. Summary

A Fuseki deployment is described by assembler files. `config.ttl` names the server and
its services, and Fuseki's full server also reads every file in `run/configuration/`.
The services name their endpoints and their datasets, and the datasets are TDB2
databases, in-memory datasets, text-indexed datasets, GeoSPARQL datasets, inference
models, RDFS views or access-controlled views. Users and passwords come from `shiro.ini`
or from the password file that `fuseki:passwd` names, and `fuseki:allowedUsers` limits
who may use a service or an endpoint.

Sparkles reads the part of such a description that maps to one dataset in
`POST /$/datasets` and refuses the rest. Migrating a deployment therefore means reading
each file and translating it by hand into `serve` flags, settings files and an auth
configuration. This spec adds a converter that does the translation and says what it
could not translate:

```
sparkles fuseki-config convert config.ttl --out sparkles/
sparkles fuseki-config convert run/ --check
sparkles serve --fuseki-config config.ttl
```

Goals:
- Every element of a Fuseki configuration is accounted for in a report, as converted,
  approximated, a manual step, ignored or unsupported. Nothing is dropped silently.
- The output starts a Sparkles server that serves the same datasets at the same URLs,
  with the same text and spatial indexes, inference, timeouts and access rules, as far
  as Sparkles has them.
- Access never widens. Where Sparkles cannot express a Fuseki rule exactly, the converted
  rule grants less, and the report says so.
- Passwords are hashed with argon2id when they are in plain text. A password never
  appears in the report, in logs or in any file other than the hashed auth
  configuration.
- The exit status tells a script whether something important could not be converted.

Non-goals:
- Copying data out of TDB2 or TDB1 databases. Sparkles cannot read their files. The
  output says how to export each one with Jena's `tdb2.tdbdump` and load it.
- Custom Java code: `ja:loadClass`, `fuseki:implementation` and custom operations.
- A general assembler interpreter. Graphs assembled from parts of other datasets
  (`ja:UnionModel`, a `tdb2:GraphTDB2` that renames a graph) have no Sparkles equivalent.
- Shiro realms other than the `[users]` section of `shiro.ini`, such as LDAP. The report
  points to OIDC or a forward-auth proxy instead.

## 2. Input

`convert` takes one or more paths. A file is read as RDF in the syntax its extension
names (Turtle, TriG, N-Triples, N-Quads, RDF/XML or JSON-LD). A directory is read as a
Fuseki base directory: its `config.ttl`, every RDF file in its `configuration/`
subdirectory, and its `shiro.ini`. When the input is a file, a `shiro.ini` and a
`configuration/` directory next to it are read too, and the report names them.
`--shiro FILE` and `--passwd FILE` name the user files explicitly.

Each file is parsed into its own graph with its own blank nodes, as Fuseki does. The
server resource (`rdf:type fuseki:Server`) may be in any one file, and a second one is an
error. Services are those that the server lists in `fuseki:services`, else every
`fuseki:Service` of every file. Relative paths in `ja:data`, `ja:externalContent`,
`ja:rdfsSchema`, `ja:rulesFrom` and `fuseki:passwd` are resolved against the directory
of the file that names them, and the report shows the resolved path. Fuseki resolves
some of them against its working directory, which is usually the same directory.

## 3. Output

`convert` writes into `--out DIR` (default `sparkles-config`), which must be empty or
missing unless `--force` is given:

| File | Content |
|---|---|
| `serve.sh` | `sparkles serve` with the flags of §4, run from its own directory. Extra arguments are passed on. |
| `load.sh` | The steps that move the data: `tdb2.tdbdump` and `sparkles load` for each TDB database, `sparkles load` for data files, and `sparkles infer` for inference. |
| `auth.toml` | The auth configuration of §6, mode `0600`, when the configuration has users or access rules. |
| `datasets/NAME/text.json` | The full-text configuration of §5.3. |
| `datasets/NAME/geo.json` | The spatial index configuration of §5.4. |
| `datasets/NAME/*` | Copies of the RDFS schema, inference schema and rule files a dataset uses. |
| `report.txt` | The report of §7. |

Persistent datasets live in `db/NAME` under the output directory and are served with
`--loc NAME=db/NAME`. In-memory datasets are served with `--mem NAME`. `--check` writes
nothing and only prints the report. `--format json` prints the report as JSON.

`serve --fuseki-config PATH` runs the same conversion when the server starts and applies
the result directly. It refuses to start when the report has an unsupported item. It
loads the data files of in-memory datasets at each start, as Fuseki does. Its persistent
datasets live in `<data>/fuseki/NAME`, and the auth configuration it generates is
written to `<data>/fuseki/auth.toml` unless `--auth-config` is given. Flags given on the
command line win over the converted ones.

## 4. Server settings

| Fuseki | Sparkles | Report |
|---|---|---|
| `fuseki:Server` with `fuseki:services` | the datasets of the listed services | converted |
| `ja:context [ ja:cxtName "arq:queryTimeout" ; ja:cxtValue "N" ]` | `--timeout N/1000` | converted |
| `arq:queryTimeout "N,M"` (first result, overall) | `--timeout M/1000` | approximated: Sparkles has no time limit for the first result |
| `arq:updateTimeout` | `--update-timeout` | converted |
| `tdb:unionDefaultGraph`, `tdb2:unionDefaultGraph` in a context | `--union-default-graph` | converted when it holds for every dataset |
| `fuseki:contextPath "/ABC"` | none | unsupported: datasets are served at the root, so a reverse proxy has to strip the prefix |
| `fuseki:pingEP`, `fuseki:statsEP`, `fuseki:metricsEP`, `fuseki:compactEP` | always served; `metricsEP true` adds `--metrics-fuseki-names` | converted |
| `fuseki:passwd`, `fuseki:realm`, `fuseki:auth "basic"` | §6 | converted |
| `fuseki:auth "digest"` | Basic | approximated |
| `fuseki:allowedUsers` | §6 | converted |
| `ja:loadClass` | none | unsupported |
| other `ja:context` symbols | none | ignored, each named |

Fuseki listens on every interface and Sparkles on loopback. `serve.sh` keeps Sparkles'
default and the report says how to listen on the network, which needs `--auth-config` or
`--allow-open-network` (see [Network exposure](../USAGE.md#network-exposure)).

A timeout in the context of a dataset or an endpoint applies to that dataset in Fuseki.
Sparkles' timeouts are per server, so the converter takes the server's value when there
is one, else the largest of the others, and reports the difference as approximated.

## 5. Services and datasets

### 5.1 Endpoints

Endpoints come from `fuseki:endpoint` and from the older `fuseki:serviceQuery`,
`fuseki:serviceUpdate`, `fuseki:serviceUpload`, `fuseki:serviceShacl`,
`fuseki:serviceReadGraphStore` and `fuseki:serviceReadWriteGraphStore`. Sparkles serves
the standard operations at fixed names, the same table that the assembler bodies of
`POST /$/datasets` use. Queries are served at the dataset URL, `sparql` and `query`.
Updates are served at the dataset URL and `update`. `gsp-rw` is served at the dataset
URL and `data`, `gsp-r` also at `get`, `patch` at the dataset URL and `patch`, `upload`
and `shacl` at their names, and `prefixes-r` and `prefixes-rw` at `prefixes`. An endpoint
at one of these names converts. Any other name is unsupported, because clients would
find nothing at its URL. The report names the URL Sparkles serves instead.
`gsp-direct-r` and `gsp-direct-rw` turn on `--gsp-direct-naming`, which applies to every
dataset. A custom operation or `fuseki:implementation` is unsupported.

Sparkles serves every operation on every dataset. A Fuseki service without write
endpoints is read-only. When every service is read-only, `serve.sh` gets `--read-only`.
When only some are, the auth configuration of §6 grants the anonymous caller, and each
user, only the endpoints that Fuseki served.

### 5.2 Datasets

The dataset of a service is resolved through its wrappers:

| Fuseki | Sparkles | Report |
|---|---|---|
| `tdb2:DatasetTDB2`, `tdb2:DatasetTDB`, `tdb:DatasetTDB` with a location | a persistent dataset, with a `tdb2.tdbdump` step in `load.sh` | manual step |
| the same at `--mem--`, `ja:MemoryDataset`, `ja:DatasetTxnMem` | an in-memory dataset | converted |
| `ja:data` on an in-memory dataset | a persistent dataset loaded by `load.sh`; `serve --fuseki-config` loads it into memory at each start | approximated for `convert`, converted for `serve` |
| `tdb2:unionDefaultGraph true` | `--union-default-graph` when every dataset agrees | converted, else unsupported |
| `ja:RDFDataset` whose graphs are `ja:MemoryModel`s with `ja:externalContent` | the files loaded into the default graph or into each `ja:graphName` | as `ja:data` |
| `ja:RDFDataset` whose graphs are the default graph or the union graph of one TDB2 dataset, under their own names | that dataset, with `--union-default-graph` for `urn:x-arq:UnionGraph` | converted |
| `ja:RDFDataset` that selects or renames graphs of a TDB2 dataset, `ja:UnionModel` | none | unsupported, with a pointer to clones of selected graphs |
| `ja:InfModel` in the default graph | §5.5 | approximated |
| `ja:DatasetRDFS` with `ja:rdfsSchema` | `--rdfs NAME=FILE` | converted |
| `text:TextDataset` | §5.3 | converted or approximated |
| `geosparql:GeosparqlDataset` | §5.4 | converted or approximated |
| `access:AccessControlledDataset` | §6.3 | converted |
| one dataset shared by two services | one dataset, served under the first service's name | unsupported for the other names, since Sparkles has no aliases |

### 5.3 Text indexes

`text:TextDataset` with a `text:TextIndexLucene` becomes `datasets/NAME/text.json` and
`--text NAME=…`. The entity map's `text:map` gives `predicates`, the list of every
`text:predicate`. Sparkles indexes literals per predicate, so the field names do not
matter, and the report lists them with their predicates. These parts are approximated
and named in the report:

- A search without a property searches every indexed predicate in Sparkles, and only the
  `text:defaultField` in Jena. This differs when the map has more than one field.
- Lucene's analyzers. Sparkles splits words, lowercases and folds to ASCII. A
  `text:LocalizedAnalyzer` with `text:language` adds that language to `languages`, and
  `text:multilingualSupport true` sets `languages` to `"all"`. Stop words, keyword
  analyzers, `text:ConfigurableAnalyzer`, `text:DefinedAnalyzer` and
  `text:queryAnalyzer` have no equivalent.
- `text:propLists`.

`text:directory`, `text:entityField`, `text:uidField`, `text:graphField`,
`text:langField` and `text:storeValues` are ignored, because Sparkles keeps the index in
the database, always records graphs and languages, and always can highlight. Any other
index type, such as Elasticsearch, is unsupported.

### 5.4 GeoSPARQL

`geosparql:GeosparqlDataset` (or `geosparql:geosparqlDataset`) becomes
`datasets/NAME/geo.json` and `--geo NAME=…`, since Jena always builds a spatial index
for it. `geosparql:queryRewrite` (true by default) sets `queryRewrite`.
`geosparql:inference` (true by default) adds `sparkles infer --vocab geosparql` to
`load.sh`, and `geosparql:applyDefaultGeometry` adds `--geo-default-geometry`.
`geosparql:spatialIndexFile`, `geosparql:indexEnabled`, `geosparql:indexSizes` and
`geosparql:indexExpiries` concern Jena's index files and caches and are ignored.
`geosparql:srsUri` is approximated, because Sparkles' index works on WGS 84 coordinates
whatever the preferred CRS is.

### 5.5 Inference

A `ja:InfModel` becomes a materialization in `load.sh` and `--auto-reason 5` in
`serve.sh`, which re-runs it a few seconds after writes stop. Jena's inference models
answer queries over the current data at once, so this is approximated.

| `ja:reasonerURL` | Sparkles |
|---|---|
| `…/RDFSExptRuleReasoner` | `--profile rdfs` |
| `…/TransitiveReasoner` | `--profile rdfs`, a superset |
| `…/OWLFBRuleReasoner`, `…/OWLMiniFBRuleReasoner`, `…/OWLMicroFBRuleReasoner` | `--profile owl-rl` |
| `…/GenericRuleReasoner` with `ja:rulesFrom` or `ja:rule` | `--rules FILE`, copied or written into `datasets/NAME/` |

`ja:schema` on the reasoner is loaded into the graph `urn:x-sparkles:fuseki:schema` and
passed to `infer` with `--ontology-graph`. Its base model is loaded as data, as in
§5.2. Rule sets that use backward rules are materialized forward.

## 6. Users and access

### 6.1 Users

Users come from `shiro.ini`'s `[users]` section (`name = password, role…`) or from the
Jetty realm file that `fuseki:passwd` names (`name: password[, role…]`). A password in
plain text becomes an argon2id hash with the parameters of `sparkles auth hash`. A
password in another form is unsupported for that user: Shiro hashes (`$shiro1$…`, or a
`[main]` section that sets a hashing credentials matcher) and Jetty's `MD5:`, `CRYPT:`
and `OBF:` forms. The user is then written to `auth.toml` as a comment, and the operator
sets a new password with `sparkles auth hash`. `--check` hashes nothing.

### 6.2 Who may use what

Fuseki applies two filters in turn. Shiro's `[urls]` rules come first, by the first
pattern that matches the request path. `fuseki:allowedUsers` on the server, the service
and the endpoint all must allow the user. The converter evaluates both for each dataset
and operation and for each principal, which is the anonymous caller or a named user:

- `anon` allows everyone. `authcBasic`, `authc` and `user` allow every user.
  `roles[a,b]` allows the users that have all of the roles. `localhostFilter` and
  `perms[…]` allow nobody here and are reported as approximated, since Sparkles cannot
  test them and access must not widen.
- `fuseki:allowedUsers "*"` allows every user, `"_"` everyone, `"!"` or an empty list
  nobody, and a list of names those users.

The result is written as grants. A principal that may use a write operation gets
`write` on the dataset, otherwise `read`. When its operations include queries (and
updates, for `write`), it can already do what the other endpoints do, so the grant has
no `endpoints`. Otherwise the grant lists them, mapped as in [C12](C12-graph-access-control.md):
`query`, `update`, `gsp-r`, `gsp-rw`, `upload`, `patch`, `shacl`, and `info` for the
prefixes operations. The anonymous caller's grants go under `[anonymous]`.

Shiro rules for `/$/**` decide who administers the server. A role that the rule
requires becomes a Sparkles role with `server = ["server-admin"]`. A rule that requires
only authentication gives `server-admin` to every user, as in Fuseki, and the report
says so. `localhostFilter`, the default of Fuseki's own `shiro.ini`, has no equivalent
once authentication is on, so the report asks for a user with `server-admin`.

A configuration without users and without access rules gets no `auth.toml`, and the
server stays open on loopback, as Sparkles always is without authentication.

### 6.3 Graph access control

`access:AccessControlledDataset` with an `access:SecurityRegistry` becomes grants
limited to graphs, following the table in
[API: Mapping Fuseki's configuration](../API.md#mapping-fusekis-configuration). Each
`access:entry`, in list form `("user" <g1> <g2>)` or as `access:user` and
`access:graphs`, adds the graphs to that user's `read` grant on the dataset. A user
without an entry gets no grant. `<urn:x-arq:DefaultGraph>` keeps its name. Fuseki
allows graph access control on read-only services only, so the grants are `read`.

## 7. The report

Each element of the configuration becomes one line with its kind, the place in the
configuration (`service /ds`, `dataset /ds`, `server`, `shiro.ini [urls]`), and what
happened. The kinds are:

| Kind | Meaning |
|---|---|
| `converted` | Sparkles behaves as Fuseki did. |
| `approximated` | Sparkles behaves close to Fuseki, and the line says how it differs. |
| `manual` | The output says what to run, such as exporting a TDB2 database. |
| `ignored` | The element has no effect in Sparkles, such as `tdb2:location` or `rdfs:label`. |
| `unsupported` | Sparkles has no equivalent, and clients or users will notice. |

The exit status is 0 when nothing is unsupported, 1 when something is, and 2 when the
input cannot be read or holds no Fuseki service. `convert` still writes its output when
the status is 1, so that the operator can fix the rest by hand.

## 8. Acceptance examples

- A1. `config-1-mem.ttl` from Jena's examples converts with status 0 to `--mem dataset`.
- A2. `config-tdb2.ttl` converts to `--loc dataset=db/dataset`, with a `tdb2.tdbdump`
  step in `load.sh` and `--union-default-graph` from the endpoint's context.
- A3. `config-text-tdb2.ttl` gives a `text.json` whose `predicates` is `[rdfs:label]`.
- A4. `config-inference-1.ttl` gives `sparkles infer --profile owl-rl` and loads
  `Data/data.ttl`.
- A5. `config-timeout-server.ttl` gives `--timeout 10`, and `config-timeout-endpoint.ttl`
  gives `--timeout 60` with an approximated line.
- A6. `rdfs/config-rdfs.ttl` gives `--rdfs dataset=…/vocabulary.ttl`.
- A7. `tdb2-select-graphs.ttl` exits with status 1 and names the selected graphs.
- A8. The access test configuration `config-server-1.ttl` with a password file gives an
  `auth.toml` that `sparkles auth check` accepts, in which `user1` and `user3` read
  `database1` and nobody else does.
- A9. A `shiro.ini` with `admin = pw, administrator` and `/$/** = authcBasic,roles[administrator]`
  gives a user `admin` with an argon2id hash and the role `administrator` with
  `server-admin`. The report never contains `pw`.
- A10. For every Jena example that the assembler bodies of `POST /$/datasets` accept, the
  converter gives a dataset of the same name and type.
- A11. `serve --fuseki-config` on a configuration with `ja:data` serves the loaded data.

## 9. Rejected alternatives

- **Reading assembler files at every start as Sparkles' own configuration.** The
  assembler vocabulary is open-ended and tied to Java classes, and Sparkles' settings live
  with each dataset. `serve --fuseki-config` exists for trying a deployment out, and the
  converted files are what a migration keeps.
- **Approximating `localhostFilter` with anonymous grants.** Sparkles cannot tell a local
  caller from a remote one once the server listens on the network, so this would widen
  access.
- **Per-dataset timeouts.** They would be a new server feature for one migration case.
  The report names the datasets whose timeout changes.
- **Dataset aliases.** A second name for the same store would complicate the registry,
  the metrics and the access checks. The report names the services that share a dataset.

## 10. Code layout

`crates/sparkles-server/src/fuseki_config/` holds the reader of configuration graphs
(`graph.rs`), the user files (`users.rs`), the conversion into a plan with its report
(`convert.rs` and `access.rs`), the writer of the output directory (`write.rs`) and the
command (`mod.rs`). The endpoint table of §5.1 moves out of
`http/fuseki/assembler.rs` so that both readers share it. The Jena configurations used by
the tests are copied unchanged into `testsuite/fuseki-config/jena` with Jena's license
and notice.

## Outcome

**Delivered.** All of §2 to §7 landed on 2026-10-03 in
`crates/sparkles-server/src/fuseki_config/`. The conversion is split over more files than
§10 planned: `service.rs` reads services, endpoints and the dataset wrappers, `parts.rs`
reads text indexes, GeoSPARQL, registries, assembled datasets and reasoners, and
`report.rs` holds the report. `endpoints.rs` holds the table of §5.1, which the assembler
bodies of `POST /$/datasets` now use too. `sparkles fuseki-config convert` writes the
files of §3, and `serve --fuseki-config` writes its settings files and auth configuration
under `<data>/fuseki/`.

**Deviations and decisions.**
- A Fuseki configuration in which every service is read-only gets `--read-only` and no
  auth configuration. When only some services are read-only, the converter writes an
  auth configuration whose anonymous grants follow each service's endpoints, as §5.1
  says, so a server that was open in Fuseki starts with authentication on.
- `--auto-reason 5` is left out on a read-only server, where the data cannot change.
- A query grant has no `endpoints` limit, because a query can read whatever the Graph
  Store endpoints return. A grant is limited to endpoints only when the operations lack
  queries, or lack updates for a `write` grant.
- A shared dataset is found through its wrappers too, so a plain service and an
  access-controlled view of the same TDB2 dataset count as sharing it.
- A union-default-graph setting in an endpoint's context applies to the whole dataset,
  and the report says so.
- `serve --timeout` and `--update-timeout` have no clap default any more, so that a
  converted timeout applies unless the flag is given. The defaults stay 60 and 0 seconds.
  A converted timeout above 30 minutes also raises `--max-timeout`.
- `serve --fuseki-config` materializes inferences at the first start of a dataset that
  has none recorded, and loads a reasoner's `ja:schema` files into in-memory datasets.
  A persistent dataset starts empty and logs the `tdb2.tdbdump` step.
- A user that access rules name but no user file defines is reported as ignored, since
  Fuseki could not authenticate it either.
- Paths in the report are relative to the working directory, and the scripts use
  absolute ones.

**Tests.** `fuseki_config/tests.rs` converts every configuration in
`testsuite/fuseki-config/jena` and checks A1 to A10: memory and TDB datasets, the union
default graph, the text index against `TextConfig`, inference with and without a schema,
the three timeout examples, RDFS on read, selected graphs, endpoint names, the context
path, `allowedUsers` with a password file, endpoint users, graph access control, a
Fuseki base directory with `shiro.ini` whose `auth.toml` passes `FileConfig::load`, and
GeoSPARQL against `GeoConfig`. A10 compares five of Jena's examples with the assembler
bodies. `users.rs` and `access.rs` have unit tests for Shiro's sections and filters,
password forms, Ant patterns and Fuseki's `allowedUsers` rules.
`tests/cli_fuseki_config.rs` runs the binary: the exit statuses 0, 1 and 2, `--check`,
`--format json`, `--force`, a base directory whose `auth.toml` passes
`sparkles auth check`, the converted `serve.sh` of a text and a GeoSPARQL configuration
started on a port in 5540–5559, and A11 with `serve --fuseki-config`.

**Not built.** Per-dataset timeouts, dataset aliases, Shiro realms other than `[users]`,
custom operations and Java code (§1 non-goals and §9). Jetty's `OBF:` passwords are not
decoded, although the obfuscation is reversible, so that the converter never handles a
recoverable secret that the operator did not write in plain text. `ja:literalContent`
is unsupported.
