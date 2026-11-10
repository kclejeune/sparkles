# Querying capabilities

This page describes the available capabilities and their current limits. The
[capability matrix](../FEATURES.md#capability-status) records delivery status within
each stated scope.

## Standards conformance

Sparkles runs the W3C and community test suites in its own harness. `mise run test:w3c`,
`test:shacl` and `test:shex` run them, as [DEVELOPMENT.md](../DEVELOPMENT.md) describes.

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

The query evaluation tests of these SPARQL, CDT, property function and GeoSPARQL suites
also pass with `execution=streaming`, through the cursors that a streamed query uses.
`mise run test:w3c:streaming` runs that pass alone and reports, for each suite, the
queries that fall back to eager execution for an operator the cursors do not stream.

The SPARQL parser is a vendored, patched `spargebra`
([`vendor/spargebra/PATCHED.md`](../../vendor/spargebra/PATCHED.md)).


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
* Pure BIND expressions that read one variable can reuse their results across cursor
  batches. The query owns and budgets the cache, preserving RDF term identity and
  expression errors. Admission uses observed repetition and bounded snapshot statistics.
  Sparse reuse can remain rowwise, tracked in [item 293](../ROADMAP.md). Volatile expressions
  and application callbacks run per row.
* The function library has the SPARQL 1.1 built-ins, the XSD casts, including the
  derived integer types with their ranges, `anyURI` and the Gregorian types, and ARQ's
  `fn:`, `afn:` and `math:` functions ([API.md](../API.md#extension-functions-and-aggregates)).
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
  ([API.md](../API.md#arq-syntax-extensions), [G06](../specs/G06-arq-query-extensions.md)).
  `LATERAL` substitutes each outer solution into its group and runs as a join when
  substitution cannot change the result. Path ranges such as `p{n,m}` count each way
  through the graph, as ARQ does. CONSTRUCT templates with `GRAPH` blocks return quads.
  `LET`, `SEMIJOIN`, `ANTIJOIN` and the paths `distinct(…)`, `multi(…)` and `:p^:q` follow
  ARQ's evaluation, and Jena 6.2.0's answers check them.
* Jena's composite datatypes `cdt:List` and `cdt:Map` follow Jena 5 and 6, with their
  literals, equality and order, the 16 `cdt:` functions, the `FOLD` aggregate and the
  `UNFOLD` operator. A blank node label inside a literal names the same node as that label
  in the literal's file or query.
* ARQ's property function library is supported ([API.md](../API.md#arqs-property-functions)).
  It has `list:member`, `list:index` and `list:length` over RDF collections, `apf:container`,
  `apf:bag`, `apf:seq`, `apf:alt` and `rdfs:member` over RDF containers, and `apf:strSplit`,
  `apf:concat`, `apf:str`, `apf:splitIRI`, `apf:assign`, `apf:bnode` and
  `apf:versionARQ`. A call reads the variables that earlier patterns bind, as in ARQ.
  `rdfs:member` acts as an ordinary triple pattern while the store holds no container.
* DESCRIBE is configurable ([API.md](../API.md#describe)). The default is the W3C concise
  bounded description, read from each graph as Jena's default handler reads them and
  extended with the reifiers that annotate the described triples. `scbd` adds the triples that point at
  the resource, and `outgoing` keeps only the resource's own triples. Options add labels
  of linked IRIs and limit the size and depth. A dataset keeps its setting through
  `/$/describe/{ds}` and `sparkles describe-settings`, and a request can choose another.
  A streamed DESCRIBE describes the new resources of each batch of solutions as the
  batch arrives, so its first triples come before the WHERE clause has finished. The
  resources, triples and nodes it has seen stay in memory under the query's budget, and
  the labels of linked IRIs come last. When `maxTriples` cuts a streamed description
  short, the triples kept can differ from the eager ones, because the resources are
  described in the order the batches name them.

### Federated queries

* SERVICE, with SILENT, runs under an outbound network policy
  ([USAGE.md](../USAGE.md#outbound-requests-service-and-load)). A server allows only public
  destinations by default, while the local `query` and `update` commands can also reach
  private ones. The policy has allowlists, DNS pinning, checked redirects, timeouts and a
  response ceiling.
* Jena's service enhancer options work on SERVICE
  ([API.md](../API.md#service-options-loop-bulk-and-cache),
  [G10](../specs/G10-service-enhancer.md)). `loop:` runs a SERVICE once per solution of the
  patterns before it. `bulk+n:` sends `n` solutions per request and retries one at a time
  when an endpoint cuts a response short. `cache:` keeps each solution's remote result in a
  per-dataset cache with a memory budget, separate for each caller, with `cache+clear` and
  `cache+off`. `urn:x-arq:self` reads the dataset itself.

### Planning and execution

* The planner estimates joins from statistics that a bulk load or compaction counts:
  the characteristic sets of subjects and of nodes over both directions, each with a
  remainder for the nodes outside the 32,768 sets kept, and key probes of small inputs.
  Stars, joins on an object, the links of chains and joins on several keys are estimated
  from them, and other joins from distinct values
  ([internals](../internals/OPTIMIZATIONS.md#further-scan-and-filter-optimizations)).
  Statistics written before node sets existed keep the earlier estimates until the next
  bulk load or full compaction.
* The planner can move a small basic graph pattern into the branches of a pure UNION, so
  that each branch uses its bindings to choose a join order. It keeps duplicates and
  FILTER scope, and it keeps the ordinary plan when that plan is estimated to be cheaper.
  This avoids enumerating unrelated coauthor walks in SP²Bench `q8` and `q12b`.
* A VALUES table joined with a UNION moves into each branch, so that each branch looks up
  the table's rows by index instead of reading its whole pattern. The table also moves
  into a `GRAPH` that holds a UNION and through a BIND or FILTER whose expression reads
  none of its variables or only variables that the pattern under it always binds. A table
  stays outside a BIND or FILTER that reads a variable an OPTIONAL, a MINUS or one UNION
  branch may leave unbound, outside a `GRAPH ?g` when it binds `?g`, and outside a FILTER
  that reads one of its UNDEF cells. It also stays outside a BIND or FILTER that calls
  `RAND`, `UUID`, `STRUUID`, `BNODE` or a custom function, and outside a UNION that holds
  a SERVICE, a LATERAL or a property function call. At most 10,000 rows move, and at most
  20,000 over all the copies made for the branches and for the named graphs of a
  `GRAPH ?g`.
* A LIMIT without OFFSET passes through projections and BINDs into a SERVICE that reads
  no input, whose remote query is then sent with that limit. It also reaches a full-text
  search with a constant query string, which keeps only that many of its best hits, so
  the LIMIT is answered even when the search matches more than `maxHits`.
* An EXISTS or NOT EXISTS that is a FILTER conjunct, a BIND expression or an OPTIONAL
  condition is answered from a key set of its pattern, built once per query, when the
  pattern costs less than evaluating it for each distinct outer key. A row is evaluated
  on its own when it binds a variable that a FILTER inside the pattern reads and the
  pattern under that FILTER may leave unbound. An EXISTS inside a larger expression, such
  as IF or OR, is evaluated once per distinct outer key.
* Eager ASK execution can visit a pure UNION branch with lower estimated work before
  its first answer. On an unchanged snapshot without a total produced-row cap, a
  bounded whitelist of the whole ASK lets pure input operators request only the rows
  the limit needs. Expression calls, EXISTS and external effects retain their ordinary
  order and demand policy. Reordering also requires conservative output bounds that fit its
  row and memory budgets. Joins through index probes retain their order because their
  duplicate-key fanout lacks such a bound. A retrying operator retains the ordinary
  prefix when its input would restart a full join build and conservative bounds prove
  that the larger request fits its row and memory limits. Explicit streaming keeps its policy.
* An OPTIONAL whose sides share no variable, but whose condition contains `?a = ?b` with
  one variable from each side, pairs its rows through that equality instead of forming
  their product ([internals](../internals/OPTIMIZATIONS.md)). SP²Bench `q6` at 10M tests 7.4
  million candidate pairs where the product had 1.6 × 10¹² rows.
* Groups linked only by a FILTER equality also join through candidate keys and test the
  full condition on those pairs. Large ASK queries read the inputs alternately and stop
  at the first qualifying pair. They retain prefixes of both inputs while both can
  produce rows, then discard obsolete history when either input ends.
* A triple pattern with one free variable can bind it during planning when a bounded
  snapshot read proves that the pattern has exactly one row. This makes other patterns
  and numeric FILTER bounds constant while preserving the result variables. Volatile
  filters, application callbacks and algebra boundaries retain the ordinary plan.
* An index join can use a bound second variable to skip incompatible rows within an
  already-read key range. It preserves input order, duplicate solutions and unbound
  matches, and retains all compatibility checks when the secondary ordering cannot be used.
* Queries can run eagerly or as streaming cursors
  ([X05](../specs/X05-streaming-query-execution.md)). Automatic execution, the default over
  HTTP, in the CLI, in Node and in the JVM library, streams scans expected to return at
  least a thousand rows, groupings of an estimated 20,000 input rows or more, and bounded
  ASK products whose eager candidates would exceed a budget, and large equality-keyed
  ASK joins. Other plans run eagerly, and
  `execution=eager` or `execution=streaming` overrides the choice. Both modes join
  through the same hash, keyed and merge kernels, so an OPTIONAL over two inputs sorted
  on their shared variable merges in both when `merge_left_join` is on and hashes in
  both when it is off, an OPTIONAL condition is tested once over each chunk of candidate
  pairs rather than over the whole product, and EXPLAIN names the algorithm that ran in
  either mode.
* Rust, Python and Node have snapshot-owning SELECT and graph cursors, and the JVM
  library pulls SELECT results. Result writers use bounded buffers with backpressure and
  stop when the client disconnects.
* Native SELECT output and standard JSON, XML, CSV and TSV solution responses of at
  most 65,536 rows check a complete decoded row before writing its cells. Row storage
  and decoded terms retain query memory credit while they are written. A required
  memory refusal stops output with its original local or shared-pool budget error and
  preserves the count of earlier complete rows. Native output holds one decoded row
  rather than one cell, so a tight budget can refuse the additional live storage.
  Ownership across public results, native buffers, reasoners and views remains in
  [roadmap item 675](../ROADMAP.md).
* Streaming joins run batch by batch with column kernels. ORDER BY with LIMIT keeps only
  its best rows in a bounded heap. A cursor filter over an index scan filters whole blocks
  in parallel, and a range top-k ranks its range a part at a time. FOLD and registered
  aggregates read their input in group-key order, through a stable sort on the keys that
  can spill when the input does not arrive that way. They hold the rows of the group
  they have not finished and at most a batch more. A full sort and the
  shapes the cursor does not support materialize within the budget and fail explicitly
  when they exceed it.
* Property path closures run within the query's memory budget and timeout in both
  executors. The visited set, the frontier and the edge relation of a sequence or
  alternative path count against the budget, and the walk checks the deadline while it
  scans and between index lookups. A path with two unbound ends reads its start nodes a
  few thousand at a time from the indexes, and under an `ASK` or a `LIMIT` it stops once
  it has the rows it needs. A closure over an alternative of predicates and inverse
  predicates, such as `(ex:p|^ex:q)*`, walks from a bound or constant end by index
  lookups. With two unbound ends, such an alternative still reads its whole edge relation
  first, and the full closure of a large graph exceeds the budget or the timeout with the
  usual error rather than finishing.
* Disk spill lets large queries finish within their memory budget
  ([X08](../specs/X08-disk-spill-and-server-memory.md)). `serve` spills by default,
  `--spill off` turns it off, and `--spill encrypted` encrypts the files. An embedded
  store spills when `QueryOptions::spill` holds a spill directory.
  * A streamed sort without LIMIT writes sorted runs and merges them. A grouping or a
    DISTINCT keeps what fits and handles the rest in later passes over hash-partitioned
    files.
  * A hash join whose build input outgrows its share partitions both inputs by key, up to
    three levels, and joins a heavy key as a block nested loop. A merge join spools a long
    run of equal keys to a file. A spilled join restores its probe input's order only for
    an operator above that relies on it, which a grouping of COUNT, SUM, AVG, MIN and MAX
    does not.
  * A DISTINCT over input ordered on some of its columns holds the rows of one run of the
    first order column at a time, with or without spill.
  * `--spill encrypted` seals every frame with AES-256-GCM under a per-file key held only
    in memory. Spill files are versioned, CRC-checked, unnamed while written and removed
    with their query or process.
  * Disk is reserved per query with `--query-spill-mb` or `spill-mb=`, and for the server
    with `--spill-max-mb`. With spill on, an eager join whose hash table does not fit the
    budget partitions its two inputs to disk through the cursor's join and gives the same
    answer, automatic execution streams a query that eager execution would hold in more
    than half its budget, and it reruns an eager query that exceeds its budget as a
    cursor unless it calls SERVICE.
  * With `--spill on` or `encrypted`, the server refuses to start with an unusable spill
    directory, one on tmpfs or ramfs, or one that other users can write. With the default,
    it logs a warning and runs without spill.

### Explaining queries

* Every node of a returned plan has an id and its path from the root, and the estimated,
  eager and streamed plans have the same shape
  ([USAGE.md](../USAGE.md#explaining-a-query), [API.md](../API.md#explaining-a-query)). Nodes
  say when they did not run and why, when a `LIMIT` stopped them, how often they reran,
  which filters a scan applied itself, and when a result came from the cache.
* Finite HTTP response bodies have a separate 60-second idle limit, configured with
  `serve --stream-idle-timeout`. It runs while application output or its final end is
  ready but unread, and pauses while the engine has nothing ready. An optional
  `--stream-transfer-timeout` bounds the whole transfer, even when it keeps progressing.
  These limits observe uncompressed application chunks rather than remote receipt.
  Expiry aborts the transfer and requests cancellation. Actual producer owners remain
  until work stops, and only a successful transfer is a complete answer. The settings
  also cover finite Graph Store, diff and changes bodies. See [server settings](../USAGE.md).
* A query that a budget or its timeout stopped keeps its plan in the error body of the
  `application/x-sparkles+json` format.
* `POST /{ds}/sparql/explain` explains an estimated plan, a read-only run or a plan the
  client gives. Its notes cover the slowest operator, misestimates, blowups, late filters,
  large sorts, open paths, skipped nodes and the budget, together with a description of
  what the query asks, all computed without a model. A configured `explain` model role can
  rewrite them as prose, which the server checks against the plan. MCP's `explain_query`
  takes `profile`, `notes` and `useServerModel`, which needs the `serverModels` grant.

### Incremental views and live queries

* Incremental views and live queries ([API.md](../API.md#incremental-views), spec [F13](../specs/F13-incremental-views-and-provenance.md) Phase 1). A `table` view keeps a SELECT or CONSTRUCT query's result current after every commit, through a circuit of Z-set operators compiled from the query that joins each commit's net changes against the store's own indexes. Basic graph patterns, `GRAPH`, `FILTER`, `BIND`, `UNION`, `OPTIONAL`, `MINUS`, `EXISTS`, `VALUES`, `DISTINCT`, subqueries, `GROUP BY` with `COUNT`, `SUM` and `AVG`, and property paths are maintained, and other operators are evaluated again as `recompute` nodes. A commit that misses a view's patterns skips it. Queries read a view with `SERVICE view:read`, and `GET /{ds}/live` streams a query's result and then its changes as server-sent events, resuming after a commit with `Last-Event-ID`. Views and live queries with the same query and access share one circuit. On a dataset with RDFS on read or materialized inferences in the default graph, a view or live query evaluates its whole query again with them at each commit that touches it, so it answers as the one-off query does, and the view's state reports `evaluation: recompute`. Turning either setting on or off switches existing views and live queries between the two modes without detaching them. View state is checkpointed and recovered from the change log, views read as their principal, and `Dataset::views`, `Dataset::live`, `sparkles views`, `sparkles query --live` and Python's `Dataset.views` and `Dataset.live` expose them.
* Property paths in views ([F13 §4.3](../specs/F13-incremental-views-and-provenance.md#fixed-point-operator)). A path with `+` or `*` over a step of IRIs, inverses, sequences and alternatives is maintained by a fixed-point operator that keeps each reachable pair with the one derivation that first reached it. After a commit only the walks that reach a changed step are followed again. A deleted step removes exactly the pairs that no other path still reaches, so cycles and self-loops need no recomputation, and the zero-length rows of `*` follow the active graph's nodes without state. Paths with constant ends, inverses, `GRAPH` with a constant or a variable, and paths inside `EXISTS`, `NOT EXISTS`, `MINUS` and `OPTIONAL` are covered. The operator's state takes about 40 bytes for each reachable pair on top of the view's rows, and a view with a path checkpoints in `VIEW_STATE` format 2. With a provenance setting a path row reports one real witness, the quads of the path that reached it first. Paths with `?`, negated property sets, nested closures or a closure inside a sequence are still `recompute` nodes ([measurements](../benchmarks/writes/RESULTS.md#property-path-views)).
* Internal circuits ([F13 §3.6](../specs/F13-incremental-views-and-provenance.md#internal-circuits)). Server code, or an embedding application through `Dataset::attach_internal`, can attach a circuit that no user defined. It keeps a query's rows, or counts the changed facts per key in each commit, so that a fact added and removed later counts 2, a replacement within one commit counts 2 and a deletion counts 1, as the change feed lists them. The counts after any commit they cover are exact, and a commit whose changes are not known is reported as unknown. An internal circuit has no definition file, no entry in the list of views, no rows in `SERVICE view:read` and no route, binding or MCP tool, and it is checkpointed under `circuits/` and recovered from the change log like a view.
* Provenance annotations ([F13 §5](../specs/F13-incremental-views-and-provenance.md#provenance-annotations)). A circuit that keeps rows can annotate each row with `count`, `lineage` or `why`, the images of the row's provenance polynomial over the input quads. `count` is the number of derivations. `lineage` lists every input quad, or condition, that some derivation uses. `why` keeps the row's witnesses, each a set of quads that derives it, bounded by `maxWitnesses` (8 by default) and reported minimal and smallest first. Joins multiply annotations, and unions, projections, `DISTINCT` and groups add them. A row that `MINUS`, `NOT EXISTS` or an unmatched `OPTIONAL` keeps has a negative condition in its witnesses, which names the operator and the bindings that must have no match. Quads are named by a 128-bit `FactKey` over their terms' vocabulary keys, which survives compaction and resolves back to the quad through the circuit's snapshot. Deletions are exact, since every coefficient is a signed count. A `why` row whose witnesses did not fit in the bound is reported as truncated. Its witnesses are then real but may not be all. The row is exact again once the extra derivations are removed, or after a rebuild when its supports fit the bound. Rows of a `recompute` node, and triples of a CONSTRUCT template with blank nodes, are truncated with no known witnesses. Internal circuits take the setting through `InternalSpec::with_provenance` and read the rows with `annotated_rows()`. An annotated pass costs little more than a plain one, but `lineage` can make a state up to 80 times larger and `why` up to 8 times, and an annotated build matches its patterns with index probes, which takes seconds for a large join ([measurements](../benchmarks/writes/RESULTS.md#provenance-annotations)). Views do not take it, and `view:witness` and `view:why` remain planned under roadmap item 429.


## Command line

One `sparkles` binary carries the server, Jena-style database commands and Jena's file
tools.

* The database commands work on the database directory. They are `load`, `query`,
  `update`, `patch`, `dump`, `compact`, `compaction`, `clone`, `stats`, `log`, `check`,
  `diff`, `snapshot`, `backup`, `repo`, `infer`, `shacl`, `shex`, `validation`, `schema`,
  `queries`, `graphql`, `text-index`, `vector`, `geo-index`, `quota`, `describe-settings`
  and `vocab-index`. `load`, `query`, `update`, `patch`, `vector`, `compaction`, `quota`
  and `describe-settings` also work on a remote server with `--server`.
* Jena's file tools are built in ([G05](../specs/G05-command-line-tools.md),
  [USAGE.md](../USAGE.md#file-tools)). `convert`, alias `riot`, parses, validates, counts and
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
  ([USAGE.md](../USAGE.md#shell-completions-and-man-pages)).


## Formatter and editor support

The formatter and linter check their own output and serve the command line, the server,
the UI and editors.

* `sparkles-fmt` formats SPARQL queries and updates, Turtle, TriG, N-Triples, N-Quads,
  JSON-LD and SHACL Compact Syntax, through `sparkles fmt` and `POST /$/format` with the `fmt` cargo feature, on by
  default ([USAGE.md](../USAGE.md#formatting), [API.md](../API.md#formatting)). It keeps
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
* SHACL Compact Syntax keeps directives as written, separates shapes with one blank line,
  and puts one property on each line. A property's constraints wrap at `line-width`.
  `prune-prefixes` and `align-values` apply, and the sort keys do not. The shapes editor
  in the UI formats and lints SHACLC as well.
* `sparkles lint` checks SPARQL, Turtle, TriG, N-Triples, N-Quads, JSON-LD and SHACL
  Compact Syntax with nineteen rules ([X04](../specs/X04-linter.md),
  [USAGE.md](../USAGE.md#linting)). They cover undefined and unused prefixes, unused,
  single-use and unbound variables, cartesian products, `SELECT *` with grouping,
  ungrouped variables, FILTER scope and equality, IRIs with spaces, language tags,
  suspicious and redundant datatypes, and deprecated names. The term rules apply to every
  language, and JSON-LD adds two rules for context terms that nothing uses and keys that no
  context maps. In Turtle, TriG and JSON-LD shapes, the SPARQL of `sh:select`, `sh:ask`,
  `sh:construct` and `sh:update` literals is linted with the prefixes of `sh:declare` and
  the pre-bound variables of SHACL, and findings are placed inside the literal. Severities
  are set in `.sparklesfmt.toml` or with `--rule`, and `--fix` applies safe fixes after
  checking that the meaning is unchanged. The linter also runs as `POST /$/lint`, as the
  MCP `lint` tool, in the query editor and in the shapes editor.
* `sparkles lsp` is a language server for every language the formatter handles. It
  formats documents and ranges, reports syntax errors and lint findings, and offers safe
  fixes as quick fixes. [editors.md](../editors.md) has setups for Neovim, Helix, Emacs and
  VS Code.
* The UI's editors use a WebAssembly build of the formatter, `sparkles-fmt-wasm`, and fall
  back to `POST /$/format` when it is missing
  ([ui/README.md](../../ui/README.md#formatting-in-the-browser)). The Nix `sparkles` and
  `sparkles-ui` packages always include it.

## Known gaps

These limits name the roadmap items that address them. Current priorities and
dependencies are recorded in the [central roadmap](../ROADMAP.md).

### SPARQL and formats

* Sparkles lacks ARQ's JavaScript functions, `afn:context`, the Leviathan `lfn:` library
  the `JSON` query form, ARQ's `EXISTS { … }` group element and `AGG` with more than one
  argument. `afn:print` and `afn:execTime` print nothing, and `afn:collation` orders by
  code point instead of by locale. Roadmap item 69, a lower-priority extension outside the completion of Server and
  Fuseki integration.
* The linter reads a document whole, so a file over `--max-bytes` is refused even in a line
  format. It does not lint SHACL query literals in N-Triples and N-Quads or JSON-LD queries
  under `@index` maps, and a JSON-LD document over 8 MiB gets no query findings and no
  fixes. Each JSON-LD query and fix is checked by expanding the document again, within a
  budget of four times the document, so a large document with many queries lints only
  some of them. It skips an embedded query whose holder is ambiguous, such as the
  same query text on two blank nodes, rather than risk another node's findings. Roadmap
  item 444.
* Sparkles does not evaluate `shortest(…)` paths, which ARQ does not evaluate either.
  Roadmap item 51.
* Jena's service enhancer has no slice-aware cache and no `se:cacheRm` or `se:cacheLs`
  functions. Roadmap item 38.
* There is no openCypher frontend and no JSON query language. Roadmap items 20 and 72.
* Mappings read no PostgreSQL or DuckDB database and no Parquet file in an object store.
  Roadmap items 230 and 237.
* Function maps call 20 of SPARQL's functions. Roadmap item 236.
* Stored mapping runs are not tasks, and the web UI has no Mappings page.
  Roadmap items 233 and 234.
* The command line and the upload parameters read CSVW metadata and cells strictly,
  where the library can also warn and use the default. Roadmap item 450.
* A template lookup that binds a blank node of the dataset writes a new node, and a JSON
  number with a fraction or an exponent loses its source form. Roadmap items 238 and 239.
* Tables and databases are imported, not queried in place. Roadmap item 43.
* Joins on a link whose far end is restricted, the object of `?x a ?c` and large inputs
  that no statistic covers are estimated from distinct values. Roadmap items 454 and
  697 to 699.

* Incremental views are `table` views on `main`, maintained after the commit. There are
  no `graph` views, no maintenance inside the commit, no WebSocket or MCP access, no Node
  or JVM API, no UI, no views per branch, no ordered windows and no provenance per fact.
  A stored definition that asks for one of these, including `onFailure: reject` and
  `declassify: true`, is listed as failed with `unsupported-option` and does not run,
  while the dataset's other views keep running. Roadmap item 17.
* A view maintains a property path with `+` or `*` only when its step is made of IRIs,
  inverses, sequences and alternatives, and recomputes paths with `?`, negated property
  sets, nested closures or a closure inside a sequence. Roadmap item 544. A path
  row reports one witness, not all of them, under a provenance setting. Roadmap item
  543. A path with two variable ends walks from every subject of its step, even
  when a joined pattern binds one end. Roadmap item 546.
* A zero-length `*` row with a constant end over the union default graph can stay stale
  for a reader who cannot read a named graph that gains its first quad or loses its last,
  until the next commit the reader sees. Roadmap item 545.
* Backups and persistent clones carry view definitions, which come back suspended until
  they are resumed. In-memory clones carry no views. Roadmap item 401.
* Views and live queries on a dataset with RDFS on read or materialized inferences
  evaluate the whole query again at each commit that touches them, rather than
  maintaining it incrementally. Roadmap item 221.
* A caller reads a view only with full access or the principal's exact access. Roadmap
  item 222.
* View passes run on one thread per dataset, and the live query limits count per
  dataset. Roadmap items 223 and 224.
* GraphRAG's automatic refresh still counts changed facts from the change feed rather
  than from an internal circuit. Roadmap item 12.
* Views and live queries never read the graphs that a disclosure guard protects. Roadmap
  item 227.

## Configuration and usage

The following guides describe configuration and supported UI, command-line, HTTP
or library workflows. Exact HTTP contracts remain in the API reference.

* Read [Outbound requests (SERVICE and LOAD)](../USAGE.md#outbound-requests-service-and-load).
* Read [Explaining a query](../USAGE.md#explaining-a-query).
* Read [File tools](../USAGE.md#file-tools).
* Read [Shell completions and man pages](../USAGE.md#shell-completions-and-man-pages).
* Read [Formatting](../USAGE.md#formatting).
* Read [Linting](../USAGE.md#linting).
* Read [Extension functions and aggregates](../API.md#extension-functions-and-aggregates).
* Read [ARQ syntax extensions](../API.md#arq-syntax-extensions).
* Read [ARQ's property functions](../API.md#arqs-property-functions).
* Read [DESCRIBE](../API.md#describe).

## Roadmap

The remaining work described above refers to [item 17](../ROADMAP.md), [item 20](../ROADMAP.md), [item 38](../ROADMAP.md), [item 43](../ROADMAP.md), [item 51](../ROADMAP.md), [item 69](../ROADMAP.md), [item 72](../ROADMAP.md), [item 221](../ROADMAP.md), [item 222](../ROADMAP.md), [item 223](../ROADMAP.md), [item 224](../ROADMAP.md), [item 227](../ROADMAP.md), [item 230](../ROADMAP.md), [item 231](../ROADMAP.md), [item 233](../ROADMAP.md), [item 234](../ROADMAP.md), [item 236](../ROADMAP.md), [item 237](../ROADMAP.md), [item 238](../ROADMAP.md), [item 239](../ROADMAP.md), [item 450](../ROADMAP.md), [item 454](../ROADMAP.md), [item 293](../ROADMAP.md), [item 401](../ROADMAP.md).
The central roadmap owns priority and dependencies. Its
[completion paths](../ROADMAP.md#completion-paths-for-partially-complete-specs)
link partially complete specs to their remaining work and acceptance criteria.
