# Features

This page lists what Sparkles can do, grouped by area, so that you can check whether it
supports a feature and find where that feature is documented. Each entry describes the
current behaviour. [USAGE.md](USAGE.md) documents the command line and the bindings,
[API.md](API.md) documents the HTTP interface, and the design specs in
[specs/](specs/README.md) explain why each larger feature is built the way it is.
[Known gaps](#known-gaps) lists what Sparkles does not do, and each gap names the
[roadmap](ROADMAP.md) item that addresses it. [COMPARISON.md](COMPARISON.md) compares
Sparkles with Jena and Fuseki, QLever, Fluree and Oxigraph, and
[BENCHMARKS.md](BENCHMARKS.md) has the performance figures.

* [Standards conformance](#standards-conformance)
* [Storage and indexes](#storage-and-indexes)
* [Versioning and history](#versioning-and-history)
* [SPARQL queries and updates](#sparql-queries-and-updates)
* [Search](#search)
* [GeoSPARQL](#geosparql)
* [Reasoning, validation and schema](#reasoning-validation-and-schema)
* [Server and protocols](#server-and-protocols)
* [Access control and security](#access-control-and-security)
* [AI and agents](#ai-and-agents)
* [Web UI](#web-ui)
* [Libraries and bindings](#libraries-and-bindings)
* [Command line](#command-line)
* [Formatter and editor support](#formatter-and-editor-support)
* [Operations](#operations)
* [Known gaps](#known-gaps)

## Standards conformance

Sparkles runs the W3C and community test suites in its own harness. `mise run test:w3c`,
`test:shacl` and `test:shex` run them, as [DEVELOPMENT.md](DEVELOPMENT.md) describes.

| Standard | Result |
|---|---|
| SPARQL 1.1 Query | All 328 W3C tests pass. |
| SPARQL 1.1 Update | All 157 W3C tests pass. |
| SPARQL 1.0 | All 482 W3C tests pass. |
| SPARQL 1.2 | All 269 tests pass. |
| Jena SPARQL-CDTs | All 655 tests of Jena's composite datatype suite pass. |
| Jena ARQ property functions | All of ARQ's PropertyFunctions tests pass. |
| SHACL Core and SHACL-SPARQL | All 98 Core tests and all 20 SPARQL tests of the W3C suites pass. |
| SHACL 1.2 | The eight list constraint tests and all 146 node expression tests pass. |
| SHACL Compact Syntax | Every test pair of the Working Group's suite reads and writes back to the expected graph. |
| ShEx 2.2 | Every syntax, structure and representation test of shexTest passes, and 99.9% of the validation tests. The 42 tests that check blank-node labels are skipped. |
| GeoSPARQL 1.1 | 37 of Oxigraph's 44 tests pass, and the other 7 are listed with reasons. Tests derived from Jena's also pass. |
| Apache Jena | Jena's contract tests pass on the `sparkles-jena` dataset, and Jena's HTTP clients are tested against the server. |

The SPARQL parser is a vendored, patched `spargebra`
([`vendor/spargebra/PATCHED.md`](../vendor/spargebra/PATCHED.md)).

## Storage and indexes

The store follows TDB2's model of bulk loads, transactions, compaction and backups. Its
index is a set of QLever-style sorted and compressed permutations.

### Indexes and vocabulary

* Terms are 64-bit tagged ids. `xsd:integer`, `xsd:decimal`, `xsd:double`,
  `xsd:boolean`, `xsd:dateTime` and `xsd:date` values are stored inline when their
  lexical form is canonical.
* The index holds 7 permutations (SPO, SOP, PSO, POS, OSP, OPS and GSPO) in compressed
  blocks of 32k rows. Index format 3 splits each block column into frames of 2,048 rows
  with a directory of their first keys, so a point lookup, a short range scan or an index
  join decodes only the frames that can hold its rows. Stores in format 2 open as they
  are and are rewritten at the next compaction.
* The base vocabulary is sorted, front-coded and memory-mapped, and an append-only delta
  vocabulary holds the terms added since. With `--vocab-codec fsst`, a load or compaction
  also compresses each key's suffix with FSST, which shrinks `vocab.dat` by about 30% and
  writes generation index format 4. FSST is off by default because it slows queries that
  decode many strings
  ([X11](specs/X11-index-compression-and-predicate-patterns.md#item-108-faster-fsst-reads)).
* A numeric column beside the base vocabulary, `vocab.num`, holds the exact value of
  every numeric literal whose lexical form is not canonical, such as DBpedia's
  `xsd:float` coordinates or any `xsd:int`. Range filters, sorts and aggregates over these
  values read 8 bytes per value instead of decoding the key. Loads and compactions write
  the column, and `sparkles vocab-index` adds it to a database that lacks it
  ([USAGE.md](USAGE.md#command-line-tools)).
* The planner keeps statistics for predicates, with their counts and distinct subjects
  and objects, and for classes and graphs.
* Predicate patterns implement QLever's pattern trick
  ([X11](specs/X11-index-compression-and-predicate-patterns.md#outcome)). Each generation
  stores every subject's set of predicates by number, for the default graph and for the
  union of the named graphs. The patterns answer `GROUP BY ?p` with `COUNT(DISTINCT ?s)`
  over `?s ?p ?o`, `SELECT DISTINCT ?p` over the same shape, and `spk:hasPredicate` with
  its QLever name `ql:has-predicate`, corrected for the snapshot's delta. Class profiles
  use them to skip predicates that no profiled instance has. Under a protection of
  triples, RDFS on read, a set of named graphs or a large delta, the same queries read the
  permutations.

### Cold reads

These read paths are tuned for queries over data that is not yet in memory
([X07](specs/X07-cold-sparse-reads.md), [API.md](API.md#the-io-profile)).

* A query hints the vocabulary groups of all its constants in one batch and requests
  the blocks of its first scans at plan time. Each scan pass has its own read-ahead
  window that grows from 1 MiB to the limit that `SPARKLES_READ_WINDOW_KB` sets. A warm
  repeat of a query makes no read hint.
* Index joins decode the columns they need on a separate pool, and optional warmup
  observes cancellation and deadlines. Each step has its own `Optimizations` flag.
* `profile=io` on a query returns its phase times, the index columns it decoded, the
  bytes it requested and sampled page residency, for callers with the `metrics`
  permission. `/$/metrics` counts decoded columns, read-ahead bytes and vocabulary
  groups read.

### Loading data

* The parallel bulk loader reads Turtle, N-Triples, N-Quads, TriG, RDF/XML and JSON-LD,
  streaming or buffered, and chooses the mode itself unless told. Input can be
  compressed with gzip, xz, bzip2, zstd, brotli or LZ4. `sparkles load` and the library's
  `Dataset::load_file` also read Jena's TriX, RDF Thrift, RDF Protobuf and RDF/JSON.
* Inputs larger than the memory budget are sorted externally.
* Large updates and inference batches take a bulk write path that merges them into a
  rebuilt generation and publishes it by an atomic switch of `CURRENT`.
* CSV and TSV tables map to triples ([C05](specs/C05-tabular-imports.md),
  [USAGE.md](USAGE.md#loading-csv-and-tsv)). `sparkles load`, `sparkles csv`,
  `POST /{ds}/upload` and the web UI's upload form accept them. A table maps with a
  default mapping in which a key column or the row number names each row, with W3C CSVW
  metadata, or with Tarql-style CONSTRUCT templates that the SPARQL engine runs in
  batches. CSVW covers datatypes and formats, null values, defaults, lists, virtual
  columns and RFC 6570 URI templates. Tables stream, compressed or not, and an invalid
  cell stops the load and names its row and column.
* CSVW's standard mode adds the table group, table and row descriptions, and primary and
  foreign keys are checked across the tables of a load in the order their references
  need. A CONSTRUCT template can call Tarql's `tarql:expandPrefix` and
  `tarql:expandPrefixedName`, and with `--lookup` or `lookup=true` it reads the
  dataset's head to link rows to resources the dataset has.
* R2RML, RML and YARRRML mappings load CSV, TSV, JSON, JSON Lines, Parquet and SQLite
  sources ([F19](specs/F19-mappings-and-virtual-graphs.md),
  [USAGE.md](USAGE.md#mappings-r2rml-rml-and-yarrrml)). RML covers both the rml.io
  vocabulary and RML-Core. `sparkles map run`, `sparkles load --mapping` and uploads run
  them. Each source is read once as a stream, and values keep their natural types. A join
  over one SQLite connection runs as one SQL statement, and other joins use a hash table
  within the memory budget. Function maps call GREL, IDLab and SPARQL functions.
  `sparkles map` also checks a mapping, converts YARRRML to RML-Core Turtle and drafts an
  R2RML mapping from a SQLite database. The W3C R2RML test cases pass on SQLite apart
  from three, and the RML-Core cases with CSV and JSON sources apart from six.
* Mappings of every kind can be stored per dataset with versions and run again
  ([API.md](API.md#stored-mappings)). A run in replace mode commits only the net change.
  The Graph Store protocol takes CSV bodies with a stored or default mapping, and the
  server reads SQLite databases through the named connections of its `connections`
  settings kind.

### Transactions and durability

* Reads use MVCC snapshots with a single writer, and a write-ahead log replays safely
  after a crash.
* A commit is acknowledged once its WAL record and any new vocabulary entries are
  synchronized. The two writes are issued in parallel, and the update vocabulary is
  written in checksummed chunks, so a torn tail is detected and trimmed at open.
* Group commit shares one synchronization among concurrent writers and is on by default
  for persistent stores ([X09](specs/X09-group-commit-by-default.md)).
  `--no-group-commit` or `SPARKLES_GROUP_COMMIT=off` turns it off, and the Node and
  Python bindings take a `groupCommit` or `group_commit` option. The JVM library follows
  the default.
* Group commit covers SPARQL updates, Graph Store writes, uploads, RDF Patch, commit
  messages and digests, preconditions, graph-restricted writes, SHACL-guarded writes,
  writes during a compaction, and datasets with a full-text, spatial or embedding index.
  ShEx-guarded datasets go through the group path one commit at a time. No index on disk
  ever names a commit that the log could lose. A commit that does not qualify, or one made
  while a full-text index recovers or rebuilds or while a spatial index builds, takes the
  ordinary path.
* Group commit costs memory. A persistent dataset queues up to 64 commits or 4 MiB of
  log records, plus the speculative snapshots of the commits in flight. On an indexed
  10.5M-triple dataset with sixteen writers, that state raised peak resident memory by
  15 to 21 MiB.
* `/$/metrics` reports the synchronization time, the commits per fence, the wait, and why
  commits fell back or failed. `/$/stats/{ds}` says whether the dataset groups commits.
* A WAL written before logs had a header record is framed strictly from the offset where
  the current release first opened it for writing. The offset is recorded in the
  generation's `commit.json` once the log, its vocabulary and its annotations are durable,
  so a torn modern record after it is never read as a legacy one.

### Compaction, clones and integrity

* Compaction writes a new generation, `gen-NNNN`, and publishes it by an atomic switch of
  `CURRENT`. The build reads a snapshot while writes continue, the commits made meanwhile
  are carried into the new generation's log, and the writer lock is held only for the
  switch. After a power loss during a compaction or bulk load, the next open deletes an
  unfinished build directory, sets any other directory above `CURRENT` aside under a new
  name, and refuses a directory that holds generations but no `CURRENT`. `sparkles check`
  reports such a leftover as a warning.
* Automatic compaction runs on a server ([C13](specs/C13-automatic-compaction.md),
  [API.md](API.md#automatic-compaction)). It starts when a dataset's delta reaches a share
  of the base index or a size, its log reaches a size, its oldest change reaches an age, or
  after a quiet period. Flags set the policy, and `compaction.json`,
  `/$/compaction/{ds}` and `sparkles compaction` override it per dataset. It waits for
  bulk loads, backups, restores, the retention window and free disk space, takes a task
  slot only when one is free, and builds with fewer threads at a lower priority. A delta
  that adds no terms is compacted partially, rewriting only the blocks it touches.
* `POST /$/datasets/{ds}/clone` and `sparkles clone` copy a dataset into an independent
  sandbox from one snapshot. The clone has the same quads and blank-node ids and a new
  dataset id with `forkedFrom`. A source with no changes since its last compaction is
  cloned by sharing its index files, by reflink where the file system supports it, by
  copy otherwise, or by hard link on request. A clone can copy only some graphs, can be an
  in-memory dataset, and waits for one of the `--max-clones` slots.
* `/$/backup` and dumps write N-Quads, compressed with zstd by default or with gzip, xz,
  bzip2, brotli or LZ4. Request and response bodies can be compressed with `zstd`, `br`
  or `gzip`. Incremental backup repositories are described under [Operations](#backups).
* `sparkles check` and `sparkles::check` run a read-only integrity check that is safe
  beside a running server ([USAGE.md](USAGE.md#checking-a-database)). It checks the
  layout, every block of the 7 permutations and their consistency, vocabulary order and
  id ranges, WAL checksums and commit continuity, the catalog, full-text segment
  checksums, and the spatial index files.
* In-memory datasets run on the same engine, with the base in a temporary directory.

## Versioning and history

Every write is a numbered commit, and past states, diffs, branches and the change history
stay readable.

### Commits

* Each dataset has a UUID and a gap-free commit sequence with timestamps and net counts.
  Writes return receipts and `Sparkles-Commit` headers, and `/$/commits` and
  `sparkles log` list the commit catalog.
* A write's `Sparkles-Commit-Message` header, or `--message` on `update` and `load`, is
  stored with its commit and shown in receipts, `/$/commits` and `sparkles log`. With
  `--commit-digests`, a SHA-256 digest chains each commit's net changes.
* Graph Store GET and HEAD return entity tags named after the commit read.
  `If-None-Match` answers `304`, and `If-Match` on PUT, POST and DELETE is an optimistic
  concurrency check made under the writer lock that answers `412` on a mismatch.
* `dryRun=true` or `Sparkles-Dry-Run: true` previews an update, Graph Store write or
  upload by running it up to its commit and rolling it back
  ([API.md](API.md#write-previews)). The response gives the commit it would create, net
  counts per graph, optionally the changed quads as JSON or RDF Patch, the validation
  summary, the quota effect and the outcome of `If-Match`, with the status the write would
  get. Nothing is written and no commit number is used.
* A Graph Store `PUT` through the write-ahead log records only the quads that change.
* A commit catalog horizon, set by `catalog` in `PUT /$/history/{ds}` or
  `sparkles snapshot catalog`, prunes the records, messages and digests of commits older
  than both the readable history and the horizon.

### Time travel

* `?at=commit:N`, `time:…` or `snapshot:NAME` reads a past state in queries, explain,
  Graph Store GET, SHACL validation, stats and schema discovery. The Graph Store adds
  Memento headers and `Accept-Datetime` negotiation. `--at` does the same on `query`,
  `dump` and `clone`.
* Named snapshots keep a commit readable across compaction. They can expire, be taken on
  a schedule and be kept materialized for fast reads. A retention window keeps older
  commits readable within a disk cap. `/$/snapshots`, `/$/history` and
  `sparkles snapshot` manage them.
* A past state is rebuilt from the nearest known state in the write-ahead log, found
  through a sparse index, by replaying forward or undoing backward. In-memory datasets
  keep the states their snapshots and window hold.

### Diffs, change feed and history queries

* `GET /{ds}/diff`, `sparkles diff` and `Store::diff` compare any two readable commits
  ([API.md](API.md#diffs-between-commits)). A diff comes as JSON counts and quads, as
  N-Quads lines marked `+` and `-`, or as RDF Patch in text or RDF Thrift, for one graph
  or all. Within the retained logs a diff reads only the changes between the commits, and
  across a bulk commit it compares the two states with a sorted merge.
* A diff can name a branch on either side with `fromBranch` and `toBranch`, and can keep
  only changes whose subject or object is one of up to 1,000 `term` values. With
  `group=class` it returns counts grouped by subject class, predicate and object class or
  datatype, up to 5,000 groups.
* `GET /$/commits/{ds}/timeline` lists a branch's commits in buckets with the size of
  their changes. `POST /{ds}/sparql/diff` runs a SELECT query at two states and returns the
  rows only one of them has, comparing 128-bit row hashes so that large results fit the
  memory budget ([API.md](API.md#comparing-a-query-between-two-states)).
* `GET /{ds}/changes` and `Store::changes` serve a change feed of the commits after a given
  one, as JSON or one RDF Patch per commit, in pages bounded by commits and quads. It
  resumes from any readable commit, long-polls with `wait=`, and streams server-sent
  events that resume with `Last-Event-ID`.
* History queries search the changes across commits ([API.md](API.md#history-queries)).
  `SERVICE hist:changes` in SPARQL, `GET /{ds}/history`, `sparkles history` and
  `Store::history_changes` return each added or removed quad with its commit's number,
  time, kind, author and message, and write the triple pattern as an RDF 1.2 reifier. The
  pattern's terms and the commit range can come from the rest of the join group.
* History queries read a per-dataset change log that outlives compactions. It is written
  off the commit path, indexed by subject, predicate, object and graph, recovered from the
  write-ahead log after a crash, and trimmed by commit count, age or size. Diffs and the
  change feed read it where the retained generations no longer can. Graph views and
  protections of triples apply.

### Branches and merges

Branches work like git branches on the quad store
([F09](specs/F09-branches-and-merges.md), [API.md](API.md#branches-and-merges)).

* A persistent branch starts from any readable commit of another branch and shares that
  generation's index files until it compacts, so creating one writes a few kilobytes.
  In-memory branches share immutable base state and keep their writes and validation
  state separate.
* A branch has its own commits, history, snapshots, write guard and writer lock. Requests
  choose one with `?branch=`, the path form `/{ds}@{branch}/…` or `--branch`.
* A merge finds the merge base and either fast-forwards or merges quad sets three ways.
  It reports conflicts by cell or subject with the base, ours and theirs objects, and takes
  resolutions per graph, subject or cell, a rule for the rest, and the heads the caller
  saw. The merge is one commit of kind `merge` that passes the target's write-time
  validation, and a dry run previews it.
* Merges can also squash a branch into one commit, replay its commits one by one, run as
  cancellable tasks, and keep both sides' changes to exempt predicates. Reverts and
  cherry-picks are merges of one commit. Blank nodes keep their labels across branches.
* Branches can be protected, renamed, deleted, which re-parents the branches created from
  them, and limited by grants. `/$/branches`, `/$/merge`, `/$/revert`, `/$/cherry-pick`,
  `sparkles branch`, `merge`, `revert` and `cherry-pick` manage them, as do the web UI's
  Branches panel and merge page. A branch backup restores as an independent dataset.

### RDF Patch

* `POST` and `PATCH /{ds}/patch`, a patch sent to `/{ds}`, `sparkles patch`,
  `Store::apply_patch` and the Python `Dataset.apply_patch` apply a patch in text or
  binary form as one commit of kind `patch`, as Fuseki's `patch` operation does
  ([F10](specs/F10-replication.md), [API.md](API.md#applying-rdf-patch)).
* `TA` aborts the whole patch, and `PA` and `PD` change the prefixes. A `prev` header that
  names a commit of the dataset applies the patch only while that commit is the head.
  Write-time validation, quotas, graph-limited grants, dry runs and receipts apply as to
  other writes.

## SPARQL queries and updates

Queries and updates follow the W3C recommendations and Jena ARQ's extensions, and tests
check them against the W3C suites and Jena's own answers.

### Query and update language

* SPARQL 1.1 Query covers BGP, OPTIONAL, UNION, MINUS, FILTER, BIND, VALUES, subqueries,
  GROUP BY and aggregates, ORDER BY, DISTINCT, LIMIT and OFFSET, and EXISTS.
* SPARQL 1.1 Update covers INSERT DATA, DELETE DATA, DELETE/INSERT WHERE, LOAD, CLEAR,
  DROP, CREATE, ADD, COPY and MOVE.
* RDF 1.2 and SPARQL 1.2 work in every RDF syntax. This covers triple terms
  (`<<( s p o )>>`), the reification syntax `<< >>`, annotations, base-direction literals
  such as `"x"@en--rtl`, and the functions `TRIPLE`, `SUBJECT`, `PREDICATE`, `OBJECT`,
  `isTRIPLE`, `LANGDIR`, `hasLANG`, `hasLANGDIR` and `STRLANGDIR`.
* The property paths `p*`, `p+` and `p?` run as a breadth-first search over the index,
  starting from the side the join input binds.
* The value space follows the specification for numeric promotion, comparisons, effective
  boolean values and the total order of ORDER BY.
* The function library has the SPARQL 1.1 built-ins, the XSD casts, including the
  derived integer types with their ranges, `anyURI` and the Gregorian types, and ARQ's
  `fn:`, `afn:` and `math:` functions ([API.md](API.md#extension-functions-and-aggregates)).
  Examples are `fn:round-half-to-even`, `fn:format-number`, `fn:apply`, the duration
  accessors, timezone adjustment, `fn:normalize-unicode`, `afn:sprintf` and
  `afn:sha1sum`, as well as ARQ's arithmetic on dates, times and durations. Tests compare
  them with Jena's answers.
* Jena ARQ's statistical aggregates `MEDIAN`, `MODE`, `STDEV`, `STDEV_SAMP`, `STDEV_POP`,
  `VARIANCE`, `VAR_SAMP` and `VAR_POP` work with `DISTINCT`, as keywords, by their `agg:`
  and `afn:` IRIs and through `AGG <iri>(…)`. Their results, types, empty groups and errors
  match ARQ's.
* Results come as JSON, XML, CSV, TSV and `x-sparkles+json`, which includes the executed
  plan. Graphs come as Turtle, N-Triples, N-Quads, TriG, JSON-LD and RDF/XML, and as
  Jena's TriX, RDF Thrift, RDF Protobuf and RDF/JSON.

### Jena ARQ extensions

* ARQ's syntax extensions are accepted by default, as Fuseki accepts them
  ([API.md](API.md#arq-syntax-extensions), [G06](specs/G06-arq-query-extensions.md)).
  `LATERAL` substitutes each outer solution into its group and runs as a join when
  substitution cannot change the result. Path ranges such as `p{n,m}` count each way
  through the graph, as ARQ does. CONSTRUCT templates with `GRAPH` blocks return quads.
  `LET`, `SEMIJOIN`, `ANTIJOIN` and the paths `distinct(…)`, `multi(…)` and `:p^:q` follow
  ARQ's evaluation, and Jena 6.2.0's answers check them.
* Jena's composite datatypes `cdt:List` and `cdt:Map` follow Jena 5 and 6, with their
  literals, equality and order, the 16 `cdt:` functions, the `FOLD` aggregate and the
  `UNFOLD` operator. A blank node label inside a literal names the same node as that label
  in the literal's file or query.
* ARQ's property function library is supported ([API.md](API.md#arqs-property-functions)).
  It has `list:member`, `list:index` and `list:length` over RDF collections, `apf:container`,
  `apf:bag`, `apf:seq`, `apf:alt` and `rdfs:member` over RDF containers, and `apf:strSplit`,
  `apf:concat`, `apf:str`, `apf:splitIRI`, `apf:assign`, `apf:bnode` and
  `apf:versionARQ`. A call reads the variables that earlier patterns bind, as in ARQ.
  `rdfs:member` acts as an ordinary triple pattern while the store holds no container.
* DESCRIBE is configurable ([API.md](API.md#describe)). The default is the W3C concise
  bounded description, read from each graph as Jena's default handler reads them and
  extended with the reifiers that annotate the described triples. `scbd` adds the triples that point at
  the resource, and `outgoing` keeps only the resource's own triples. Options add labels
  of linked IRIs and limit the size and depth. A dataset keeps its setting through
  `/$/describe/{ds}` and `sparkles describe-settings`, and a request can choose another.

### Federated queries

* SERVICE, with SILENT, runs under an outbound network policy
  ([USAGE.md](USAGE.md#outbound-requests-service-and-load)). A server allows only public
  destinations by default, while the local `query` and `update` commands can also reach
  private ones. The policy has allowlists, DNS pinning, checked redirects, timeouts and a
  response ceiling.
* Jena's service enhancer options work on SERVICE
  ([API.md](API.md#service-options-loop-bulk-and-cache),
  [G10](specs/G10-service-enhancer.md)). `loop:` runs a SERVICE once per solution of the
  patterns before it. `bulk+n:` sends `n` solutions per request and retries one at a time
  when an endpoint cuts a response short. `cache:` keeps each solution's remote result in a
  per-dataset cache with a memory budget, separate for each caller, with `cache+clear` and
  `cache+off`. `urn:x-arq:self` reads the dataset itself.

### Planning and execution

* The planner can move a small basic graph pattern into the branches of a pure UNION, so
  that each branch uses its bindings to choose a join order. It keeps duplicates and
  FILTER scope, and it keeps the ordinary plan when that plan is estimated to be cheaper.
  This avoids enumerating unrelated coauthor walks in SP²Bench `q8` and `q12b`.
* Eager ASK execution can visit a pure UNION branch with lower estimated work before
  its first answer. On an unchanged snapshot without a total produced-row cap, a
  bounded whitelist of the whole ASK lets pure input operators request only the rows
  the limit needs. Expression calls, EXISTS and external effects retain their ordinary
  order and demand policy. Reordering also requires conservative output bounds that fit its
  row and memory budgets. Joins through index probes retain their order because their
  duplicate-key fanout lacks such a bound. A retrying operator retains the ordinary
  prefix when its input would restart a full join build. Explicit streaming keeps its policy.
* An OPTIONAL whose sides share no variable, but whose condition contains `?a = ?b` with
  one variable from each side, pairs its rows through that equality instead of forming
  their product ([internals](internals/OPTIMIZATIONS.md)). SP²Bench `q6` at 10M tests 7.4
  million candidate pairs where the product had 1.6 × 10¹² rows.
* Groups linked only by a FILTER equality also join through candidate keys and test the
  full condition on those pairs. Large ASK queries read the inputs alternately and stop
  at the first qualifying pair. They retain prefixes of both inputs while both can
  produce rows, then discard obsolete history when either input ends.
* A triple pattern with one free variable can bind it during planning when a bounded
  snapshot read proves that the pattern has exactly one row. This makes other patterns
  and numeric FILTER bounds constant while preserving the result variables. Volatile
  filters, application callbacks and algebra boundaries retain the ordinary plan.
* Queries can run eagerly or as streaming cursors
  ([X05](specs/X05-streaming-query-execution.md)). Automatic execution, the default over
  HTTP, in the CLI, in Node and in the JVM library, streams scans expected to return at
  least a thousand rows, groupings of an estimated 20,000 input rows or more, and bounded
  ASK products whose eager candidates would exceed a budget, and large equality-keyed
  ASK joins. Other plans run eagerly, and
  `execution=eager` or `execution=streaming` overrides the choice.
* Rust, Python and Node have snapshot-owning SELECT and graph cursors, and the JVM
  library pulls SELECT results. Result writers use bounded buffers with backpressure and
  stop when the client disconnects.
* Streaming joins run batch by batch with column kernels. ORDER BY with LIMIT keeps only
  its best rows in a bounded heap. A cursor filter over an index scan filters whole blocks
  in parallel, and a range top-k ranks its range a part at a time. A full sort and the
  shapes the cursor does not support materialize within the budget and fail explicitly
  when they exceed it.
* Disk spill lets large queries finish within their memory budget
  ([X08](specs/X08-disk-spill-and-server-memory.md)). It is off by default, and
  `serve --spill on` or `encrypted` and `QueryOptions::spill` turn it on.
  * A streamed sort without LIMIT writes sorted runs and merges them. A grouping or a
    DISTINCT keeps what fits and handles the rest in later passes over hash-partitioned
    files.
  * A hash join whose build input outgrows its share partitions both inputs by key, up to
    three levels, and joins a heavy key as a block nested loop. A merge join spools a long
    run of equal keys to a file.
  * `--spill encrypted` seals every frame with AES-256-GCM under a per-file key held only
    in memory. Spill files are versioned, CRC-checked, unnamed while written and removed
    with their query or process.
  * Disk is reserved per query with `--query-spill-mb` or `spill-mb=`, and for the server
    with `--spill-max-mb`. With spill on, automatic execution streams a query that eager
    execution would hold in more than half its budget, and reruns an eager query that
    exceeds its budget as a cursor unless it calls SERVICE.
  * The server refuses to start with an unusable spill directory, one on tmpfs or ramfs, or
    one that other users can write.

### Explaining queries

* Every node of a returned plan has an id and its path from the root, and the estimated,
  eager and streamed plans have the same shape
  ([USAGE.md](USAGE.md#explaining-a-query), [API.md](API.md#explaining-a-query)). Nodes
  say when they did not run and why, when a `LIMIT` stopped them, how often they reran,
  which filters a scan applied itself, and when a result came from the cache.
* A query that a budget or its timeout stopped keeps its plan in the error body of the
  `application/x-sparkles+json` format.
* `POST /{ds}/sparql/explain` explains an estimated plan, a read-only run or a plan the
  client gives. Its notes cover the slowest operator, misestimates, blowups, late filters,
  large sorts, open paths, skipped nodes and the budget, together with a description of
  what the query asks, all computed without a model. A configured `explain` model role can
  rewrite them as prose, which the server checks against the plan. MCP's `explain_query`
  takes `profile`, `notes` and `useServerModel`, which needs the `serverModels` grant.

### Incremental views and live queries

* Incremental views and live queries ([API.md](API.md#incremental-views), spec [F13](specs/F13-incremental-views-and-provenance.md) Phase 1). A `table` view keeps a SELECT or CONSTRUCT query's result current after every commit, through a circuit of Z-set operators compiled from the query that joins each commit's net changes against the store's own indexes. Basic graph patterns, `GRAPH`, `FILTER`, `BIND`, `UNION`, `OPTIONAL`, `MINUS`, `EXISTS`, `VALUES`, `DISTINCT`, subqueries and `GROUP BY` with `COUNT`, `SUM` and `AVG` are maintained, and other operators are evaluated again as `recompute` nodes. A commit that misses a view's patterns skips it. Queries read a view with `SERVICE view:read`, and `GET /{ds}/live` streams a query's result and then its changes as server-sent events, resuming after a commit with `Last-Event-ID`. Views and live queries with the same query and access share one circuit. On a dataset with RDFS on read or materialized inferences in the default graph, a view or live query evaluates its whole query again with them at each commit that touches it, so it answers as the one-off query does, and the view's state reports `evaluation: recompute`. Turning either setting on or off switches existing views and live queries between the two modes without detaching them. View state is checkpointed and recovered from the change log, views read as their principal, and `Dataset::views`, `Dataset::live`, `sparkles views`, `sparkles query --live` and Python's `Dataset.views` and `Dataset.live` expose them.

## Search

Full-text, vector, path and graph-algorithm search run inside SPARQL and join with the
rest of a query.

### Full-text search

* Full-text search supports a subset of Jena's `text:query`, ranked by BM25 through
  Tantivy ([API.md](API.md#full-text-search)). It needs the `text` cargo feature, which
  the server enables.
* Each quad is a document, kept current with each commit. A write stages its documents,
  and the next search or a 1 s tick commits them. A bulk commit updates only the documents
  it changes, and `POST /$/text/{ds}/rebuild` rebuilds the index while writes continue.
* A search returns the top k within a graph. The query string can be a variable that the
  rest of the group binds, and a join can pass a search the subjects it already has.
* The index stems the literals of configured languages with Tantivy's Snowball stemmers
  and stop words, and `lang:` searches use them, as with jena-text's multilingual support.
  Analyzers that reproduce Lucene's for English, French, Spanish, Italian, Portuguese,
  German, Dutch, Russian, Swedish and CJK text can be selected per language instead. With
  them, 1,576 of 1,590 stemmed searches in nine languages find the same literals as
  Fuseki, and the other 14 consist of stop words alone.
* The tokenizer of those analyzers follows the grammar of Lucene's `StandardTokenizer`
  over Unicode 12.1, emoji and South East Asian scripts included, and agrees with Lucene
  on every code point.
* Japanese, Chinese and Korean text can be segmented into words with a dictionary
  through the `ja-dict`, `zh-dict` and `ko-dict` analyzers. `ja-dict` and `ko-dict`
  produce the tokens of Lucene's Kuromoji and Nori analyzers on every line of their
  fixtures, and `zh-dict` segments by jieba's word frequencies. `sparkles text-dict`
  downloads the pinned IPADIC, mecab-ko-dic and jieba sources with verified hashes and
  compiles them.
* An index can be enabled when a dataset is created. A missing or damaged index recovers
  in the background at open while the RDF data stays usable, and text searches answer
  `503` until recovery succeeds. Reconfiguring an index waits for old writers and
  checkpoints before it reuses their paths.
* A background checkpoint synchronizes only the segment files written since the last
  one, from several threads, while documents continue to publish. Under steady writes it
  repeats its unlocked pass over the newest files and takes the writer lock only for a
  short last pass when those repetitions keep being raced.

### Vector search and embeddings

* Vectors are `spk:vector` literals or their compact base64 form `spk:vectorB64`, compared
  with `spk:cosine`, `spk:dot` and `spk:euclidean`.
* `spk:vectorSearch` returns the top k within the active graph. Its query can be a
  literal, an entity, a variable bound by the rest of the group, or a text for an index
  that embeds on write. It takes `ef:`, `exact:true`, `distinct:subject` and
  `candidates:join`. `ORDER BY DESC(spk:cosine(?v, C)) LIMIT k` over one pattern runs as
  an exact vector search.
* Vector indexes pack one predicate's vectors and build an HNSW graph over them in the
  background ([API.md](API.md#vector-indexes)). `vector.json`, `/$/vector/{ds}/{name}` and
  `sparkles vector` manage them. The graph is a file of the generation that is mapped
  after a restart. New rows are inserted into it after each commit, and a compaction
  carries it over, so neither waits for a full rebuild. Searches overlay deleted and
  not-yet-inserted vectors exactly, past commits are searched exactly, and `M`,
  `efConstruction` and `efSearch` are set per index.
* An index can compute embeddings on write ([API.md](API.md#embeddings-on-write)). It
  names the literals to embed, by predicate, language, class or a SELECT query, and an
  OpenAI-compatible endpoint such as OpenAI, Ollama or vLLM. A background worker embeds
  new and changed text after each commit, removes the vectors of deleted text, and catches
  up after restarts and bulk loads without resending unchanged text. Long texts can be
  split into overlapping chunks, and requests can be rate-limited by count and by
  estimated tokens. API keys come from operator-defined secrets, and Prometheus metrics
  report requests, failures, the backlog and the lag.
* With the `embed-local` build feature, which the flake's packages include, the server
  runs BERT, XLM-RoBERTa, NomicBERT and Qwen3 sentence-embedding models itself with Candle
  ([API.md](API.md#model-providers), [F12](specs/F12-local-embeddings.md)). Models run on
  a small thread pool at a lower priority than queries, load at first use and unload when
  idle. They come from a store of pinned, hash-verified snapshots that
  `sparkles models pull` fills from the Hugging Face Hub or a Nix build provides.
* `spk:hybridSearch` fuses a `text:query` ranking and a `spk:vectorSearch` ranking by
  reciprocal rank fusion and returns the fused score and both ranks
  ([API.md](API.md#hybrid-text-and-vector-search)).

### Path search

* `SERVICE path:search` returns paths as solutions, one row per edge or per path
  ([API.md](API.md#path-search), [U09](specs/U09-path-finder.md#outcome)). It finds one
  shortest path per pair, every shortest path, the k shortest paths with Yen's algorithm,
  or every path up to a length.
* Edges come from a set of predicates, each followed forward, backward or both ways, from
  every triple, or from the solutions of a nested graph pattern. Each edge can be bound as
  a triple term. Sources and targets are constants or variables bound by the rest of the
  group.
* Weights on RDF 1.2 reifiers switch the search to Dijkstra's algorithm. A single pair
  runs from both ends, as a breadth-first search over the PSO and POS permutations
  without weights and as Dijkstra's algorithm with them. Yen's spur searches run from both
  ends too.
* `path:excludePredicate` skips predicates such as `rdf:type`, and `path:maxDegree` keeps
  paths from passing through hubs. Paths never use triples the caller cannot see.

### Graph algorithms

* `SERVICE algo:<name>`, `sparkles algo` and `POST /$/algo/{ds}/run` run graph
  algorithms ([API.md](API.md#graph-algorithms), [F14](specs/F14-graph-algorithms.md#outcome)).
  The MCP tool `graph_algorithm` and the Python and Node.js bindings expose them too.
* The algorithms are PageRank and personalized PageRank, betweenness, weakly and strongly
  connected components, label propagation, Louvain and Leiden with hierarchy levels, node
  similarity, triangle counts and clustering, k-core, breadth-first search, single-source
  shortest paths, and FastRP and node2vec embeddings.
* They run on a compressed sparse row projection built from chosen predicates,
  directions, node classes and reifier weights, or from a nested pattern. Results are
  rows that join with the rest of the query, and the projection holds only what the
  caller may read. Under `GRAPH ?g`, each graph's call uses only that graph's edges.
* A named projection is kept in `algo.json`, and built projections are cached for
  committed views.
* Weights follow each algorithm's domain. PageRank requires finite non-negative weights
  and treats nodes with no outgoing weight as dangling, and weights and sums must stay
  finite in f32. Graphalytics clustering defaults to forward edges.
* A run can write its results back in batched commits with a run record, as a
  cancellable task, and embeddings can go to a vector index the same way. Write-back
  replaces counters only within the destination graph.
* All calls share a thread pool, and each has a memory budget that charges every
  workspace before it grows. Long loops check cancellation and the deadline within a
  bounded amount of work.
* HTTP runs check the caller's Query or Update grants and rate limits before they read
  the dataset. A read run waits for the query memory pool as a query does, and a
  write-back run takes its grant when its task starts.

## GeoSPARQL

GeoSPARQL 1.1 and Jena's spatial functions run over a spatial index that is exact for
every snapshot. They need the `geo` cargo feature, which the server enables.

### Geometry literals and functions

* Literals can be `geo:wktLiteral`, `geo:geoJSONLiteral`, `geo:gmlLiteral` and
  `geo:kmlLiteral` ([API.md](API.md#geosparql)). GML covers the Simple Features levels 0
  and 1 of GML 3.2 with linear curves and surfaces, and KML is version 2.2. Z and M
  layouts and EMPTY are supported, and parse errors give byte offsets. `asGML` and `asKML`
  write GML and KML.
* The built-in CRSs are CRS84, CRS84h, EPSG:4326 and EPSG:4979 with their latitude-first
  axes, and Web Mercator, with OGC, QUDT and EPSG units. The 120 UTM zones use Krüger's
  series. A `--geo-crs` file adds projected CRSs from proj4 definitions, and the server
  resolves 4,916 projected EPSG codes from an EPSG-derived table. Definitions that need
  grid files are refused, and queries warn when a CRS shifts its datum by approximate
  Helmert parameters.
* The functions cover the 24 topological relations and `relate` on DE-9IM, `convexHull`,
  `envelope`, `boundary`, `centroid`, the four overlay operations, the accessors,
  `boundingCircle`, `concaveHull`, `isSimple` and the six `geof:agg…` aggregates
  ([API.md](API.md#hulls-aggregates-jena-filter-functions-utm-and-conversion)).
* `distance` is geodesic on WGS 84 by default, haversine if a dataset asks for it, and
  Euclidean for projected CRSs. `buffer` makes metric buffers through a local projection,
  and `area`, `length` and `perimeter` are geodesic.
* With `"wgs84": true` in `geo.json`, W3C Basic Geo `wgs84_pos:lat` and `long` pairs are
  indexed as points.

### Spatial index and queries

* Each dataset can have a spatial index, configured by `geo.json`, `/$/geo/{ds}`,
  `sparkles geo-index` or `serve --geo`. It is a packed Hilbert R-tree over a generation's
  geometry literals plus an overlay of committed writes. It stays within the `--geo-mb`
  memory budget and reports its status, rows, skipped literals and CRSs in
  `sparkles_geo_*` metrics.
* The index files in `gen-NNNN/geo/` are checksummed and read in place, so opening a
  database parses no literals. Damaged or foreign files are rebuilt, and compactions and
  bulk loads parse only new literals. The files are not part of backups or clones.
* Jena's `spatial:` property functions `nearby`, `withinCircle`, `withinBox`,
  `intersectBox`, the cardinal directions and their `…Geom` forms use the index, and so do
  spatial FILTERs. Arguments can be variables that the rest of the group binds. Jena's 15
  `spatialF:` filter functions and `spatial:equals` are supported.
* A GeoSPARQL relation, `relate` or a distance bound between two parts of a group runs as
  a spatial join instead of a cross product, and the index answers
  `ORDER BY geof:metricDistance(?w, C) LIMIT k`
  ([API.md](API.md#spatial-joins-and-nearest-neighbours)). EXPLAIN shows `SpatialScan`,
  `SpatialPf`, `SpatialJoin` and `SpatialKnn`, and warns when a query misses a join or
  nearest-neighbour plan.
* Query rewrite of the 24 topological properties, over asserted triples and those derived
  through default geometries and serializations, is opt-in per dataset with
  `queryRewrite` ([API.md](API.md#query-rewrite-spatialequals-and-rdfs-entailment)).
  `infer --vocab geosparql` adds RDFS entailment of the GeoSPARQL, Simple Features and GML
  vocabularies and types each geometry from its serializations, and
  `infer --geo-default-geometry` materializes default geometries.
* `GET /{ds}/geo?bbox=…` returns the indexed geometries in a box as simplified CRS84
  GeoJSON, and `POST /$/geo/convert` converts literals to CRS84 GeoJSON for maps.

## Reasoning, validation and schema

Inferences are materialized and kept current or derived on demand, shapes validate on
request or on every write, and the schema report counts what the data holds.

### Reasoning

* RDFS, OWL 2 RL and Jena rule syntax materialize through `sparkles-reasoner`,
  `/$/reason` and `sparkles infer`. The rules read their input graphs from the
  compressed indexes, so a run's memory follows the facts it derives. LUBM(1000) under
  RDFS, 138 million triples, materializes in 91 s at 15.2 GiB of RSS on 16 cores
  ([F16](specs/F16-materialization-at-scale.md#outcome), [BENCHMARKS.md](BENCHMARKS.md)).
* Each iteration merges its results in parallel on a reasoning pool, half the cores by
  default with `serve --reason-threads`, that admits runs in arrival order. A run stops at
  50 million derived triples by default, and `sparkles infer --max-inferred`,
  `serve --reason-max-inferred`, a request's `maxInferred` and the bindings change the
  limit. A run past the limit fails with the code `inferred-limit` and commits nothing.
  `ReasonOptions::max_memory` bounds the estimated heap of the whole run.
* Re-runs, automatic runs and `sparkles infer` update the previous materialization
  incrementally with the backward/forward algorithm, and write what a full run would.
  Rules that are not monotonic or create blank nodes, large deletions and a changed set of
  input graphs run in full. The kept closure holds only the derived facts, which takes 41%
  to 47% of the memory of a closure with the explicit facts on LUBM-style data.
* A materialization reads the default graph unless data and ontology graphs are named,
  and follows `owl:imports` to graphs of the dataset through a Jena-style location
  mapping ([API.md](API.md#input-graphs-and-imports)). With `imports: fetch`, missing
  imports are loaded into the graph of the import IRI, so later runs work offline.
* RDFS on read matches Fuseki's `--rdfs` and Jena's `ja:DatasetRDFS`
  ([API.md](API.md#rdfs-on-read)). Queries see the RDFS closure of each graph against a
  schema graph by rewriting the query, and the answers equal Jena 6.2's on 55 fixture
  queries. `/$/rdfs/{ds}`, `serve --rdfs` and `query --rdfs` turn it on.
* Query-time reasoning with `reasoning=demand` derives the facts a query's patterns need
  as it runs, from RDFS, RDFS simple, OWL 2 RL or a Jena rule file, without the inferred
  graph ([API.md](API.md#query-time-reasoning), [F17](specs/F17-query-time-reasoning.md#outcome)).
  Jena backward rules answer this way. A demand (magic sets) transformation starts from
  the query's constants and bindings over rules specialized to the schema, so LUBM query 1
  under OWL 2 RL derives no facts beyond the schema. Derived tables are cached per
  snapshot and view, and `maxDerived`, the deadline and the memory budget bound them.
  EXPLAIN shows a `Derive` node per pattern.
* `POST /$/reason/{ds}/explain` returns derivation trees from materialized inferences,
  RDFS on read or the demand engine ([API.md](API.md#explanations),
  [F18](specs/F18-explanations-and-entailment-diffs.md#outcome)). A tree has well-founded
  premises, the rule's variable bindings and passed builtins. Generalized intermediate
  facts and default geometries are explained, and a bounded partial tree marks premises
  that could not be expanded. RDFS-on-read trees can include materialized derivations.
  Schema builds honor cancellation and the deadline.
* The same explanation route accepts `mode: "whyNot"`. It lists candidate rules,
  conditions that block them and small sets of missing facts, with a verified derivation
  under each hypothesis. Fresh blank-node placeholders stand for missing resources.
  `complete` reports whether the bounded search exhausted its candidates. The CLI,
  library, MCP, Python and Node bindings expose both modes, and Explore has a Why-not
  form beside a resource's properties.
* Sparkles records the commit at which inferences were made, and only a later change to a
  graph the run read makes them stale. `GET /$/reason/{ds}`, dataset info, `/$/stats` and
  the `Sparkles-Inferences` header report freshness. Automatic re-runs are opt-in with
  `serve --auto-reason` or `PUT /$/reason/{ds}/auto`.
* `GET /$/reason/{ds}/diagnostics` and `sparkles infer --check` run 17 inconsistency
  checks that cover every OWL 2 RL rule with a `false` conclusion, in JSON or Turtle.
  `literal-not-in-datatype` finds ill-typed literals for the datatypes whose value spaces
  it can decide. The checks never prove consistency.

### SHACL

* `sparkles-shacl` validates SHACL Core and SHACL-SPARQL in parallel over the indexes.
* SHACL 1.2 support covers `sh:targetWhere`, the list constraints `sh:memberShape`,
  `sh:minListLength`, `sh:maxListLength` and `sh:uniqueMembers` with `sh:detail` results,
  and node expressions in the Working Group's `shnex:` vocabulary. Node expressions work as
  targets, constraint parameters, `sh:deactivated`, `sh:values` and `sh:defaultValue`,
  and as constraints.
* Fuseki's `/{ds}/shacl` endpoint, with `graph=default|union|<iri>`, and `sparkles shacl`
  return the report as Turtle, N-Triples, JSON-LD or JSON. Validation covers the data and
  the inferences.
* The SHACL Compact Syntax, `text/shaclc`, works wherever shapes are read or drafted
  ([API.md](API.md#shacl-compact-syntax-shaclc)). That includes `/{ds}/shacl`,
  write-time validation, the CLI, the MCP tools `validate_shacl` and `draft_shapes`, the
  Python bindings and the UI's shapes editor. `sparkles shacl parse` converts shapes files
  between SHACLC and the RDF syntaxes, and the writer refuses a graph that SHACLC cannot
  express and names the triples.

### ShEx

* `sparkles-shex` validates ShEx 2.2 with the `shex` cargo feature, which is on by default
  ([API.md](API.md#shex-validation)). Schemas can be ShExC, ShExJ or ShExR, and ShExR can
  be in any RDF syntax Sparkles reads. `EXTENDS` and `ABSTRACT` are supported.
* Imports can be inline, files under `--load-dir`, or http(s) URLs fetched through the
  outbound policy. `EXTERNAL` shapes, annotations and the Test semantic-action extension
  are supported.
* Shape maps can be compact or JSON, with `{FOCUS p o}` selectors and
  `SPARQL """SELECT …"""` selectors that run within the query budgets and without
  SERVICE. Recursion and negation use stratified greatest-fixed-point typing without deep
  stacks.
* `POST /{ds}/shex` returns JSON, ShapeMap JSON, compact or text reports.
  `sparkles shex validate|parse` accepts Jena's flag names, and `parse --out shexr`
  writes Turtle.

### Write-time validation

* SHACL and ShEx can validate the post-state of every commit before anything is written.
  In `reject` mode a failing write is refused with `422`, and in `warn` mode it is
  committed and the results are reported. Grandfather mode, in both languages, blocks only
  the results a write introduces.
* SHACL shapes come from dataset graphs, a file, or both. ShEx uses a schema, which can
  also live in ShExR graphs of the dataset, and a query shape map expanded per write.
  When a schema is set, its imports are resolved and copied into the database.
* `/$/validation/{ds}`, `sparkles validation` and `serve --validate` configure it in
  `validation.json`. The CLI's `--no-validate` skips it, and writes that bypass
  validation are flagged `unvalidated` in the commit log. If a dataset's guard cannot be
  loaded, its writes are refused rather than accepted unvalidated.
* A write validates only the focus nodes or associations it can affect, against the
  exact result counts of the head kept in `validation-status.json`. It skips writes that
  touch no relevant graph or only predicates no shape reads, and falls back to a full
  validation when the head's state is unknown, the shapes change or too much is affected.
  SHACL-SPARQL constraints anchored at the focus node, `sh:targetWhere` targets and
  `rdfs:subClassOf` changes validate incrementally. For recursive ShEx schemas the guard
  keeps the head's typing and validates only where typings change.
* Results show in `sparkles_validation_*` metrics, the `validation` and `validation_ms`
  access-log fields, CLI summaries and `sparkles stats`.

### Schema discovery and drafted shapes

* `GET /$/schema/{ds}`, `sparkles schema` and `sparkles::schema` list classes and
  predicates with exact counts per graph ([API.md](API.md#constraints-layer)). The counts
  cover triples, distinct subjects and objects, object kinds, datatypes, languages and the
  most objects per subject, kept apart from the RDFS and OWL declarations. The report
  also gives subClassOf roots and cycles, and `detail=subjectClasses` lists the classes of
  each predicate's subjects.
* Pagination uses cursors bound to one snapshot, and time and entry budgets make the
  report fail rather than truncate. An RDF `Accept` header or
  `sparkles schema --format void|turtle` returns a VoID description with class and
  property partitions.
* A constraints layer lists the SHACL constraints per class and says whether each is
  enforced on write, reported on write, or checked only on request. Anonymous
  superclasses, equivalent classes, domains and ranges are rendered in the OWL 2
  Manchester Syntax.
* After a write, the server updates its kept report from the changes instead of
  recounting. On stores above ten million quads, the schema that grounds an Ask comes
  from the index statistics ([API.md](API.md#schema-reports-from-statistics)).
* Class profiles list, for each class, the predicates its instances use, with how many
  instances have a value, values per instance, object kinds, datatypes and value classes,
  and the predicates that point at its instances ([API.md](API.md#class-profiles)).
  Schema diffs compare the reports of two commits, times or snapshots field by field.
  `/$/schema/{ds}/profiles`, `/$/schema/{ds}/diff`, `sparkles schema --profiles` and
  `--diff` and the MCP tools `describe_schema` and `diff_schema` serve them.
* Drafted shapes give each class a SHACL node shape, or a ShEx shape with a query shape
  map, computed from exact per-class profiles ([API.md](API.md#drafted-shapes)). A draft
  can use cardinalities, node kinds, datatypes, `sh:class`, `sh:in`, language
  constraints, patterns, observed ranges, inverse property shapes, sequence paths through
  blank nodes and optionally `sh:closed`. A constraint is drafted when the share of
  instances that satisfy it reaches a support threshold, and at support 1 the current
  data conforms. `GET /$/schema/{ds}/shapes`, `sparkles schema --draft-shapes` and the MCP
  tool `draft_shapes` draft them, reading only the caller's visible graphs.

## Server and protocols

The server speaks Fuseki's protocols and admin API, and adds GraphQL, stored queries and
an OpenAPI description. The MCP server for agents is described under
[AI and agents](#mcp-server).

### SPARQL protocol and Fuseki compatibility

* The server implements the SPARQL protocol, the Graph Store Protocol, upload, the `/$/`
  admin API for datasets, stats, compaction, backups and tasks, and Jena's special graphs
  `urn:x-arq:DefaultGraph` and `urn:x-arq:UnionGraph`.
* A GET without a query at `/{ds}/sparql` or `/{ds}/query` returns a SPARQL 1.1 Service
  Description ([API.md](API.md#service-description)). It describes the services, their
  languages, formats and features, every extension function, aggregate and property
  function, the entailment regime and the default dataset, and links the VoID
  description. Named graphs and counts follow the caller's grants.
* The admin API has Fuseki's `/$/backups` alias, `/$/backups-list`, `/$/stats` with
  request counters, `?state=offline|active`, `deleteOld`, the `/$/validate/*` services and
  assembler bodies on `POST /$/datasets`. Dataset, task and server JSON carry Fuseki's
  members.
* The protocol supports `using-graph-uri`, `?graph=union`, `?target=` on `/shacl`, direct
  Graph Store naming with `--gsp-direct-naming`, `output=`, `results=` and `force-accept`,
  and Jena's RDF Thrift, RDF Protobuf, RDF/JSON, TriX and SPARQL Results Thrift formats.
* `mise run test:jena-clients` tests Jena's own clients against the server. These are
  `RDFConnectionRemote`, `RDFConnectionFuseki`, `GSP`, `DSP`, `QueryExecHTTP`,
  `UpdateExecHTTP` and Jena's RDF Patch writers.
* `sparkles config import fuseki` converts a Fuseki installation
  ([G08](specs/G08-fuseki-configuration.md)). It reads a `config.ttl`, the service files
  of `run/configuration/` and `shiro.ini` or the `fuseki:passwd` file. It writes a `serve`
  script, a data migration script with `tdb2.tdbdump` steps, an auth configuration with
  argon2id hashes and grants, and each dataset's text, spatial, RDFS and inference
  settings. A report marks each element as converted, approximated, manual, ignored or
  unsupported. `serve --fuseki-config` converts at each start and loads `ja:data` into
  memory, as Fuseki does.
* `/$/openapi.json` and `/$/openapi.yaml` serve an OpenAPI 3.1 description of the whole
  HTTP API, which `sparkles openapi` prints and `docs/openapi.json` checks in
  ([X03](specs/X03-openapi-and-completions.md), [API.md](API.md#openapi-description)). A
  test keeps it equal to the server's route table.
* The query result cache has `--result-cache-mb`, `nocache=true`, statistics in
  `/$/stats` and `POST /$/cache/clear/{ds}`.

### GraphQL

* Each dataset can serve read-only GraphQL at `/{ds}/graphql`, configured through
  `/$/graphql/{ds}` and `sparkles graphql` and implemented in the crate
  `sparkles-graphql` ([API.md](API.md#graphql)).
* An administrator installs a mapping schema in GraphQL SDL whose `@rdf` directives name
  classes, predicates and enum IRIs, with `@prefix`, `@lang` and `@single`. Drafts come
  from the write-time guard's SHACL shapes or from the shapes the data supports.
* The server generates lookups, cursor connections with `totalCount`, `node(id:)`,
  filters with `and`, `or`, `not` and nested object filters, and orders with an id
  tie-break.
* A request runs as a fixed number of SPARQL queries on one snapshot, and its cursors name
  their commit. Depth, fetch groups and estimated nodes are limited before a request runs.
  Every query runs with the caller's graph view and protections of triples.
* Introspection, the GraphQL over HTTP media types and status codes, and execution errors
  with null propagation follow the specifications.

### Stored queries

* Named, parameterized queries are stored per dataset in `queries.json`, which backups
  and clones include ([API.md](API.md#stored-queries)). `/$/queries/{ds}`,
  `/{ds}/queries/{name}`, `sparkles queries` and `sparkles::stored` manage and run them.
* Parameters are typed, with defaults and allowed values, and each query has a
  description, example questions and a default result format.
* A run binds each value as one RDF term through initial bindings, never as query text,
  and goes through the same path as `/{ds}/sparql`, with the caller's view, budgets and
  rate limits.
* Every change is a version with its author, message, the dataset's head commit and a
  chained digest, and `If-Match` makes changes conditional. The MCP server offers each
  stored query as a tool.

## Access control and security

Authentication is off by default. Once it is on, access is denied unless a grant allows it.

* `serve --auth-config` turns on authentication and per-dataset access control
  ([API.md](API.md#authentication-and-access-control),
  [C09](specs/C09-dataset-access-control.md)). The access levels are `read`, `write` and
  `admin`, granted by dataset name or pattern, plus `metrics`, `federate` and
  `server-admin`. Hidden datasets answer `404`.
* Callers sign in as HTTP Basic users with argon2id hashes, with API tokens, with OIDC in
  the UI, or through a trusted forward-auth proxy. API tokens are scoped, expiring and
  revocable, hashed at rest, and never have more access than their owner.
* OIDC sign-in for the UI is native, using the authorization code flow with PKCE, with
  back-channel logout and optional idle timeouts. API clients can send the provider's JWT
  access tokens, checked against its JWKS with key rotation, and Cloudflare Access
  assertions are verified. Groups map to roles.
* With an OIDC provider and `serve --mcp`, `/$/mcp` is an OAuth 2.1 protected resource
  ([API.md](API.md#oauth-for-the-mcp-endpoint)). It publishes RFC 9728 metadata that
  names the provider, accepts access tokens whose audience is its own resource
  identifier at `/$/mcp` only, and answers a token that lacks a scope with an
  `insufficient_scope` challenge. The scopes `sparkles:read`, `sparkles:update` and
  `sparkles:admin`, optionally per dataset, only narrow the user's grants.
* `serve --tls-cert --tls-key` serves HTTPS with rustls, HTTP/2 through ALPN and
  certificate reload. Cookies follow CSRF and CORS rules. `sparkles auth login` signs the
  CLI in with a browser loopback or a device code.
* A grant can cover only some named graphs of a dataset, only some endpoints, or both
  ([API.md](API.md#graph-level-access-control), [C12](specs/C12-graph-access-control.md)).
  Each query runs on a filtered view, so every query form, search, schema report, diff,
  change feed and Graph Store request sees only the covered graphs, and a hidden graph
  behaves like a missing one. Writes outside the writable graphs fail before anything
  changes.
* Protections of triples hide triples by predicate, by subject class, by graph, or by a
  SPARQL pattern with the caller's name, roles and groups bound
  ([API.md](API.md#protections-of-triples), [C12b](specs/C12b-triple-access-control.md)).
  Grants lift protections by name. Every read path, including statistics, paths, search,
  `DESCRIBE`, schema reports, diffs, stored queries, MCP tools and RDFS on read, sees only
  the visible triples, and writes of protected triples fail.
* Callers with access to every graph and no protections in force keep the same plans and
  caches.
* Rate limiting is off by default. Each request class gets per-client GCRA buckets and
  concurrency caps, with per-dataset overrides, and a limited request gets `429` or `503`
  with `Retry-After` and `RateLimit` headers. SIGHUP reloads the configuration. With
  auth, a `preauth` stage limits failed credential checks per address and per IPv6 /48
  before any password is hashed.
* Outbound requests from SERVICE, LOAD, imports, embeddings, model providers and
  notifications go through one
  [outbound network policy](USAGE.md#outbound-requests-service-and-load).

## AI and agents

An MCP server gives agents bounded, grant-checked access to the data. The server and the
web UI turn questions into checked queries, keep cited agent memory, ingest documents and
build community reports.

### MCP server

* `sparkles mcp` serves MCP over stdio, and `serve --mcp` serves it over Streamable HTTP
  at `/$/mcp`, with the `mcp` cargo feature, which is on by default
  ([USAGE.md](USAGE.md#mcp-server-llm-agents)). It supports MCP revisions `2026-07-28`,
  `2025-11-25` and `2025-06-18`, with completions, `subscriptions/listen` and the tasks
  extension. `sparkles mcp --url` bridges stdio to a running server with the saved login.
* Its tools list datasets, describe the schema, run bounded SPARQL, explain queries,
  describe resources, find paths, list commits and changes, run full-text and vector
  search, validate with SHACL and ShEx, draft shapes, compare schemas, format SPARQL and
  RDF, run graph algorithms, and run GraphQL. Each stored query is a tool of its own.
* `check_query` checks a query against the schema without running it,
  `similar_queries` finds stored queries for a question, `link_entities` maps names to
  entities, and `recall` returns the facts around a question with a citation for each.
* Results come as a compact table or JSON, and truncation reports the exact total.
  `atCommit` keeps several calls on one snapshot, and `at` reads a past state. Every call
  runs under the engine's budgets with SERVICE off.
* Over HTTP each call runs as the request's caller, so tools, resources and prompts see
  only the datasets that caller may read, and the server's rate limits apply.
* Standard MCP hosts connect over HTTP with OAuth and no hand-made token
  ([USAGE.md](USAGE.md#connecting-with-oauth)). The host finds the OIDC provider through
  the endpoint's protected-resource metadata, signs the user in there and asks for the
  `sparkles:update` scope only when a write tool needs it. `--mcp-require-auth` makes
  anonymous requests start the sign-in even when anonymous callers may read a dataset.
* The `sparql_update` tool is off unless the operator enables it, and is listed only for
  callers that may write. Writes pass write-time validation and carry a commit message,
  `LOAD` is refused, `dryRun` previews, `ifHead` makes a write conditional, and `patch`
  applies an RDF Patch.
* Every tool that reads or writes takes `branch`, and `list_branches`, `create_branch`,
  `merge_branch` and `delete_branch` manage scratch branches. Merges preview by default,
  need the previewed heads and refuse conflicts. `--mcp-scratch-branch-ttl` deletes idle
  scratch branches.
* Resources give each dataset's schema summary and prefixes and each stored query's
  definition. The prompts `explore_dataset`, `answer_question`, `run_stored_query`,
  `explain_term`, `agent_memory` and `ask_graph` start common workflows.

### Questions in plain language

* `sparkles ask`, `POST /{ds}/ask` and the query page's Ask bar turn a question into a
  checked SPARQL query ([C18](specs/C18-natural-language-questions-and-ingest.md),
  [USAGE.md](USAGE.md#asking-questions-with-a-model),
  [API.md](API.md#natural-language-questions)). On DBpedia's billion quads, an Ask takes
  about two seconds with under 512 MiB of anonymous memory.
* The server knows model providers of three kinds, Ollama, the OpenAI protocol and
  Anthropic, with keys from named secrets and an ordered list of models for each role.
  `--model-config`, `GET /$/models` and `POST /$/models/{name}/test` configure and test
  them. The server detects structured output per model and falls back to JSON mode and
  plain text.
* `POST /{ds}/ask` runs as the caller and streams each step as a server-sent event, or
  answers one JSON object ([API.md](API.md#asking-in-the-server)). `assistant.json`,
  through `/$/assistant/{ds}`, turns the assistant on for a dataset and sets what may leave
  the server per provider, with token budgets, a deadline and history retention.
* Each role moves from a cheaper model pair to a stronger one only on signals the server
  checks, such as a repair that still fails, an empty result that `why_empty` blames on
  the query, a failed call, a complex query, or **Try harder**.
* `why_empty` and `POST /{ds}/sparql/diagnose` name the first pattern, join or filter of a
  query without solutions, with a verdict of `query`, `data` or `unknown`.
  `POST /{ds}/check` and `POST /{ds}/recall` run `check_query` and `recall` over HTTP.
* An agent hands a query it wrote to the web UI with the `share_query` tool, and the
  query page opens it with the question, the explanation and the terms it uses, without
  running it.
* The server keeps each person's questions for `historyDays` in `/$/asks/{ds}`, records
  feedback, and counts answers, escalations, tokens and cost in `GET /$/models/usage` and
  `/$/metrics`. `scripts/eval-ask` measures each model on the 68 questions of
  `testsuite/ask/demo.json`.

### Agent memory

* `assert_facts`, over MCP or `POST /{ds}/facts`, writes facts into one named graph per
  source or session, each with an RDF 1.2 reifier that records PROV-O provenance, the
  source, time, caller and an optional confidence and quote
  ([API.md](API.md#memory-writes)). It mints IRIs for new entities, refuses unknown
  predicates and classes, invented IRIs and likely duplicates with suggestions,
  supersedes old values while keeping their record, and makes retries with an idempotency
  key write nothing. Each call is one commit on the normal write path.
* `memory.json` names a dataset's memory graphs, and `recall` marks facts found only there
  as unreviewed. `reviewedOnly` hides memory graphs from an Ask.
  `sparkles auth grant --template agent` prints grants for an agent that writes its own
  graphs and proposes changes to curated ones on branches.
* `sparkles memory import` and `sync` read the memory files of coding agents
  ([USAGE.md](USAGE.md#agent-memory), [API.md](API.md#importing-agent-memory)). They cover
  Claude Code's memory files, `MEMORY.md`, `CLAUDE.md` and rules, Codex's `AGENTS.md` and
  generated memories, `GEMINI.md`, Cursor rules and GitHub Copilot's instructions. Secrets
  are redacted, each file becomes a source, and its facts cite their line span and stay
  unreviewed until a person promotes them.
* A sync writes only what changed. An edit re-anchors facts to where their quotes now
  occur and retracts the rest, and a rename carries them to the new file. With
  `imports.extract: "server"`, a sync starts a server extraction of each changed file.
* Session transcripts of Claude Code, Codex, Gemini CLI, Cursor, the GitHub Copilot CLI
  and Aider, and any file the generic adapter can read, become searchable episodes when
  both the dataset and the person opt in.
* `sparkles memory brief` and `POST /{ds}/memory/brief` print a ranked, bounded and cited
  brief, and `sparkles memory setup claude-code|codex` installs hooks that sync on writes
  and print the brief when a session starts. The `memory` subcommands also cover `init`,
  `sources`, `status`, `recall`, `query`, `assert`, `forget`, `inbox`, `review`,
  `promote`, `reject` and `export --sources`.
* A consolidation task asserts once, in the consolidated graph, the facts that several
  agent graphs repeat and no reviewed graph asserts, and lists duplicates and conflicts
  ([USAGE.md](USAGE.md#maintaining-agent-memory), [API.md](API.md#memory-maintenance)). A
  retention task deletes old session graphs whose facts all appear in reviewed graphs.
  Both run on demand or on a schedule and support dry runs.
* The server counts what waits in each review inbox and reports it in metrics, the
  maintenance answer, a badge in the UI, the brief and the MCP resource
  `sparkles://{ds}/memory/review`. `recall` takes `recency`, a half-life that ranks recent
  and corroborated facts first.

### Document ingestion

* An agent registers a document's text as a source with `register_source`, reads it back
  with `read_chunks`, and writes facts with `assert_facts` and a `span` whose quote the
  server checks against the text
  ([USAGE.md](USAGE.md#ingesting-documents-and-reviewing-memory),
  [API.md](API.md#review-inbox)). A changed text makes a new rendition, and `retractStale`
  retracts facts it no longer supports. Ingest profiles live in `/$/ingest/{ds}`.
* The review inbox, `GET /$/memory/{ds}/inbox`, lists unreviewed agent facts with span,
  link, guard and corroboration signals and the open review branches. `promote`,
  `reject`, `relink` and `edit` act on them as the caller.
* `POST /$/ingest/{ds}` and `sparkles ingest` ingest a Markdown, HTML, plain-text, PDF,
  Word, PowerPoint or OpenDocument document as a task
  ([USAGE.md](USAGE.md#ingesting-documents-in-the-server), [API.md](API.md#ingestion)).
  The server converts it to Markdown, asks the `extract` model role for facts in the
  profile's vocabulary, finds each quote to compute its span, links names to existing
  entities, and writes the facts on a review branch. `mode: "preview"` holds them for
  approval, and `mode: "auto"` merges them when every check passes.
* PDF conversion uses pdf-inspector with the `pdf` cargo feature, on by default, and the
  `pdf-ocr` feature reads scanned pages with OCR models that the operator provides. Office
  files are read by `sparkles-office` under the `office` feature, on by default. On Linux
  each conversion runs in a child process with an address-space limit, which limits
  resources but is not a sandbox.
* A CSV, TSV, Excel or OpenDocument spreadsheet gets a CSVW mapping draft for
  `sparkles csv` per sheet, written by a model from the header and 20 sample rows, with
  the triples of a dry run. IRI columns are linked to existing entities through a lookup.
* `scripts/eval-ingest` scores an ingestion of `testsuite/ingest/sample.json`, and
  `scripts/eval-text2kg` runs Text2KGBench.

### Community reports (GraphRAG)

* A GraphRAG build groups a projection's resources into the Leiden hierarchy and has the
  `community` model role write one report per community, with findings that cite their
  facts ([C22](specs/C22-graph-rag-community-summaries.md),
  [USAGE.md](USAGE.md#community-reports-graphrag), [API.md](API.md#graphrag)).
* Reports are triples with provenance and review status in a reports graph, which a reader
  sees only when its access covers everything the build read.
* A build reports an upper bound of its tokens and waits for confirmation, and reserves
  every request against per-build and daily budgets. A refresh rewrites only the reports
  of communities that changed, and `refresh.mode: auto` starts one when enough facts
  change.
* Ask answers questions about the whole dataset from the reports by map-reduce, and falls
  back to a query for callers who may not read them. The MCP tool `community_reports`,
  `recall` with `communities`, the brief, `sparkles graphrag` and the UI's Communities tab
  expose them.

## Web UI

The SvelteKit UI is embedded in the server binary.

### Query page

* The query page has an editor with completion of declared prefixes, linting and
  formatting in the browser, and results as a table, graph or plan.
* The **Saved** menu lists the dataset's stored queries, shows a form for a query's
  parameters and runs it. An admin can save the editor's query with parameters picked from
  its variables, or delete one.
* The Ask bar has **Preview first** and **Run, then show**, a step indicator, summaries
  whose `[n]` markers select table rows, clarification choices, follow-up questions and
  **Summarize again** ([USAGE.md](USAGE.md#the-ask-bar)). The page keeps a local history
  of questions, saves a question and its query as a stored example, and takes suggested
  examples from readers.
* The explanation of a plan shows beside the plan tree, with links that select
  operators, and a stopped query offers **Explain why**.
* The Pattern pane, opened with `Alt+P`, draws the query's graph pattern beside the editor
  and edits it ([U08](specs/U08-visual-query-builder.md),
  [USAGE.md](USAGE.md#the-pattern-pane)). Classes show as subtitles, filters as chips,
  `OPTIONAL` parts as hulls, and the rest as locked blocks that keep their text. Edits on
  the canvas are splices of the text that one undo reverts. Pickers fed by the schema
  supply nodes and edges, and forms chosen by datatype supply filters. See the
  [pane beside a query](images/pattern.png) and the
  [predicate picker](images/pattern-edit.png).
* **Plan on pattern** draws the plan of the query that ran on its pattern, with estimated
  and actual rows on each edge, misestimates marked, the rows that passed each filter and
  join badges in run order ([U04](specs/U04-live-query-profiling.md),
  [screenshot](images/plan-pattern.png)).
* Graph results above 2,000 nodes draw with the WebGL view, up to 100,000 nodes.
* The path finder also works over a result graph and adds the nodes a path needs
  ([USAGE.md](USAGE.md#finding-paths-on-the-query-page),
  [screenshot](images/query-paths.png)).

### Explore

* The explorer shows an entity's properties, its nearest vector neighbours, ranked
  `text:query` search and a map card with Nearby. The Similar page searches from an entity
  or a pasted vector.
* Explore opens on an overview of the dataset's classes and the predicates between them,
  drawn with sigma.js on WebGL and laid out with ForceAtlas2 in a web worker
  ([U06](specs/U06-large-graph-rendering.md), [USAGE.md](USAGE.md#the-explore-overview),
  [API.md](API.md#graph-overview)). A class opens into its members, or above 5,000
  instances into communities or groups by its best splitting property. **Group by** can
  draw the GraphRAG communities instead, with a community's report beside the picture
  ([screenshot](images/explore-communities.png)). A list view (`L`) is the accessible
  companion, and without WebGL the overview falls back to Cytoscape.
* The **Paths** panel (`Shift+P`) finds the shortest path, every shortest path or the k
  shortest paths between two entities, with filters for length, direction, predicates and
  hubs and an optional weight from reifier properties
  ([U09](specs/U09-path-finder.md), [USAGE.md](USAGE.md#finding-paths-in-explore),
  [screenshot](images/path-finder.png)). A path opens as a query or joins the drawn graph.
* The **Lenses** menu (`Shift+L`) sets a node's size, colour and badge and an edge channel
  on the canvas and the WebGL view ([U10](specs/U10-canvas-lenses.md),
  [USAGE.md](USAGE.md#lenses-on-the-graph), [screenshot](images/explore-lenses.png)).
  Size follows degree or a numeric property, colour follows class, graph or a property's
  values, and the edge lens draws inferred edges dashed. Each lens has a legend whose
  entries select their nodes.
* Algorithm lenses size nodes by PageRank, betweenness, BFS depth, k-core or triangles,
  and colour them by WCC, label propagation, Louvain, Leiden or GraphRAG communities
  ([USAGE.md](USAGE.md#algorithm-lenses), [API.md](API.md#view-only-runs),
  [screenshot](images/explore-algo-lenses.png)). A lens runs over the drawn graph or over
  a projection of the dataset, in the background with progress and **Cancel**.
* A click on a dashed edge, a **Why?** button or `W` opens the derivation tree of an
  inferred triple beside the graph ([USAGE.md](USAGE.md#why-is-this-inferred),
  [screenshot](images/explore-why.png)). **Show on graph** draws and highlights the
  premises, **Next** shows up to five derivations with other top rules, and a note says
  when the inferences are stale ([screenshot](images/explore-why-stale.png)).
* **Facets** (`Shift+F`), in Explore and as a tab of `SELECT` results, count a set's
  values by numeric or year ranges or by the 12 most frequent values
  ([U11](specs/U11-linked-views.md#outcome), [USAGE.md](USAGE.md#facets-and-use-in-query),
  [API.md](API.md#facets), [screenshot](images/explore-facets.png)). Brushing a histogram
  dims the entities outside it, **Apply to query** writes the matching triple and
  `FILTER`, and **Use in query** writes up to 1,000 selected entities as a `VALUES` block.
* Explore's **Map** (`Shift+M`) and a result's **Arcs** draw relations between located
  entities as great-circle arcs coloured by predicate
  ([USAGE.md](USAGE.md#arcs-on-the-map)). Close markers and parallel arcs merge, and a
  list of markers and arcs serves keyboards and screen readers. A selection is shared by
  the maps and the graph canvases.
* `/ui/embedding` draws a vector index's entities on a plane, placed by PCA or a force
  layout of the neighbour graph and coloured by class or a predicate's values
  ([USAGE.md](USAGE.md#the-embedding-map), [API.md](API.md#embedding-map),
  [screenshot](images/embedding-map.png)). An index above 50,000 entities is sampled, a
  lasso selects entities, and **Open as graph** draws them in Explore. A kept projection
  costs about 24 bytes of server memory per entity, with at most four and 1 GiB in all.
* The validation overlay badges the SHACL and ShEx results of the drawn nodes as a lens,
  from the write-time guard, a shapes graph or the Validate panel's shapes
  ([USAGE.md](USAGE.md#the-validation-overlay),
  [U07](specs/U07-data-quality-and-schema-diagram.md#outcome),
  [screenshot](images/validation-overlay.png)). **Show triples** marks the triples
  involved, and **Suggest fix** writes an update, previews it as a dry run and runs it only
  after that preview.

### Maps

* Results with geometry literals get a Map tab with a popup per row. The browser reads
  CRS84, EPSG:4326 and Web Mercator, and other CRSs go through `POST /$/geo/convert`.
* The dataset page's Spatial index panel shows the index's status, CRSs and memory,
  configures or rebuilds it, and maps the indexed geometries in view.
* The maps use MapLibre GL JS over a bundled Natural Earth basemap, and fetch nothing from
  elsewhere unless `serve --map-style-url URL` names a MapLibre style.

### History and time travel

* The UI shows commit history, write receipts, the changes of each commit, named
  snapshots, and query results of a past state.
* A time slider moves the query results, an entity's properties, the Explore graph, the
  schema tab and the Memory tab through the commits, compares two commits or two branch
  heads with changes marked, plays through the commits and follows new ones
  ([USAGE.md](USAGE.md#the-time-slider)).
* The changes between two commits can be drawn as a graph, grouped by class when they are
  many, and played as a time-lapse of one commit's diff per step
  ([USAGE.md](USAGE.md#commit-diffs-on-the-graph)). See the screenshots of
  [a diff graph](images/diff-graph.png) and [a time-lapse](images/diff-lapse.png).
* The History panel draws every branch as a commit graph, and the Branches panel and merge
  page manage branches.

### Datasets, validation and editing

* The dataset page has cards for the full-text, vector and spatial indexes, with create,
  edit, rebuild, drop and a recall measurement for vector indexes. It uploads documents
  for ingestion, edits settings with each field's source, and shows quotas and compaction.
* The schema browser reads `/$/schema`, with graph selection, an inference toggle, and
  observed counts and object kinds beside the declarations and SHACL constraints. Its
  class panel shows profiles, **Compare** shows a schema diff, and **Draft shapes** drafts
  SHACL or ShEx shapes and installs them as a guard in `warn` mode.
* The Validate panel switches between SHACL, in Turtle or SHACLC, and ShEx with a shape
  map, lists each failure, and downloads ShapeMap JSON. The Write-time validation panel
  configures the guard and shows the head's results, the last validated write and recent
  rejections. The query editor shows the results of a rejected write.
* Editing forms generated from SHACL shapes open from Explore's **Edit** or
  `/datasets/{name}/edit?iri=…` ([U02](specs/U02-shape-driven-forms.md#outcome),
  [USAGE.md](USAGE.md#editing-forms), [API.md](API.md#editing-forms),
  [screenshot](images/forms.png)). Fields merge every applying shape and follow
  `sh:group`, `sh:order`, `sh:name` and known DASH editors. The browser checks values as
  the person types, **Preview** runs a dry run, and **Save** commits one update, to `main`
  or to a session branch when `main` is protected. A concurrent change to a field shows a
  conflict view. `Dataset::forms()` and Python's `Dataset.forms` expose the same forms.
* The Memory page shows what memory holds about an entity with each fact's source, quote
  and history, and its Inbox tab reviews facts with **Accept all that pass**. The
  Communities tab shows the GraphRAG hierarchy and reports.
* The server page shows readiness, requests, caches, backups and a Models section for
  providers, the model configuration and write-only keys
  ([USAGE.md](USAGE.md#changing-models-and-keys-at-runtime),
  screenshots of the [form](images/models-settings.png) and its
  [errors](images/models-settings-errors.png)). The model configuration is a form built from
  the `ModelsSettings` schema of the OpenAPI description. Its fields are typed and
  bounded, allowed models are a list that can be reordered, per-model options have a card
  per model, and role lists have a row per entry. Each field is checked as it is edited,
  a refusal from the server shows under the field it names, and the JSON view under
  **Advanced** shares the form's unsaved change.

## Libraries and bindings

The engine is a Rust library, and the Python, JVM and Node.js packages embed the same
engine in process.

* The embedded Rust API is `sparkles::Dataset`, with a fluent query builder in
  `sparkles::querybuilder` ([USAGE.md](USAGE.md#embedding-the-library),
  [P06](specs/P06-library-admin-api.md)). Opening a dataset installs its write-time
  validation, RDFS on read, stored queries and GraphQL schema as the server does. Handles
  on `Dataset` manage snapshots and history, indexes, settings, schema reports, stored
  queries, reasoning, validation, GraphQL and backups, with one `Control` for
  cancellation and progress. A catalog manages datasets, restores and clones. The
  reasoner, validators, backups and GraphQL are cargo features of the `sparkles` crate,
  off by default and all on with `full`.
* The Python package `sparkles` in `crates/sparkles-py` ships abi3 wheels for CPython 3.10
  and later on Linux and macOS ([USAGE.md](USAGE.md#python),
  [P01](specs/P01-python-bindings.md)). It covers datasets, loads, SPARQL, transactions,
  history, search indexes, reasoning and validation, and has an rdflib store plugin and
  the query builder. Calls release the GIL, and Ctrl-C stops a running query.
* `sparkles-jena` is a Jena `DatasetGraph` backed by Sparkles for JVM programs
  ([USAGE.md](USAGE.md#jvm-apache-jena)). It has Jena's transactions, query and update
  engines that run in Sparkles with ARQ as the fallback, bulk loads, historical reads,
  dumps, typed administration, a Fuseki assembler and an import of TDB2 databases. It
  passes Jena's contract tests for datasets, graphs, transactions, updates and prefixes.
* `@sparkles-rdf/engine` embeds the database in Node.js through Node-API, with RDF/JS
  terms, bounded async results, streams, transactions, administration and a Comunica
  source ([USAGE.md](USAGE.md#javascript-and-typescript)). `@sparkles-rdf/client` gives
  streaming SPARQL, Graph Store operations and typed administration over Fetch. Both
  share terms, result types and errors through `@sparkles-rdf/common`.
* `crates/sparkles-client` is a Rust client of remote servers, async on tokio with a
  blocking form ([USAGE.md](USAGE.md#rust-client), [P02](specs/P02-rust-client.md)). It
  speaks the SPARQL 1.1 Protocol and the Graph Store Protocol to Sparkles, Fuseki,
  QLever, Oxigraph or Wikidata and streams results as `oxrdf` terms. On a Sparkles server
  it adds point-in-time reads, stored queries, uploads, batches, commits, datasets, tasks,
  backups, statistics and the schema report. It retries as `Retry-After` and `RateLimit`
  direct, and never repeats a write whose outcome is unknown.

## Command line

One `sparkles` binary carries the server, Jena-style database commands and Jena's file
tools.

* The database commands work on the database directory. They are `load`, `query`,
  `update`, `patch`, `dump`, `compact`, `compaction`, `clone`, `stats`, `log`, `check`,
  `diff`, `snapshot`, `backup`, `repo`, `infer`, `shacl`, `shex`, `validation`, `schema`,
  `queries`, `graphql`, `text-index`, `vector`, `geo-index`, `quota`, `describe-settings`
  and `vocab-index`. `load`, `query`, `update`, `patch`, `vector`, `compaction`, `quota`
  and `describe-settings` also work on a remote server with `--server`.
* Jena's file tools are built in ([G05](specs/G05-command-line-tools.md),
  [USAGE.md](USAGE.md#file-tools)). `convert`, alias `riot`, parses, validates, counts and
  converts RDF files as a stream with exact error positions. `qparse` and `uparse` print a
  query or update formatted, as SPARQL algebra in SSE, or as the physical plan. `compare`,
  alias `rdfdiff`, compares two files up to blank-node isomorphism with RDFC-1.0.
  `rsparql` and `rupdate` query and update any SPARQL 1.1 Protocol endpoint, `rset`
  converts result sets, and `rdfpatch` prints the rows and counts of RDF Patch files.
* `iri` and `langtag` show components, normal forms, canonical case and problems. Their
  rules give warnings in `convert --check`, `load --check` and `/$/validate/iri`, and
  `--strict` turns the warnings into failures.
* `sparkles completions` prints shell completions for bash, zsh, fish, elvish and
  PowerShell, and `sparkles man` prints man pages for every subcommand
  ([USAGE.md](USAGE.md#shell-completions-and-man-pages)).

## Formatter and editor support

The formatter and linter check their own output and serve the command line, the server,
the UI and editors.

* `sparkles-fmt` formats SPARQL queries and updates, Turtle, TriG, N-Triples, N-Quads and
  JSON-LD, through `sparkles fmt` and `POST /$/format` with the `fmt` cargo feature, on by
  default ([USAGE.md](USAGE.md#formatting), [API.md](API.md#formatting)). It keeps
  comments, and its output must parse to the same SPARQL algebra, RDF dataset or JSON and
  format to itself. Style options cover line width, indentation, prefix groups, the `a`
  shorthand, IRI compaction, quote style, operator position, aligned `VALUES`,
  `prune-prefixes` and directive style. The command line walks directories with ignore
  files, discovers config files, and has `--check`, `--write`, `--diff` and Prettier's
  exit codes.
* Turtle and TriG support `sort`, `prune-prefixes`, `turtle-layout = "conventional"` next
  to the default diff-friendly layout, and the ignore pragma. Files over `--max-bytes`
  format statement by statement in bounded memory, unless they are sorted.
* N-Triples and N-Quads get canonical term spelling with comments kept, in bounded memory
  on every core. `--sort` merges identical statements with an external sort past
  `--sort-memory`, and `--canonicalize` writes RDFC-1.0 blank node labels.
* JSON-LD uses Prettier's JSON layout with keywords first, and `--sort` sorts terms.
* `sparkles lint` checks SPARQL, Turtle and TriG with seventeen rules
  ([X04](specs/X04-linter.md), [USAGE.md](USAGE.md#linting)). They cover undefined and
  unused prefixes, unused, single-use and unbound variables, cartesian products,
  `SELECT *` with grouping, ungrouped variables, FILTER scope and equality, IRIs with
  spaces, language tags, suspicious and redundant datatypes, and deprecated names.
  Severities are set in `.sparklesfmt.toml` or with `--rule`, and `--fix` applies safe
  fixes after checking that the meaning is unchanged. The linter also runs as
  `POST /$/lint` and in the query editor.
* `sparkles lsp` is a language server for every language the formatter handles. It
  formats documents and ranges, reports syntax errors and lint findings, and offers safe
  fixes as quick fixes. [editors.md](editors.md) has setups for Neovim, Helix, Emacs and
  VS Code.
* The UI's editors use a WebAssembly build of the formatter, `sparkles-fmt-wasm`, and fall
  back to `POST /$/format` when it is missing
  ([ui/README.md](../ui/README.md#formatting-in-the-browser)). The Nix `sparkles` and
  `sparkles-ui` packages always include it.

## Operations

### Deployment

* The Nix flake builds the `sparkles` package with the web UI, the formatter's
  WebAssembly module, shell completions and man pages. Its NixOS module
  `services.sparkles` runs the server as a hardened systemd service with datasets,
  authentication, backups, TLS, metrics, OpenTelemetry and an optional nginx virtual host
  ([USAGE.md](USAGE.md#deploying-on-nixos)).
* The Home Manager module `programs.sparkles` installs the CLI for one user, writes the
  memory, backup and formatter config files, and can declare the memory hooks and skill
  of Claude Code and Codex ([USAGE.md](USAGE.md#home-manager)).
* The `Dockerfile` builds an image with the server and UI that runs as an unprivileged
  user, and `compose.yaml` serves it on the loopback address with the data in a named
  volume ([USAGE.md](USAGE.md#docker)). An `ocr` target adds PDFium and ONNX Runtime at
  pinned, checksummed versions, and the flake builds both images as OCI images.
* A Helm chart in `deploy/helm/sparkles` runs the server as a single-replica StatefulSet
  with a read-only root filesystem, configuration from values and Secrets, three ways to
  deliver local models, and reloads through `serve --watch-config`
  ([USAGE.md](USAGE.md#kubernetes-helm)).

### Settings and notifications

* A dataset's assistant, memory, ingest and prefix settings combine built-in defaults,
  the declared settings file of `serve --settings`, and runtime changes through
  `/$/settings/{ds}/{kind}` ([C19](specs/C19-layered-settings.md),
  [API.md](API.md#settings), [USAGE.md](USAGE.md#declarative-dataset-settings)). The file
  declares defaults and per-dataset entries and can lock fields. Each answer names the
  source of every field, writes take `If-Match`, and SIGHUP reloads the file.
* `sparkles settings` reads settings with each field's source, sets and resets fields,
  edits the runtime layer, lists overrides, and applies a settings file
  ([USAGE.md](USAGE.md#changing-dataset-settings-from-the-command-line)). The NixOS
  module generates the file, checks it at build time, and reloads the server when it
  changes.
* Server administrators change model providers, role lists and budgets through
  `/$/server/settings/models` and store write-only keys through
  `/$/server/secrets/{name}` without a restart ([API.md](API.md#server-settings)). Keys
  are sealed with the key of `serve --secrets-key`, the systemd credential
  `sparkles-secrets-key`, or a key generated in the data directory.
  `sparkles settings --global`, `sparkles secrets` and the UI make the same changes, and
  presets fill in Anthropic, OpenAI and Ollama providers.
* A provider with an `https` endpoint can trust an internal CA through `tls.caCert`, or
  turn certificate checks off with `tls.insecureSkipVerify`, which the server logs and the
  UI marks as unverified ([USAGE.md](USAGE.md#internal-certificates-and-tls-options)).
* Declared prefixes default to the well-known prefixes and the namespaces Sparkles
  defines ([C20](specs/C20-declared-prefixes.md), [USAGE.md](USAGE.md#declared-prefixes)).
  The settings file declares and locks them, and a dataset admin changes them through the
  settings API, the Settings tab or Fuseki's `/{ds}/prefixes`. Loaded data never replaces
  a declared prefix.
* The server pushes signed Standard Webhooks and ntfy notifications when memory review
  waits too long, a backup policy run fails, or a provider's daily token budget is used
  up ([C21](specs/C21-outbound-notifications.md), [API.md](API.md#notifications),
  [USAGE.md](USAGE.md#sending-notifications)). Channels and routes are a server settings
  kind, failures are retried with backoff, and `sparkles notify status` and
  `sparkles notify test` show and test channels.

### Memory and resource limits

* The block, result, SERVICE and history caches are sized once for the server and shared
  by all datasets, with per-dataset bytes, hits and misses
  ([X08](specs/X08-disk-spill-and-server-memory.md#gates)). The `small` memory profile,
  set with `--memory-profile`, saves about 70 MiB of RSS at 10.5M triples.
* A query memory pool, set with `--total-query-memory-mb`, bounds the estimated memory of
  all queries and updates together. Requests wait in arrival order for a share, answer
  `503` with `Retry-After` after `--query-admission-wait`, and fail with the
  `server-memory` budget when the pool cannot cover them. A finished eager SELECT or ASK
  keeps only its result's estimate, and cursor charges release with their live state.
  `GET /$/server/memory` and the `sparkles_query_pool_*` and
  `sparkles_query_admission_*` metrics report it. Embedded catalogs take a pool through
  `MemoryOptions`.
* Per-query budgets cap estimated intermediate memory, response size, rows, and the rows
  all operators produce ([C01](specs/C01-observability-and-budgets.md)). A request can
  lower a budget but never raise it, and a query over budget fails with `507`. Queries and
  writes stop when their client disconnects.
* Persistent datasets have storage quotas, a default with `--max-dataset-mb` and per
  dataset through `/$/quota/{ds}` and `sparkles quota` ([API.md](API.md#storage-quotas)).
  A write that would exceed the quota is refused with `507` before it commits, while
  reads and deletes continue.

### Observability

* Every request gets an `X-Request-Id` and one structured access-log line in text or
  JSON. `/$/metrics` serves Prometheus metrics and `/$/ready` readiness. The metrics
  include each query's rows and peak memory and index rebuilds by reason and duration.
  `--metrics-fuseki-names` adds Fuseki's request metric names, and `--metrics-addr` serves
  the metrics on a second address.
* OpenTelemetry, with the `otel` cargo feature, is off at run time unless `--otel` or
  `OTEL_*` variables enable it. Traces go out over OTLP, W3C `traceparent` is read from
  requests and sent on SERVICE and LOAD, and spans cover commits, background tasks, query
  phases and the operator tree. The Prometheus registry is bridged, and OTLP logs with
  trace correlation are optional.

### Backups

* `sparkles-backup`, with the `backup` cargo feature, on by default, keeps online backups
  in a file system or S3, including AWS, MinIO, R2 and Ceph RGW
  ([API.md](API.md#backup-repositories), [USAGE.md](USAGE.md#backup-repositories),
  [F05](specs/F05-snapshot-repositories.md)). A backup holds the writer lock for only a few
  system calls, and an in-memory dataset is backed up through a temporary generation.
* Backups are incremental and deduplicated in content-addressed 32 MiB pieces, and only
  the appended bytes of the WAL and catalog are copied. Repositories can be encrypted with
  a local key or passphrase, and encryption is off by default.
* A restore goes to a new dataset or replaces one in place, follows dataset-id rules, and
  runs an integrity check before publishing. During an in-place swap, requests get `503`
  with `Retry-After`. Backups can be verified at three levels.
* Lifecycle policies run on cron or `every` schedules in an IANA time zone, with catch-up,
  retention and two-phase GC, and can run offline against a stopped catalog. Lease locks
  use the storage server's clock, so several servers and the CLI can share a repository.
* `/$/repositories`, `/$/backups/{ds}`, `/$/backup-policies`, `sparkles repo`,
  `sparkles backup` and the UI's Backups page manage backups, which report
  `sparkles_backup_*` metrics and audit events.

### Upgrades and on-disk formats

* One registry in `sparkles-core` lists every kind of file with the version a build
  writes and the versions it reads, and `GET /$/server` reports it
  ([X12](specs/X12-release-engineering.md), [USAGE.md](USAGE.md#upgrading-databases)). A
  newer version is refused with a message that says what to do, and a dataset in an
  unreadable format stays closed while the others are served.
* The generation index is read from a set of formats, so a format change lands at the
  next compaction, and backups restore when their index format is in that set.
* `sparkles migrate` lists a stopped server's file versions and runs upgrade steps on a
  checked copy that it swaps in, keeping the dataset id and history. Fixtures in
  `testsuite/formats/` test that older files open, query, write, compact and restore.
* On SIGTERM or SIGINT the server stops accepting connections and gives requests in
  flight `--shutdown-grace` seconds to finish. It then cancels the rest, and a cancelled
  write commits nothing.

## Known gaps

These are implementation limits, missing controls, and features that other RDF stores
have and Sparkles does not. [COMPARISON.md](COMPARISON.md) covers each engine, and
[BENCHMARKS.md](BENCHMARKS.md#where-sparkles-loses) covers performance. Each gap names
the [roadmap](ROADMAP.md) items that address it, and the roadmap orders that work and
links each item to its design spec.

### Storage and execution

* The largest measured dataset is English DBpedia, 1.24 billion triples on one machine.
  Nothing larger, such as Wikidata, has been tried. Roadmap item 1.
* FSST vocabulary compression is off by default. With it, a DBpedia export takes 1.05
  times as long as with front coding, and warm queries that decode many strings run 4% to
  12% slower. Single-term lookups are the follow-up in roadmap item 160.
* IRI encoding and zstd or frame-of-reference block codecs were measured and are not
  built, because they saved less than their targets on DBpedia. Predicate patterns do not
  answer `COUNT(*)` per predicate, which counts triples. Roadmap item 16.
* Cold `values-star` and `path-plus` were slower than Oxigraph at 1.05M and 10.5M, as was
  cold `star-lookup` at 10.5M, and that comparison has not been repeated on index format
  3. Index format 3 makes cold `star-join` at 10.5M 6% to 8% slower. Dense reads on the
  1.01B dataset are unmeasured. Roadmap item 4.
* Group commit on datasets with a full-text, spatial or embedding index has measured
  costs ([X09](specs/X09-group-commit-by-default.md#179-performance-gates)). The p90 of
  single spatial inserts at 10.5M is 1.077 times the ordinary path's, which is roadmap
  item 111. A fence synchronizes in 1.4 to 1.5 ms against about 1.0 ms for one ordinary
  commit, which is item 112. Reader latency on a spatial dataset at 1.05M under mixed load
  is not established, item 171.
* A headerless WAL that the current release has never opened for writing keeps the mixed
  legacy rules for the bytes it holds, so a torn record that an older build left there can
  be read as a legacy commit or refused
  ([X09](specs/X09-group-commit-by-default.md#phase-3)). Roadmap item 3.
* Most plans run eagerly unless the caller asks for `execution=streaming`. Sorts without
  LIMIT and DESCRIBE collect their whole input, and an EXISTS evaluated once per outer key
  stays eager because the cursor is slower there. The UI's result table, the MCP tools and
  the Python and JVM query calls run eagerly, because they report exact totals or return
  collected results. Roadmap item 7.
* Disk spill is off by default. With spill on, a DISTINCT over an ordered input other than
  an ORDER BY keeps its whole set, a grouping's single group can outgrow the budget, and
  eager operators still fail at their budget. A spilled join that must keep its order
  writes its output once more to merge it, which made one join under 4 MiB 8.9 times
  slower than the same cursor without a memory limit. Grouping above an ordered spilled
  join is slow. Roadmap item 93.
* There are no pinned results, materialized views or live query monitoring. Roadmap items
  17 and 27.
* A query whose plan would nest deeper than 4000 levels is refused, so a basic graph
  pattern holds at most 1998 triple patterns ([nesting limits](API.md#nesting-limits)).
  Planning time grows with the square of the number of triple patterns, and a star of
  1998 patterns on one frequent predicate takes about 3 s to plan. The explanation of
  `/{ds}/sparql/explain` reads plans of at most 256 levels. Roadmap item 84.
* A SERVICE call to this server runs in process only from the SPARQL query and update
  endpoints, so one from an MCP tool or GraphQL waits behind its caller's grant. SHACL,
  ShEx validation outside its selectors, diff, and the `/$/stats`, `/$/schema` and
  `/$/validate` reads hold their admitted grant idle while their own query takes a second
  one. An eager CONSTRUCT or DESCRIBE result keeps its query's peak estimate in the pool
  until it is released. Roadmap item 7.
* The server-wide caches count the bytes of the values they hold, and the process uses
  more. The heap behind the result cache is about 1.2 times the bytes it counts, and
  evicted entries leave free pages in thread heaps. Ten datasets at `--cache-mb 32` use
  1.157 times the anonymous memory that one dataset and the cache bytes predict. Roadmap
  item 88.
* SP²Bench `q4` at 10M answers 1.96 GB of TSV, more than the default response budget of
  1 GiB, so it answers only with a larger `--max-result-mb`. Jena is faster on `q12a`
  and Oxigraph on `q12b`. Roadmap item 91.

### Versioning and history

* A past state can be read only while a retained generation holds it, even when the
  change log has its changes. Full-text search works at the head only. Roadmap items 23
  and 52.
* A bulk commit into a non-empty dataset is recorded in the change log with counts only
  when it loads or drops more than 1,000,000 quads, even if few of them change. After a
  crash right after a bulk commit, the log can lack that commit's changes. Roadmap item
  52.
* A patch is applied as one commit even when it holds several `TX … TC` transactions, and
  a patch from another dataset cannot name its existing blank nodes. There are no patch
  logs and no replicas that follow a server. Roadmap items 66 and 74.

### SPARQL and formats

* Sparkles lacks ARQ's JavaScript functions, `afn:context`, the Leviathan `lfn:` library
  and the `JSON` query form, and `afn:collation` orders by code point instead of by
  locale. Roadmap item 69.
* Sparkles does not evaluate `shortest(…)` paths, which ARQ does not evaluate either.
  Roadmap item 51.
* Jena's service enhancer has no slice-aware cache and no `se:cacheRm` or `se:cacheLs`
  functions. Roadmap item 38.
* There is no openCypher frontend and no JSON query language. Roadmap items 20 and 72.
* The loader reads TriX, RDF Thrift, RDF Protobuf and RDF/JSON by first rewriting each
  file as N-Quads in a temporary file. The library's `load_str` and `dump_graph` take only
  the W3C syntaxes, and SPARQL `LOAD` reads TriX but not the other three. Roadmap item 73.
* Mappings read no PostgreSQL or DuckDB database and no Parquet file in an object store.
  Roadmap items 230 and 237.
* RML join conditions written as expression maps are not read, and function maps call 20
  of SPARQL's functions. Roadmap items 232 and 236.
* Stored mapping runs are not tasks, the web UI has no Mappings page, and the JVM and
  Node.js bindings cannot run mappings. Roadmap items 231, 233 and 234.
* The W3C CSVW test suite does not run. Roadmap item 235.
* A template lookup that binds a blank node of the dataset writes a new node, and a JSON
  number with a fraction or an exponent loses its source form. Roadmap items 238 and 239.
* Tables and databases are imported, not queried in place. Roadmap item 43.

* Incremental views are `table` views on `main`, maintained after the commit. There are
  no `graph` views, no maintenance inside the commit, no WebSocket or MCP access, no Node
  or JVM API, no UI, no views per branch, no ordered windows and no provenance per fact.
  Roadmap item 17.
* Backups and clones do not carry view definitions. Roadmap item 220.
* Views and live queries on a dataset with RDFS on read or materialized inferences
  evaluate the whole query again at each commit that touches them, rather than
  maintaining it incrementally. Roadmap item 221.
* A caller reads a view only with full access or the principal's exact access. Roadmap
  item 222.
* View passes run on one thread per dataset, and the live query limits count per
  dataset. Roadmap items 223 and 224.
* Views and live queries never read the graphs that a disclosure guard protects. Roadmap
  item 227.

### Search and graph algorithms

* Full-text search uses Tantivy's Snowball stemmers by default, and the analyzers that
  reproduce Lucene's are selected per language. Lucene's analyzers for other languages,
  such as Greek and Arabic, have no counterpart. Roadmap item 251.
* `zh-dict` segments Chinese by jieba's word frequencies, so its tokens differ from those
  of Lucene's SmartChineseAnalyzer. Kuromoji's normal and extended modes and the user
  dictionaries of Kuromoji and Nori have no counterpart. Where a run of more than 1,024
  UTF-16 units of unknown characters reaches that limit inside a surrogate pair, `ja-dict`
  and `ko-dict` cut it before the pair while Lucene cuts inside it, which can shift later
  positions. Roadmap item 250.
* Full-text documents have one field. Roadmap item 35.
* Enabling or reconfiguring a full-text index builds it while writes wait. A write
  transaction cannot search the full-text index after it changes data, because the index
  covers committed data only. Roadmap item 23.
* A vector index has no quantization, so its packed vectors take 4 bytes per dimension,
  roadmap item 34. Each pass that inserts new rows into a vector index's graph scans all
  rows inserted since the last compaction, item 82.
* A weighted shortest search with `path:maxLength` lists paths in order of cost until one
  is short enough, so a pair whose short paths are all dear runs until the visit limit.
  The UI's path finder always sends a length limit and stops at a million visits. Roadmap
  item 275.
* Weighted `path:allShortest` without `path:maxLength`, cycles through the source and
  weighted searches to several targets run Dijkstra's algorithm from the source alone. Every cheapest path of one pair
  takes 0.9 s on the 10.5M-triple benchmark data. Roadmap item 277.
* Graph algorithms match the Graphalytics reference outputs for BFS, WCC and PageRank on
  the two small official example graphs, through `scripts/graphalytics-reference.py`.
  There is no full Graphalytics driver, no run on the larger datasets, no reference check
  of CDLP, LCC or SSSP, and no certification. Roadmap item 8.
* The UI cannot complete `algo:` names, the JVM binding has no typed algorithm handle,
  and openCypher procedures wait for an openCypher frontend. Write-back commits through
  the ordinary writer rather than a bulk writer. A write-back run has no deadline unless
  the request or `--update-timeout` gives one. Thread admission has no queue order, so a
  call that needs several threads can wait while one-thread calls take the free slots. A
  background view-only run reports progress by phase only. Roadmap item 8.
* Betweenness requires finite, strictly positive edge weights, while SSSP accepts zero
  weights and PageRank non-negative ones. Roadmap item 79.
* Weighted betweenness refuses a distance addition that cannot keep a strict increase at
  `f64` precision and refuses non-finite distances. Accepted distances and path counts are
  approximate, including rounded path ties, and sampling checks only the selected source
  searches. Roadmap item 80.

### GeoSPARQL

* EPSG codes come from a table of proj4 definitions rather than the EPSG database, and
  the library includes the table only with its `geo-epsg` feature. Geographic CRSs on
  datums other than WGS 84, grid-based datum shifts, and curved GML segments or solids
  are not supported, and Helmert datum shifts are approximate. Roadmap item 24.
* Geometry constructions return 2D results. Unions of crossing lines keep the covered
  points but do not split the lines at each crossing, unlike Jena's JTS engine. Roadmap
  item 70.

### Reasoning and validation

* Inference is forward materialization, RDFS on read and derivation on demand.
  Derivation on demand has no cost-based choice against a materialization, no OWL 2 QL
  rewriting, no reuse of tables across commits and no dataset setting, and Jena's hybrid
  rules are flattened. Roadmap item 19.
* With cached tables, the selective LUBM queries take 1.4 to 2.5 times as long on demand
  as over a materialization under RDFS and 2.5 to 17 times under OWL 2 RL, and queries
  that read millions of facts take tens to over a hundred times as long. Roadmap item 190.
* A query on demand that reads schema facts uses the rules unspecialized, and schema facts
  that follow only from derived instance facts may be missing from its answers, item 191.
  The derived-table cache's budget accounting is item 192. DESCRIBE is refused under
  derivation on demand, item 193.
* Entailment diffs, minimal inconsistency justifications and provenance-based inferred
  visibility remain roadmap item 18. Query result cells do not offer a Why? action,
  item 285. `why_empty` adds hypotheses for a grounded failing intensional pattern.
  Abduction for patterns with unbound target variables remains item 286.
* Derived facts use the ordinary inferred graph. A separate derived-fact layer and
  canonical `owl:sameAs` representatives are not built. Roadmap item 9.
* Incremental maintenance does not cover rules with `noValue`, `now` or new blank nodes,
  or RDF lists that change under OWL 2 RL. Those runs, and the first run after a
  compaction without retained history, recompute everything. Inconsistency checks cover a
  fixed subset of OWL 2 RL. Roadmap items 9 and 53.
* Because the kept closure holds only derived facts, changes of 100 triples at 16 threads
  over it take 1.6 to 2.3 times as long as with a closure that also holds the explicit
  facts, and undos at 1 thread take 1.3 to 1.8 times, on LUBM-style data. Roadmap item
  113.
* ShEx 2.2 has no `RESTRICTS` and no `EXACTLY` reference syntax. SHACL 1.2's reifier
  constraints, `sh:uniqueValuesFor`, `sh:subsetOf`, `sh:someValue`, `sh:rootClass`,
  `sh:singleLine` and custom functions called from SPARQL are not supported. Roadmap item
  13 keeps them out of scope, and the Outcome sections of
  [G02](specs/G02-shex.md#roadmap-follow-ups-2026-10-12) and
  [G03](specs/G03-shaclc.md#roadmap-follow-ups-2026-10-12) list them.
* `sparkles fmt` and the UI's Format button do not format SHACLC. Roadmap item 48.
* Drafted shapes reference other class shapes only at support 1, have patterns only for
  all-string properties and ranges only for inline numbers and dates, go one level deep in
  sequence paths, and write sequence paths and date ranges into ShEx drafts as comments.
  Roadmap item 13 and [C02](specs/C02-schema-discovery.md#roadmap-follow-ups-2026-10-12).
* Write-time validation validates recursive SHACL shapes, and SHACL-SPARQL constraints not
  anchored at the focus node, in full on every write. Roadmap item 78.
* ShEx keeps no typing when the process is killed or exits without closing the store, so
  the first writes afterwards to a recursive schema over a large connected graph may
  validate in full. A ShEx schema copy edited outside Sparkles is trusted without a digest
  check. Roadmap items 54 and 83.
* Validation endpoints refuse callers whose grants cover only some graphs or triples.
  Roadmap item 75.

### Server, access control and operations

* There are no JVM metrics. Assembler bodies are read only for the part that maps to a
  dataset. Direct Graph Store naming is chosen per dataset at start with
  `--gsp-direct-naming` and not through the settings API. Roadmap item 76.
* There is no ontology object API. Roadmap item 71.
* The change feed refuses callers whose protections depend on the data, and protections
  hide triples, not the IRIs that visible triples mention. Roadmap item 75.
* GraphQL has no mutations, subscriptions, stored GraphQL queries, `persistedOnly` or UI
  page. Nested lists are not connections, and a nested `first` limits the response while
  the engine still produces every value of the field. Parent ids reach a child query as
  terms rather than engine ids, and blank node parents each take a `UNION` branch.
  Roadmap item 37.
* Stored queries have no stored updates, no parameters bound to several values at once,
  and no private queries per user. Roadmap item 39.
* Sparkles runs as a single node on local disk. There is no clustering and there are no
  replicas, roadmap item 66. Live RDF data on local disk is not encrypted at rest, item 65.
* Backups cover individual datasets, with no whole-server backup, roadmap item 61.
  Encryption keys for backup repositories cannot be managed through the API, Vault or a
  cloud KMS, item 65.
* Without `--secrets-key`, runtime keys are sealed with a key that the server generated in
  the data directory, which does not protect them from someone who can read that
  directory. An operator who wants no keys on disk locks the secrets in the settings file.
  Roadmap item 77.

### AI and agents

* Ask has been evaluated with Anthropic models only, on the demo set, CK25, QALD-9-plus
  and 74 DBpedia-only questions
  ([C18](specs/C18-natural-language-questions-and-ingest.md#roadmap-follow-ups-2026-10-12),
  [BENCHMARKS.md](BENCHMARKS.md#natural-language-questions)). On DBpedia the best model
  answers 40 of 107 QALD-9-plus questions. Most DBpedia questions stop at a
  clarification, because `link_entities` offers a category or a namesake beside the main
  resource, and the grounding seldom lists the predicate a question needs, roadmap item
  263. Local and OpenAI-compatible models, LC-QuAD 2.0, which needs a Wikidata load from
  the full dump, and an evaluation of independent roles have not run. Roadmap item 6.
* The UI's ingestion controls do not turn on assistant ingestion. Roadmap item 77.
* An MCP tool operation that needs admin, such as deleting a protected branch, refuses an
  OAuth token without `sparkles:admin` as a tool error instead of a scope challenge.
  Roadmap item 270.
* Agent memory does not import from Cursor's SQLite store or the editors' Copilot Chat
  storage, and redaction finds only the secrets its patterns describe. Roadmap item 15.
* The server does not convert images or audio. Scanned PDF pages need a build with
  `pdf-ocr` and OCR models from the operator. The Office converter is not registered with
  the query memory pool, and a workbook's token estimate for `confirmTokens` does not
  cover all its sheets. Roadmap items 15 and 77.
* A CSV mapping draft links an IRI column's values only to entities whose labels match
  exactly or after folding case and spacing. A value with several matches keeps a minted
  IRI until a person chooses, and the lookup reports only the first variant of an
  ambiguous group. Roadmap item 15.
* There is no shared profile that connects sources, cited assertions, sessions and
  imported memory to personal notes, decisions and tasks with evidence, dependency and
  validity semantics. Roadmap item 81.
* A dataset has one GraphRAG configuration, so readers with different views cannot each
  have reports built from their own view, roadmap item 100. The evaluation of global
  answering covers one generated question set on the demo graph, compares it with BM25
  rather than vector retrieval and has no samples checked by a person, items 260 and 261.
  `auto` mode answers about half of the questions about the whole graph locally, item
  262. An automatic refresh counts changes from the change feed rather than from an
  incremental view, item 12.

### Web UI

* Forms edit predicate and inverse paths with literal and IRI values. Blank node values
  with `dash:DetailsEditor`, list editors for `sh:memberShape` and choices for `sh:or` and
  `sh:xone` are not built, and ShEx schemas do not drive forms. A check without a guard
  does not validate shapes that reach the resource through more than two triples.
  Roadmap item 21.
* The Pattern pane builds and edits basic graph patterns, filters, one level of
  `OPTIONAL`, simple paths, the projection and the modifiers. `UNION`, `GRAPH`, inline
  `VALUES`, `MINUS` and `FILTER NOT EXISTS` are not editable on the canvas, a waiting node
  is not kept across a reload, and the pane needs the formatter's browser module with no
  server fallback. Roadmap item 101.
* The plan drawn on the pattern shows the numbers of a plan after it ran or was
  explained, and does not update while a query runs. Roadmap item 27.
* Layouts are not computed or cached on the server and there is no binary transfer
  format, so a WebGL view lays out every result in the browser. Frame-rate and main-thread
  targets have no automated test on a GPU runner, and the overview opens one path below
  one class at a time, with no grouping by named graph. Roadmap item 29.
* The embedding map cannot colour by a kept Leiden run, `Shift+M` does not open it, date
  facets count whole years only, and at phone width the facets sit below the graph. The
  Explore map and facets read one dataset on its main branch. The `VALUES` block of
  **Use in query** is a locked block on the Pattern canvas. Roadmap item 140.
* The frame rate of arcs and of the embedding map has been measured only in software
  rendering, roadmap item 141. Projections are kept in memory only and are not offered to
  callers with partial access, item 142.
* The validation overlay validates the drawn nodes instead of reading a recorded result,
  roadmap item 120. Under an incremental guard with other results in the dataset, a
  preview cannot say whether a fix resolves its result, item 121. Callers whose grants
  cover only some graphs get no overlay, item 75.

### Bindings and packaging

* There is no WebAssembly build of the engine. Roadmap item 44.
* The Python package is not on PyPI and has no PyPy or free-threaded wheels. The JVM
  library is not on Maven Central, and the JavaScript packages are not on npm. Release
  workflows build for Linux on x86_64 and arm64 and for macOS on Apple silicon only, with
  no Windows or Intel macOS packages, and Linux is the platform tested locally. The JVM
  library has no musl build. Roadmap item 58.
* JVM functions registered in Java run through ARQ. Roadmap item 25.
