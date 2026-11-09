# Storage and history capabilities

This page describes the available capabilities and their current limits. The
[capability matrix](../FEATURES.md#capability-status) records delivery status within
each stated scope.

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
  ([X11](../specs/X11-index-compression-and-predicate-patterns.md#item-108-faster-fsst-reads)).
* A numeric column beside the base vocabulary, `vocab.num`, holds the exact value of
  every numeric literal whose lexical form is not canonical, such as DBpedia's
  `xsd:float` coordinates or any `xsd:int`. Range filters, sorts and aggregates over these
  values read 8 bytes per value instead of decoding the key. Loads and compactions write
  the column, and `sparkles vocab-index` adds it to a database that lacks it
  ([USAGE.md](../USAGE.md#command-line-tools)).
* The planner keeps statistics for predicates, with their counts and distinct subjects
  and objects, and for classes and graphs.
* Predicate patterns implement QLever's pattern trick
  ([X11](../specs/X11-index-compression-and-predicate-patterns.md#outcome)). Each generation
  stores every subject's set of predicates by number, for the default graph and for the
  union of the named graphs. The patterns answer `GROUP BY ?p` with `COUNT(DISTINCT ?s)`
  over `?s ?p ?o`, `SELECT DISTINCT ?p` over the same shape, and `spk:hasPredicate` with
  its QLever name `ql:has-predicate`, corrected for the snapshot's delta. Class profiles
  use them to skip predicates that no profiled instance has. Under a protection of
  triples, RDFS on read, a set of named graphs or a large delta, the same queries read the
  permutations.

### Cold reads

These read paths are tuned for queries over data that is not yet in memory
([X07](../specs/X07-cold-sparse-reads.md), [API.md](../API.md#the-io-profile)).

* A query hints the vocabulary groups of all its constants in one batch and requests
  the blocks of its first scans at plan time. Each scan pass has its own read-ahead
  window that grows from 1 MiB to the limit that `SPARKLES_READ_WINDOW_KB` sets. A warm
  repeat of a query makes no read hint.
* Index joins decode the columns they need on a separate pool, and optional warmup
  observes cancellation and deadlines. Each step has its own `Optimizations` flag.
* A point lookup in index format 3 decodes the frames of 2,048 rows that can hold its
  keys. An index join whose keys in one block span more than a quarter of the block's
  frames reads the block whole at its first cold frame, so a large join does not read
  frames one by one before it reads their block again.
* `sparkles serve --prewarm` records, at shutdown and every `--prewarm-interval` seconds,
  the decoded blocks that each persistent dataset's block cache holds
  and the pages of its vocabulary and frame directories that are in the page cache. The
  next start with `--prewarm` reads them in the background once the server is ready, at
  a lower CPU priority than requests. The list is `prewarm.json` in the database
  directory. A list for another generation is ignored, and a list of an unknown version
  is refused with a warning while the dataset serves as usual. Recording takes page
  residency from the page cache, so a list written soon after a load holds every page
  the load left there, up to 4 GiB. On forge at 10.5M, with the list of a warm pass, cold
  `star-lookup` takes 3.3 ms instead of 11.4 ms and `star-join` 33 ms instead of 78 ms,
  after a background read of 15 and 71 ms. The interval is 300 seconds by default.
* Before `sparkles serve` reports ready, it runs one query that reads no data on a scratch
  in-memory dataset, and a background thread maps the executable's code into memory. The
  first request after a start then finds the query path built and its code resident, and
  the served stores stay as cold as before. The mapped code adds about 100 MB of shared
  file pages to the resident set. `--no-warm-up` and `--no-code-prefault` turn the steps
  off, and the embedded library takes neither ([USAGE.md](../USAGE.md#memory-and-caches)).
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
* CSV and TSV tables map to triples ([C05](../specs/C05-tabular-imports.md),
  [USAGE.md](../USAGE.md#loading-csv-and-tsv)). `sparkles load`, `sparkles csv`,
  `POST /{ds}/upload` and the web UI's upload form accept them. A table maps with a
  default mapping in which a key column or the row number names each row, with W3C CSVW
  metadata, or with Tarql-style CONSTRUCT templates that the SPARQL engine runs in
  batches. CSVW covers datatypes and formats, null values, defaults, lists, virtual
  columns and RFC 6570 URI templates. Tables stream, compressed or not, and an invalid
  cell stops the load and names its row and column. The 270 RDF tests of the W3C CSVW
  test suite pass in minimal and standard mode.
* CSVW's standard mode adds the table group, table and row descriptions, and primary and
  foreign keys are checked across the tables of a load in the order their references
  need. A CONSTRUCT template can call Tarql's `tarql:expandPrefix` and
  `tarql:expandPrefixedName`, and with `--lookup` or `lookup=true` it reads the
  dataset's head to link rows to resources the dataset has.
* R2RML, RML and YARRRML mappings load CSV, TSV, JSON, JSON Lines, Parquet and SQLite
  sources ([F19](../specs/F19-mappings-and-virtual-graphs/README.md),
  [USAGE.md](../USAGE.md#mappings-r2rml-rml-and-yarrrml)). RML covers both the rml.io
  vocabulary and RML-Core. `sparkles map run`, `sparkles load --mapping` and uploads run
  them. Each source is read once as a stream, and values keep their natural types. A join
  over one SQLite connection runs as one SQL statement, and other joins use a hash table
  within the memory budget. Function maps call GREL, IDLab and SPARQL functions.
  `sparkles map` also checks a mapping, converts YARRRML to RML-Core Turtle and drafts an
  R2RML mapping from a SQLite database. The W3C R2RML test cases pass on SQLite apart
  from three, and the RML-Core cases with CSV and JSON sources apart from one. A join
  condition compares references or RML-Core expression maps.
* Mappings of every kind can be stored per dataset with versions and run again
  ([API.md](../API.md#stored-mappings)). A run in replace mode commits only the net change.
  The Graph Store protocol takes CSV bodies with a stored or default mapping, and the
  server reads SQLite databases through the named connections of its `connections`
  settings kind.

### Transactions and durability

* Reads use MVCC snapshots with a single writer, and a write-ahead log replays safely
  after a crash.
* The main store, branch stores and branch set share the dataset root's exclusive
  process lock on `sparkles.lock`. A retained branch store keeps that lock through
  resource destruction after main closes, including when the branch was removed or
  retired.
* A commit is acknowledged once its WAL record and any new vocabulary entries are
  synchronized. The two writes are issued in parallel, and the update vocabulary is
  written in checksummed chunks, so a torn tail is detected and trimmed at open.
* Group commit shares one synchronization among concurrent writers and is on by default
  for persistent stores ([X09](../specs/X09-group-commit-by-default.md)).
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
* A fence's leader writes the records, new terms and annotation records of all its
  commits at once. When the records fit in the space the log has preallocated and in one
  write of at most 64 KiB, and the terms and the commit messages and digests fit the same
  way in the vocabulary and in `annotations.bin`, all three go through write-through
  handles, so the fence persists without a device cache flush on a drive with FUA
  ([results](../benchmarks/writes/RESULTS.md#written-through-annotation-records)).
  Otherwise the file that does not fit, such as the vocabulary of a fence that adds a
  long literal, is written with a buffered write and `fdatasync`. The next fence
  synchronizes while the last one publishes and applies its full-text changes.
* Group commit costs memory. A persistent dataset queues up to 64 commits or 4 MiB of
  log records and staged full-text documents, plus the speculative snapshots of the
  commits in flight. The 4 MiB is an admission threshold, and a single commit larger than
  it drains the queue and is queued alone, so the queue then holds that commit's size.
  The queued records wait on the heap until their fence writes them. On an indexed
  10.5M-triple dataset with sixteen writers, that state raised peak resident memory by
  15 to 21 MiB.
* `/$/metrics` reports the synchronization time, the commits per fence, the wait, and why
  commits fell back or failed. `/$/stats/{ds}` says whether the dataset groups commits.
* A WAL written before logs had a header record is framed strictly from the offset where
  the current release first opened it for writing. The offset is recorded in the
  generation's `commit.json` once the log, its vocabulary and its annotations are durable,
  so a torn modern record after it is never read as a legacy one.
* A data directory belongs on a local file system. Sparkles does not support NFS for a
  data directory, because the process lock is an advisory file lock and the index files
  are memory-mapped, and neither is reliable across clients of a network file system.
  Every open, including one that only reads, creates and writes `sparkles.lock` inside
  the directory, and the lock file cannot be placed elsewhere. A read-only mount of a
  copy therefore cannot be opened either. A copy can be served from a local directory
  after a backup or a clone is restored there.

### Compaction, clones and integrity

* Compaction writes a new generation, `gen-NNNN`, and publishes it by an atomic switch of
  `CURRENT`. The build reads a snapshot while writes continue, the commits made meanwhile
  are carried into the new generation's log, and the writer lock is held only for the
  switch. After a power loss during a compaction or bulk load, the next open deletes an
  unfinished build directory, sets any other directory above `CURRENT` aside under a new
  name, and refuses a directory that holds generations but no `CURRENT`. `sparkles check`
  reports such a leftover as a warning.
* Automatic compaction runs on a server ([C13](../specs/C13-automatic-compaction.md),
  [API.md](../API.md#automatic-compaction)). It starts when a dataset's delta reaches a share
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
  in-memory dataset, and waits for one of the `--max-clones` slots. A dataset whose
  configuration history is activated cannot be cloned, which is roadmap item 764.
* `/$/backup` and dumps write N-Quads, compressed with zstd by default or with gzip, xz,
  bzip2, brotli or LZ4. Request and response bodies can be compressed with `zstd`, `br`
  or `gzip`. Incremental backup repositories are described under [Operations](../operations/README.md#backups).
* `sparkles check` and `sparkles::check` run a read-only integrity check that is safe
  beside a running server ([USAGE.md](../USAGE.md#checking-a-database)). It checks the
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
  ([API.md](../API.md#write-previews)). The response gives the commit it would create, net
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

The Rust library provides `Store::snapshot_at_limited` and
`Store::snapshot_at_with_branch_limited` for request-scoped history preparation.
`Dataset::history()` exposes matching trusted methods. They accept a memory cap
alongside `HistoryOptions`, preserve exact inherited origins, and check cancellation
and deadlines while preparing live, cached or cold states. The cap cannot exceed the
store's history capacity. Cold origin reads do not recover or rewrite another Store.

These trusted handles return a snapshot for subsequent use. They do not prepare access
masks or query guards, and their cap does not cover a later query. Controlled permission
preparation and guarded Dataset, C25 and U02 adoption remain under item 395. Existing
HTTP and query entry points retain their documented limits.

A branch-time read inherits a parent state only strictly before the fork timestamp.
If pruning removes the metadata needed to resolve its own state, the read returns
`HistoryGone` with the original time selector and no invented commit sequence.

### Diffs, change feed and history queries

* `GET /{ds}/diff`, `sparkles diff` and `Store::diff` compare any two readable commits
  ([API.md](../API.md#diffs-between-commits)). A diff comes as JSON counts and quads, as
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
  memory budget ([API.md](../API.md#comparing-a-query-between-two-states)).
* `GET /{ds}/changes` and `Store::changes` serve a change feed of the commits after a given
  one, as JSON or one RDF Patch per commit, in pages bounded by commits and quads. It
  resumes from any readable commit, long-polls with `wait=`, and streams server-sent
  events that resume with `Last-Event-ID`.
* History queries search the changes across commits ([API.md](../API.md#history-queries)).
  `SERVICE hist:changes` in SPARQL, `GET /{ds}/history`, `sparkles history` and
  `Store::history_changes` return each added or removed quad with its commit's number,
  time, kind, author and message, and write the triple pattern as an RDF 1.2 reifier. The
  pattern's terms and the commit range can come from the rest of the join group.
  HTTP, CLI and library results report unrecorded commits within that range, including
  the covered portion of a gap that extends beyond the requested window.
  HTTP history and diff term filters accept RDF 1.2 triple terms, including nested
  triple terms with integer, decimal or double objects. Numeric lexical values and
  datatypes are preserved when matching recorded quads. Malformed terms return 400.
* History queries read a per-dataset change log that outlives compactions. It is written
  off the commit path, indexed by subject, predicate, object and graph, recovered from the
  write-ahead log after a crash, and trimmed by commit count, age or size. Diffs and the
  change feed read it where the retained generations no longer can. Graph views and
  protections of triples apply.

### Branches and merges

Branches work like git branches on the quad store
([F09](../specs/F09-branches-and-merges.md), [API.md](../API.md#branches-and-merges)).

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
* Direct Rust `Store` and `Dataset` calls to delete or rename a branch, change its
  note or read its information return `conflict` when they would wait for a write
  transaction held by the same thread. Refusal leaves the names, metadata and
  transaction unchanged. Calls from other threads wait for the transaction to finish. Deleting an unrelated branch can postpone collection of old
  generations in the store whose transaction is open until its next compaction or reopen.
  Python's named branch get, update and delete and JVM note changes use their existing
  caller ownership guards. Further worker-mediated branch dependencies remain under
  roadmap item 754. Python `branches.update(name, note=None)` currently leaves the note
  unchanged. Its explicit clearing behavior is roadmap item 755.

### RDF Patch

* `POST` and `PATCH /{ds}/patch`, a patch sent to `/{ds}`, `sparkles patch`,
  `Store::apply_patch` and the Python `Dataset.apply_patch` apply a patch in text or
  binary form as one commit of kind `patch`, as Fuseki's `patch` operation does
  ([F10](../specs/F10-replication.md), [API.md](../API.md#applying-rdf-patch)).
* `TA` aborts the whole patch, and `PA` and `PD` change the prefixes. A `prev` header that
  names a commit of the dataset applies the patch only while that commit is the head.
  Write-time validation, quotas, graph-limited grants, dry runs and receipts apply as to
  other writes.

## Known gaps

These limits name the roadmap items that address them. Current priorities and
dependencies are recorded in the [central roadmap](../ROADMAP.md).

### Storage and execution

* The largest measured dataset is English DBpedia, 1.24 billion triples on one machine.
  Nothing larger, such as Wikidata, has been tried. Roadmap item 1.
* FSST vocabulary compression is off by default. With it, a DBpedia export takes 1.05
  times as long as with front coding, and warm queries that decode many strings run 4% to
  12% slower. Single-term lookups are the follow-up in roadmap item 160.
* Vocabulary codec and predicate-pattern choices have no dataset-local layered settings.
  Roadmap item 343 adds settings for subsequent builds and compactions while retaining
  each existing generation's recorded format.
* The first query that arrives about 2 s after a start is slower than one that arrives at
  once, because the server's idle threads and cores wake up for it. Roadmap item 572.
* Cold `export-1m` at 1.01B takes 1,293 ms in index format 3, 4% above the earlier format 2
  measurement, and 17 times as long with `SPARKLES_IO_HINTS=off`. Roadmap item 573.
* Warm point lookups run 1.6% to 2.5% more instructions per request for the index join's
  check of cold segments. Roadmap item 516.
* The prewarm restores the block cache and the pages of the generation's files. It does
  not restore the result cache, and it reads data that the next queries may never touch.
  Its list holds at most 4 GiB of pages.
* Group commit on datasets with a full-text, spatial or embedding index meets its gates
  ([X09](../specs/X09-group-commit-by-default.md#phase-4)). A serial commit pays 0.1% to
  0.3% more instructions for the group's admission checks, and spatial readers at 1.05M
  under mixed load measured a p50 ratio of 1.028, both within their gates, which is
  item 448.
* Branch stores share their dataset's quota, which counts the zeros preallocated after
  each store's log and delta vocabulary as those of the store written last. With several
  branches written, the zeros of the others count as data. Roadmap item 560.
* A clone refuses a dataset whose configuration history is activated, because the
  clone would keep the registry's history root without the evidence-history files that
  it names. A backup and a restore that keeps the identity copy such a dataset. Roadmap
  item 764.
* The drive of the benchmark hosts alternates between a fast state, in which a
  write-through write takes about 0.2 ms, and a slow state, in which every write-through
  write and every cache flush takes about 1.5 ms. Write-through fences are faster in both
  states, and their gain over flushing fences is smaller in the slow state
  ([results](../benchmarks/writes/RESULTS.md#fences-in-each-drive-state)).
* Automatic execution streams eligible large scans, groupings and ASK joins. Other
  plans remain eager, including an EXISTS evaluated once per outer key when its cursor
  is slower. Without spill, sorts without LIMIT collect their input, and DESCRIBE stays
  collected. The UI, MCP and Python's collected query API collect results, while explicit
  cursors and the JVM's automatic execution can pull SELECT batches. Roadmap item 7.
* Disk spill is on by default. A DISTINCT above an ORDER BY that cannot move below the
  sort, or above a ranking, keeps its whole set, which is item 497. A grouping's
  single group can outgrow the budget, and eager operators still fail at their budget. A
  spilled join below an operator that relies on its order, such as an ORDER BY, a
  DISTINCT or a GROUP_CONCAT, writes its output once more to merge it, which is item
  496. Automatic execution keeps a DISTINCT over partly ordered input eager though
  its cursor is faster, which is item 498.
* Incremental table views and SSE live queries are available. Graph views, per-fact
  provenance and commit-time maintenance remain planned under item 17. Live operator
  profiling and running-query monitoring remain item 27.
* A query whose plan would nest deeper than 4000 levels is refused, so a basic graph
  pattern holds at most 1998 triple patterns ([nesting limits](../API.md#nesting-limits)).
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

## Configuration and usage

The following guides describe configuration and supported UI, command-line, HTTP
or library workflows. Exact HTTP contracts remain in the API reference.

* Read [Command-line tools](../USAGE.md#command-line-tools).
* Read [Loading CSV and TSV](../USAGE.md#loading-csv-and-tsv).
* Read [Mappings (R2RML, RML and YARRRML)](../USAGE.md#mappings-r2rml-rml-and-yarrrml).
* Read [Checking a database](../USAGE.md#checking-a-database).
* Read [The I/O profile](../API.md#the-io-profile).
* Read [Stored mappings](../API.md#stored-mappings).
* Read [Automatic compaction](../API.md#automatic-compaction).
* Read [Write previews](../API.md#write-previews).
* Read [Diffs between commits](../API.md#diffs-between-commits).
* Read [Comparing a query between two states](../API.md#comparing-a-query-between-two-states).

## Roadmap

The remaining work described above refers to [item 1](../ROADMAP.md), [item 4](../ROADMAP.md), [item 7](../ROADMAP.md), [item 17](../ROADMAP.md), [item 23](../ROADMAP.md), [item 27](../ROADMAP.md), [item 52](../ROADMAP.md), [item 66](../ROADMAP.md), [item 74](../ROADMAP.md), [item 84](../ROADMAP.md), [item 88](../ROADMAP.md), [item 160](../ROADMAP.md), [item 343](../ROADMAP.md), [item 448](../ROADMAP.md), [item 496](../ROADMAP.md), [item 497](../ROADMAP.md), [item 498](../ROADMAP.md), [item 516](../ROADMAP.md), [item 560](../ROADMAP.md).
The central roadmap owns priority and dependencies. Its
[completion paths](../ROADMAP.md#completion-paths-for-partially-complete-specs)
link partially complete specs to their remaining work and acceptance criteria.
