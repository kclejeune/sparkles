# F14: Graph algorithms

> **Status:** implemented in part (Phases 1 and 2, and the embeddings of Phase 3)
>
> **Phases:** Phases 1 and 2 shipped, apart from the LDBC Graphalytics harness, the web
> UI and the JVM binding. Of Phase 3, `algo:fastRP` and `algo:node2vec` shipped, and
> their vectors reach F04 indexes through SPARQL Update or a write-back run. The
> openCypher procedures wait for the Cypher frontend of [F01](F01-cypher.md), which is
> not built.
>
> **User docs:** [API: Graph algorithms](../API.md#graph-algorithms) · [Usage: Graph algorithms](../USAGE.md#graph-algorithms) · [Features](../FEATURES.md#search)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This design was written from the public documentation of the Neo4j Graph Data Science
library (its algorithm pages, graph projections and execution modes), the public
documentation of Amazon Neptune Analytics' algorithms, the LDBC Graphalytics
specification and Iosup et al., "LDBC Graphalytics: A Benchmark for Large-Scale Graph
Analysis on Parallel and Distributed Platforms" (VLDB 2016), and the papers that define
each algorithm: Page, Brin, Motwani and Winograd, "The PageRank Citation Ranking:
Bringing Order to the Web" (Stanford technical report, 1999); Haveliwala,
"Topic-sensitive PageRank" (WWW 2002); Brandes, "A faster algorithm for betweenness
centrality" (Journal of Mathematical Sociology, 2001); Brandes and Pich, "Centrality
estimation in large networks" (International Journal of Bifurcation and Chaos, 2007);
Tarjan, "Depth-first search and linear graph algorithms" (SIAM Journal on Computing,
1972); Raghavan, Albert and Kumara, "Near linear time algorithm to detect community
structures in large-scale networks" (Physical Review E, 2007); Blondel, Guillaume,
Lambiotte and Lefebvre, "Fast unfolding of communities in large networks" (J. Stat.
Mech., 2008); Traag, Waltman and van Eck, "From Louvain to Leiden: guaranteeing
well-connected communities" (Scientific Reports, 2019); Batagelj and Zaversnik, "An O(m)
algorithm for cores decomposition of networks" (2003); Watts and Strogatz, "Collective
dynamics of 'small-world' networks" (Nature, 1998); Chen et al., "Fast and Accurate
Network Embeddings via Very Sparse Random Projection" (CIKM 2019); and Grover and
Leskovec, "node2vec: Scalable Feature Learning for Networks" (KDD 2016). Only public
documentation of Neo4j GDS was read, not its source. The call syntax follows path search
([F07](F07-path-search.md)), and the design builds on budgets
([C01](C01-observability-and-budgets.md)), the views of [C12](C12-graph-access-control.md)
and [C12b](C12b-triple-access-control.md), vector indexes ([F04](F04-vector-search.md)),
server tasks, the MCP server ([C11](C11-mcp-server.md)) and the embedding API of
[P06](P06-library-admin-api.md). GraphRAG ([C22](C22-graph-rag-community-summaries.md))
uses its Leiden communities.

## 1. Summary, goals, non-goals

Sparkles has no graph-algorithm library. A user who wants the most central people, the
communities of a citation graph or embeddings of nodes exports the data to NetworkX,
igraph or a property-graph database, and imports the results back. F07 added paths, and
its non-goals listed centrality, communities and components.

This spec adds a library of whole-graph algorithms that run inside the engine on a
projection of the dataset. A call is a local `SERVICE` with a block of parameters, as in
F07, so it needs no new grammar and its results join with the rest of the query.

```sparql
PREFIX algo: <urn:x-sparkles:algo#>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>
SELECT ?person ?rank WHERE {
  SERVICE algo:pageRank {
    [] algo:predicate foaf:knows ;
       algo:node ?person ; algo:score ?rank .
  }
}
ORDER BY DESC(?rank) LIMIT 20
```

**Goals**

1. The algorithms of §4: PageRank and personalized PageRank, betweenness (exact and
   sampled), weakly and strongly connected components, label propagation, Louvain and
   Leiden, node similarity by Jaccard and overlap, triangle counts and clustering
   coefficients, k-core, BFS depth and single-source shortest paths, FastRP and node2vec.
2. Projections defined by predicates with directions, by node classes, by weights on RDF
   1.2 reifiers, or by a nested graph pattern, with the same parameters for every
   algorithm.
3. Results as rows that join with the query, or written back as triples to a named graph
   in a commit, with a record of the run.
4. Embeddings written as `spk:vector` literals to a predicate that an F04 vector index
   covers, so that `spk:vectorSearch` finds similar nodes.
5. Budgets for memory, time and threads, cancellation, and parallel execution on a
   bounded pool.
6. Exact answers under access control: a projection never holds an edge the caller
   cannot see.
7. Results that validate against LDBC Graphalytics' reference outputs for its six
   algorithms, and published timings on its graphs.

**Non-goals**

- A graph catalog of mutable in-memory graphs, as GDS's `mutate` mode keeps. A projection
  belongs to one snapshot and is rebuilt from the store.
- Machine-learning pipelines: link prediction, node classification and training of graph
  neural networks.
- Distributed execution across machines.
- Algorithms on RDF lists or on literals as nodes. Literals are not nodes of a projection,
  except as endpoints of a nested pattern's edges when the pattern binds them.
- Incremental maintenance of algorithm results after each commit. [F13](F13-incremental-views-and-provenance.md)
  could maintain some of them later (§13).

## 2. User-visible behaviour

### 2.1 Syntax

All terms live in `PREFIX algo: <urn:x-sparkles:algo#>`. Each algorithm is a service IRI
of that namespace, such as `SERVICE algo:pageRank { … }`. The block holds triples whose
subject is one blank node and whose predicates are the parameters. A parameter that the
algorithm does not take is an error, as in F07.

**Projection parameters**, shared by every algorithm:

| Parameter | Value | Default | Meaning |
|---|---|---|---|
| `algo:predicate` | an IRI, or a list `(iri direction)`, repeatable | every predicate between resources except `rdf:type` (§11) | The predicates whose triples are edges, with an optional direction each, as in F07. |
| `algo:direction` | `algo:forward`, `algo:backward`, `algo:undirected` | per algorithm (§4) | How edges are read. `undirected` reads each triple both ways. |
| `algo:nodeClass` | an IRI, repeatable | none | Keep only nodes with one of these `rdf:type`s, and the edges between them. |
| `algo:weight` | an IRI | none | The property of an edge's reifier that holds its weight, as `path:weight` in F07. |
| `algo:defaultWeight` | a number | 1 | The weight of an edge without one. |
| `algo:aggregate` | `algo:none`, `algo:sum`, `algo:max`, `algo:count` | `algo:none` | How parallel edges between two nodes combine: kept apart, or merged into one edge. |
| `algo:start`, `algo:end`, `algo:edgeWeight` | variables | — | With a nested pattern in the block, the pattern's variables for an edge's ends and weight. |
| `algo:projection` | a string | — | A named projection (§2.4) in place of the parameters above. |

**Execution parameters:** `algo:maxIterations`, `algo:tolerance`, `algo:seed`,
`algo:concurrency` (threads, capped by the server), `algo:maxMemory` (bytes, capped by the
query's budget) and `algo:limit` (rows).

**Output parameters**, which bind variables: `algo:node`, and per algorithm `algo:score`,
`algo:component`, `algo:community`, `algo:level`, `algo:otherNode`, `algo:similarity`,
`algo:triangles`, `algo:coefficient`, `algo:core`, `algo:depth`, `algo:distance` and
`algo:embedding`. A call can also bind `algo:summary` to one JSON literal of statistics
(iterations run, convergence, node and edge counts, modularity), which with no other
output variables gives GDS's `stats` mode.

Stats mode emits one row per selected concrete graph, including empty projections and
empty node, pair or community results. Normal SPARQL joins retain their multiplicity,
and selecting no named graphs under `GRAPH ?g` produces no stats row. An explicitly
requested PageRank personalization with no matching node does no ranking work. Its
summary keeps the projection counts and reports zero iterations and convergence,
rather than running unpersonalized PageRank.

### 2.2 Rows and inputs

A call returns one row per node, or per node pair for similarity, or per node and level
for hierarchical communities. Nodes come out as the IRIs or blank nodes of the dataset.
Nodes of the projection that no edge touches are left out, except that `algo:nodeClass`
adds every instance of the class, so isolated members get their own component or score.

Inputs that need nodes, such as the sources of personalized PageRank, of BFS and of
shortest paths, are given by `algo:source`, a constant or a variable that the rest of the
join group binds, as `path:source` is in F07. The call then runs once with all the bound
sources, not once per row, and the planner attaches it after join ordering as it does a
path search.

### 2.3 Graph scope and access

The projection reads the active graph, as a triple pattern in the same place would.
`GRAPH <g>` limits it to `<g>`, and `GRAPH ?g` runs the algorithm once per named graph,
binding `?g`, as path search does since its Phase 1 outcome. The projection is built from
the caller's view: the graphs of its grants ([C12](C12-graph-access-control.md)) and the
masked snapshot of its protections ([C12b](C12b-triple-access-control.md)). A weight on a
reifier the caller cannot read is not read, and the edge gets the default weight.

### 2.4 Named projections

Building a projection reads the index ranges of its predicates once, which for tens of
millions of edges takes seconds. A named projection stores the projection parameters per
dataset, so several calls use one definition, and the server caches the built projection
per (definition, commit, view fingerprint).

```
PUT /$/algo/{ds}/projections/{name}
{ "predicates": [ "foaf:knows" ], "direction": "undirected",
  "nodeClasses": [ "foaf:Person" ], "weight": "ex:strength" }
```

The cache is a bounded LRU charged to the server's cache memory. A call at a new commit
builds a new projection, and an older one is evicted when no call uses it. `GET
/$/algo/{ds}/projections` lists definitions with their cached builds, node and edge
counts and bytes. Defining a projection needs the dataset's `admin` grant, and using one
needs `read`.

### 2.5 Write-back

A SPARQL Update writes results with an ordinary template.

```sparql
PREFIX algo: <urn:x-sparkles:algo#>
DELETE { GRAPH <urn:graph:rank> { ?n ex:rank ?old } }
INSERT { GRAPH <urn:graph:rank> { ?n ex:rank ?r } }
WHERE  { SERVICE algo:pageRank { [] algo:projection "people" ; algo:node ?n ; algo:score ?r . }
         OPTIONAL { GRAPH <urn:graph:rank> { ?n ex:rank ?old } } }
```

That runs in one write transaction, which suits results of up to a few million triples.
For larger results and for runs that should report progress and be cancelled, the run
is a task:

```
POST /$/algo/{ds}/run
{ "algorithm": "leiden", "projection": "citations",
  "params": { "resolution": 1.0, "maxLevels": 3, "seed": 7 },
  "write": { "graph": "urn:graph:communities", "mode": "replace",
             "predicates": { "community": "ex:community", "level": "ex:level" } } }
```

The task computes at the head commit, then replaces the graph's contents with the
results in commits of at most `batchSize` triples (100,000 by default) through the
batched updates of [C23](C23-batched-updates-and-triggers.md), or in one commit when
`"atomic": true`. The graph also receives a run record:

```turtle
<urn:uuid:…> a spk:AlgorithmRun ;
    spk:algorithm "leiden" ;
    spk:parameters "{\"resolution\":1.0,\"maxLevels\":3,\"seed\":7}" ;
    spk:projection "citations" ;
    spk:sourceCommit 412 ;
    prov:startedAtTime "…"^^xsd:dateTime ; prov:endedAtTime "…"^^xsd:dateTime ;
    prov:wasAssociatedWith <urn:x-sparkles:principal:alice> .
```

`mode: "replace"` clears the graph first and `"merge"` replaces only the values of the
written predicates for the nodes in the result. Hierarchical communities are written as
one community resource per community and level, `urn:x-sparkles:algo:{run}:{level}:{id}`,
with `ex:community` from each node to its community at each level and `skos:broader`
between levels, which C22 reads. Writing needs `write` on the target graph.

### 2.6 Embeddings into vector indexes

FastRP and node2vec bind `algo:embedding` to a `spk:vector` literal. A write-back task
with `"write": { "predicates": { "embedding": "ex:fastrp" } }` writes one vector per node.
When an F04 index is configured on `ex:fastrp` with the same dimension, the vectors are
served at once from the index's overlay of the snapshot delta, exactly, and the HNSW
graph covers them after the next rebuild, or after the incremental inserts of ROADMAP
item 10. `spk:vectorSearch` then finds structurally similar nodes, and
`spk:hybridSearch` can rank by them together with text.

### 2.7 CLI, HTTP, bindings, MCP and UI

- **CLI.** `sparkles algo --loc DB run pageRank --predicate foaf:knows --top 20`, with the
  parameters as flags, prints a table or JSON lines. `--write-graph IRI` runs a write-back.
  `sparkles algo projections list|put|delete` manages named projections, and
  `--url` targets a server.
- **HTTP.** Calls in SPARQL go through `/{ds}/sparql` as any query. `POST
  /$/algo/{ds}/run` without `write` streams rows as SPARQL JSON results, with the same
  parameters as JSON, for clients that do not write SPARQL. With `write` it returns a
  task, followed through `/$/tasks/{id}` with progress per phase (projection, iterations,
  write batches) and cancelled with `DELETE`.
- **Rust and bindings.** `Dataset::algorithms()` returns an `Algorithms` handle with
  `run(&AlgoRequest, &Control) -> AlgoRows` and `run_write`, and the projection
  methods. Python exposes `ds.algorithms.page_rank(predicates=[…], …)` and the other
  algorithms as methods that return row iterators of the same solution type that `query`
  returns. Node and the JVM follow P06's mapping.
- **MCP.** The read tool `graph_algorithm` runs one algorithm on predicates or a named
  projection and returns the top rows, at most 100, in the compact table format of C11,
  with the summary. It runs under the MCP budgets and never writes.
- **UI.** The explorer can colour nodes by a community and size them by a score from a
  call, and the query editor completes `algo:` parameters from the algorithm's table.

## 3. Projections

### 3.1 Building

A projection maps the nodes it holds to dense `u32` indexes and stores compressed sparse
rows: an offset array per direction it needs, an array of neighbour indexes, and an
optional `f32` array of weights. Nodes are numbered in the order of their `Id`s, so a
node's index is found by binary search and results are reproducible. Edges come from
index scans as in path search: `PSO` for forward edges of a predicate and `POS` for
backward ones, `SPO` and `OSP` without predicates. A projection over many predicates
reads each predicate's range once and merges them. Triples whose object is a literal
are skipped, as are rows of graphs outside the active graph. Equal triples in several
graphs are one edge.

With `algo:nodeClass`, the class members come from `POS [rdf:type, class]` and edges
with an end outside them are dropped. With weights, each edge's reifiers are looked up
by `rdf:reifies` with its triple term, as F07's weighted search does, and the first
numeric value of the weight property is used. A non-numeric weight fails the call, and a
negative weight fails the algorithms that need non-negative weights.

A nested pattern in the block, as in F07's Phase 2, gives edges from any query: its
solutions' `algo:start` and `algo:end` bindings are the edges and `algo:edgeWeight`
their weights. The pattern is evaluated once and its distinct pairs become the
projection.

### 3.2 Memory

A projection of `n` nodes and `m` edges takes about `8n + 4m` bytes per direction, plus
`4m` for weights and the node id table of `8n`. The ten-million-triple benchmark's
`foaf:knows` graph of 2.6 million edges and about a million people takes about 40 MB
undirected. The projection and every array an algorithm allocates are charged to the
query's memory budget, so a call that would pass it fails before it allocates.

## 4. Algorithms

Each algorithm's defaults follow Neo4j GDS's documentation where GDS has the algorithm,
so results compare across the two, unless Graphalytics defines it differently. For
Graphalytics' algorithms, a parameter `algo:graphalytics true` selects its exact
definition.

| Service | Default direction | Outputs | Parameters and notes |
|---|---|---|---|
| `algo:pageRank` | forward | `score` | `dampingFactor` 0.85, `maxIterations` 20, `tolerance` 1e-7. Pull-based power iteration in parallel over nodes. The rank of dangling nodes is spread evenly, as Graphalytics defines. With `algo:source`, personalized PageRank teleports to the sources only. Weighted with `algo:weight`. |
| `algo:betweenness` | forward | `score` | Brandes' algorithm, parallel over sources, with Dijkstra's algorithm when weighted. `algo:samples k` estimates from `k` sources chosen with `algo:seed`, after Brandes and Pich, and scales the scores. |
| `algo:wcc` | undirected | `component` | Union-find with path compression, parallel over edges. Components are numbered by their smallest node, so numbers are stable for a commit. |
| `algo:scc` | forward | `component` | Iterative Tarjan's algorithm, without recursion. |
| `algo:labelPropagation` | undirected | `community` | `maxIterations` 10. Synchronous updates. A tie goes to the smallest label, which is the CDLP rule of Graphalytics and makes the result deterministic. Weighted with `algo:weight`. |
| `algo:louvain` | undirected | `community`, `level`, `summary` with modularity | `resolution` 1.0, `maxLevels` 10, `tolerance` 1e-4. Local moving in node order, then aggregation, per level. |
| `algo:leiden` | undirected | `community`, `level`, `summary` | As Louvain, with Leiden's refinement phase, so every community is connected. `randomness` 0.01 and `seed` for the refinement. `algo:level` binds one row per node and level, from the finest. |
| `algo:nodeSimilarity` | forward | `node`, `otherNode`, `similarity` | `metric` `jaccard` or `overlap`, `topK` 10 per node, `similarityCutoff` 1e-42, `degreeCutoff` 1. Candidate pairs share a neighbour; pairs are generated per shared neighbour, bounded by `algo:maxPairs`. |
| `algo:triangleCount` | undirected | `triangles` | Forward algorithm over nodes ordered by degree. |
| `algo:clustering` | undirected, or forward with `graphalytics` | `coefficient`, and `triangles` outside Graphalytics mode | Local clustering coefficient. Graphalytics' LCC counts directed edges among neighbours and returns only the coefficient. Explicit projection and predicate directions take precedence. |
| `algo:kCore` | undirected | `core` | Batagelj and Zaversnik's bucket algorithm, linear in edges. |
| `algo:bfs` | forward | `depth` | From `algo:source`. Levels expanded in parallel. Unreached nodes are left out, or bound with depth `"inf"` under `graphalytics`. |
| `algo:sssp` | forward | `distance` | From one `algo:source`, Dijkstra's algorithm with weights, BFS without. |
| `algo:fastRP` | undirected | `embedding` | `dimension` 128, `iterationWeights` (0.0, 1.0, 1.0), `normalizationStrength` 0, `seed`. Very sparse random projection, then powers of the normalized adjacency, as Chen et al. define. |
| `algo:node2vec` | forward | `embedding` | `dimension` 128, `walkLength` 80, `walksPerNode` 10, `windowSize` 10, `returnFactor` (p) 1, `inOutFactor` (q) 1, `negativeSamples` 5, `epochs` 1, `learningRate` 0.025, `seed`. Biased second-order walks, then skip-gram with negative sampling. |

**Determinism.** Every algorithm is deterministic for a commit, a projection, its
parameters and a seed when it runs on one thread. Parallel PageRank, components, BFS,
label propagation, triangles and k-core give the same result on any number of threads,
because their updates are synchronous or order-independent. Louvain's and Leiden's
local moving, betweenness sampling and node2vec's training are sequential in their
dependent steps by default; `algo:concurrency` above 1 parallelizes them and may change
the result, which the documentation says.

**Convergence.** An iterative algorithm that reaches `maxIterations` before `tolerance`
returns its result and sets `converged: false` in the summary. It is not an error.

## 5. Budgets and parallelism

Algorithm calls share a process-wide budget of `--algo-threads` threads, by default
half the cores. A call reserves `algo:concurrency` slots, at most
`--algo-max-threads-per-call` (4 by default), and waits when those slots are busy.
The wait counts against its timeout and checks cancellation. Admission checks both
controls before accepting free slots, including after each wake. A one-thread call
runs on its caller without a Rayon pool and keeps one slot until the call ends.
A multithreaded call owns a Rayon pool under its reservation, separate from the
reasoner's pool. If pool construction fails, it runs sequentially while retaining
that reservation. Returning an error or unwinding releases the reservation.

Native callers can explicitly use `Par::single` or `Par::with_threads` outside this
admission policy. Production `SERVICE algo:` calls use `Par::acquire`.

Each call checks the query's timeout and cancel flag between iterations and every 64K
nodes inside one. Node2vec also checks at entry, before returning and at most every
64K operations across its own initialization, shuffle, walk, neighbor and training
loops. Binary searches count as one operation and take at most the machine word's bit
width in comparisons. These are work bounds, not a wall-clock preemption guarantee.
Node2vec reserves its embedding arrays and temporary buffers before allocation, using
checked capacity arithmetic and fallible allocation. Sampling scratch reserves the
maximum outgoing degree, and walk scratch reserves `walkLength`, even if one walk uses
less. Temporary reservations end after
their buffers are dropped, and the returned embedding remains charged to the call.
This accounts for Vec capacities on the pinned Rust Global allocator, excluding
allocator bookkeeping. Incoming graph storage remains a separate accounting concern.
Memory is charged as §3.2 says. Rows count against the query's row
budget. Betweenness without sampling on more than `--algo-exact-betweenness-max`
nodes (100,000) is refused with a message that suggests `algo:samples`. Node similarity
stops with an error when its candidate pairs pass `algo:maxPairs` (10 million).

A write-back task runs with the dataset's task limits, reports progress per phase, and
stops at a cancel between batches. Batches already committed stay, the run record says
which batches were written, and a second run with `mode: "replace"` produces a complete
graph.

## 6. Architecture

- `crates/sparkles-core/src/algo/` holds the library: `projection.rs` (building, the
  compressed rows and the cache key), `pagerank.rs`, `betweenness.rs`, `components.rs`,
  `labelprop.rs`, `community.rs` (Louvain and Leiden), `similarity.rs`, `triangles.rs`,
  `kcore.rs`, `traverse.rs` (BFS and SSSP), `fastrp.rs`, `node2vec.rs`, and `pool.rs`.
  The algorithms work on `u32` indexes and know nothing of SPARQL.
- `crates/sparkles-core/src/sparql/algosvc.rs` holds the `AlgoCall` leaf. The planner
  recognizes `SERVICE <urn:x-sparkles:algo#…>` before remote SERVICE planning, as it does
  `path:search` in `pathsearch.rs` and `hist:changes` in `history_svc.rs`. A leaf with
  bound sources is attached after join ordering. EXPLAIN shows the algorithm, the
  projection, the parameters and, after execution, the projection's size and build time,
  the iterations and the time per phase. The result cache keys the leaf on its whole
  configuration and the snapshot, as it does path searches.
- Named projections are stored in `<root>/algo.json`. The projection cache lives beside
  the result cache in `sparql::cache` and shares the server's cache memory limit.
- `crates/sparkles/src/handles/algorithms.rs` in the embedding API, and the server's
  `http/algo.rs`, `mcp/algo.rs` and `algo_cmd.rs` on it. The route table, `auth/routes.rs`
  and the OpenAPI description gain `/$/algo/{ds}/…`.
- Write-back builds its triples from the result rows and commits through C23's batched
  writer, so it shares that writer's progress, cancellation and audit.
- No new dependency. node2vec's training is a small skip-gram loop over `f32` arrays, and
  random numbers come from the `rand` crate the engine already uses with a seeded
  generator.

## 7. Phases

**Phase 1.** Projections from predicates, directions, node classes and weights, the
projection cache, `algo:pageRank` with personalization, `algo:wcc`, `algo:scc`,
`algo:labelPropagation`, `algo:triangleCount`, `algo:clustering`, `algo:kCore`,
`algo:bfs`, `algo:sssp`, `algo:summary`, the thread pool and budgets, write-back through
SPARQL Update, and the Graphalytics harness of §9.

**Phase 2.** `algo:betweenness`, `algo:louvain`, `algo:leiden` with levels,
`algo:nodeSimilarity`, nested patterns, named projections, `POST /$/algo/{ds}/run` with
write-back tasks and run records, `sparkles algo`, the library handle, the bindings, the
MCP tool and the UI.

**Phase 3.** `algo:fastRP` and `algo:node2vec`, written to vector indexes, and openCypher
procedures: F01 compiles `CALL algo.pageRank({…}) YIELD node, score` to the same leaf,
with the projection parameters as a map.

## 8. Acceptance examples

The graph for A1 to A8 has `ex:p` edges `a→b`, `b→c`, `c→a`, `c→d`, `d→e`, `e→d`, `f→g`,
and a node `h` with `rdf:type ex:N` and no edges.

- **A1.** `algo:wcc` returns three components: `{a b c d e}`, `{f g}`, and, with
  `algo:nodeClass ex:N` added for `h`, a third component `{h}`.
- **A2.** `algo:scc` returns `{a b c}`, `{d e}`, `{f}` and `{g}`.
- **A3.** `algo:pageRank` with 100 iterations gives scores that sum to 1 within 1e-6, and
  `d` and `e` rank above `a`, `b` and `c`.
- **A4.** Personalized PageRank from `f` gives `g` a positive score and every node of the
  first component a score of 0.
- **A5.** `algo:triangleCount` returns 1 for `a`, `b` and `c` and 0 for the others.
- **A6.** `algo:kCore` returns core 2 for `a`, `b`, `c` and core 1 for the others.
- **A7.** `algo:bfs` from `a` returns depths `a 0`, `b 1`, `c 2`, `d 3`, `e 4`.
- **A8.** A caller whose protection hides `c→d` gets two components among `{a b c}` and
  `{d e}` from `algo:wcc`, as on a dataset without the triple.
- **A9.** On Zachary's karate club graph, `algo:leiden` with seed 1 returns communities
  whose modularity is at least 0.41, every community is connected, and the result is the
  same on a second run.
- **A10.** `algo:nodeSimilarity` with Jaccard on `ex:likes` edges `u1→x`, `u1→y`, `u2→x`,
  `u2→y`, `u3→x` returns `(u1, u2, 1.0)` and `(u1, u3, 0.5)`.
- **A11.** A write-back task of PageRank into `urn:graph:rank` with `batchSize` 2 on A1's
  graph commits in batches, the graph then holds one `ex:rank` per node and a run
  record with the source commit, and a second run with `mode: "replace"` leaves no stale
  value.
- **A12.** `algo:fastRP` with dimension 16 written to `ex:emb`, with an F04 index on
  `ex:emb`, makes `spk:vectorSearch` from `a`'s vector return `b` and `c` before `f`.
- **A13.** `algo:betweenness` without `algo:samples` on a projection over the node limit
  fails with an error that names `algo:samples`.
- **A14.** A call with `algo:maxMemory` below the projection's size fails before
  building it, with a budget error.

## 9. Testing and measurement

**Correctness.** Each algorithm has unit tests on small graphs with known answers, and
differential tests on random graphs against a straightforward reference implementation
in the test: components by repeated search, PageRank by dense matrix iteration,
betweenness by counting shortest paths, triangles by triple loops, k-core by repeated
peeling and similarity by set operations. Leiden is checked for connectedness of every
community and for modularity no lower than Louvain's on the same graph. Embedding
tests check dimensions, determinism with a seed, and that nodes of a planted
two-community graph are closer to their own community's nodes.

**LDBC Graphalytics.** [X06](X06-benchmark-coverage.md) runs the Graphalytics workload: BFS, PageRank, WCC, CDLP,
LCC and SSSP, with `algo:graphalytics true`, on the benchmark's validation graphs and on
its S and M class graphs, converted to RDF with one `ex:edge` predicate and weights on
reifiers for SSSP. A run must pass the benchmark's output validation for every algorithm
and graph. The report gives load time, projection time and processing time per
algorithm and graph, and is published with the numbers of the reference platforms that
the benchmark's public results list for the same hardware class, so the comparison is
explicit about where it is not like for like.

**Other measurements.** The projection build rate in edges per second, the memory per
edge, and the scaling of PageRank, WCC and Leiden with 1, 2, 4 and 8 threads on the
LDBC SNB SF10 `knows` graph and the ten-million-triple benchmark graph.

## 10. Rejected alternatives

- **A graph catalog with mutable projections**, as GDS keeps. Snapshots already give a
  consistent graph, and a projection that outlives its commit would answer with data the
  caller may no longer be allowed to see. The cache gives the reuse without the state.
- **One `SERVICE algo:run` with `algo:algorithm`**, as F07 does for path modes. The
  algorithms take different parameters and outputs, and an IRI per algorithm makes
  completion and errors specific. Path modes share one parameter set.
- **Property functions** in the style of `spk:vectorSearch`. They take positional
  lists, which do not suit a dozen named parameters.
- **Calling an external library** such as igraph or NetworKit through FFI. Both would
  bring a C or C++ build, a copy of the graph in their format, and no access to the
  query's budgets and cancellation. igraph is GPL-licensed.
- **Writing results only through SPARQL Update.** One transaction for tens of millions
  of rows holds the writer and has no progress. The task path batches.
- **Recursive SPARQL for the algorithms.** PageRank or components as repeated updates
  are orders of magnitude slower and write every intermediate state to history.

## 11. Open questions (defaults chosen)

1. **Default `algo:predicate`.** Every predicate between resources, as in F07, which
   includes `rdf:type` edges to classes. Classes then become hubs. The default chosen
   leaves `rdf:type` out unless named, which differs from F07, and the documentation says
   so.
2. **Literal nodes.** Kept out. A user who wants values as nodes uses a nested pattern.
3. **Undirected weights from two directions.** When `a→b` and `b→a` both exist and are
   read undirected with `aggregate none`, they are two parallel edges, as GDS does.
4. **Projection cache across commits.** A projection could be patched with a commit's
   changes instead of rebuilt. That is F13's work, and the default chosen is a rebuild.

## 12. Dependencies

Phase 2's write-back tasks need C23's batched writer. C22 needs Phase 2's Leiden.
F01's procedures need F01 Phase 1. Graphalytics needs X06's harness.

## 13. Later work

F13 could maintain WCC, label propagation and PageRank incrementally after each commit,
as differential dataflow does for iterative computations, so that a view of components
stays current. It is left out of this spec.

## 14. Sources

- Neo4j Graph Data Science library documentation: graph projections, execution modes
  (`stream`, `stats`, `mutate`, `write`) and the pages for each algorithm named in §4,
  for parameters and defaults. Public documentation only.
- Amazon Neptune Analytics documentation of its graph algorithms.
- LDBC Graphalytics specification, and A. Iosup et al., "LDBC Graphalytics: A Benchmark
  for Large-Scale Graph Analysis on Parallel and Distributed Platforms", VLDB 2016.
- L. Page, S. Brin, R. Motwani and T. Winograd, "The PageRank Citation Ranking: Bringing
  Order to the Web", Stanford InfoLab technical report, 1999.
- T. H. Haveliwala, "Topic-sensitive PageRank", WWW 2002.
- U. Brandes, "A faster algorithm for betweenness centrality", Journal of Mathematical
  Sociology 25(2), 2001. U. Brandes and C. Pich, "Centrality estimation in large
  networks", International Journal of Bifurcation and Chaos 17(7), 2007.
- R. Tarjan, "Depth-first search and linear graph algorithms", SIAM Journal on Computing
  1(2), 1972.
- U. N. Raghavan, R. Albert and S. Kumara, "Near linear time algorithm to detect
  community structures in large-scale networks", Physical Review E 76, 2007.
- V. D. Blondel, J.-L. Guillaume, R. Lambiotte and E. Lefebvre, "Fast unfolding of
  communities in large networks", J. Stat. Mech., 2008.
- V. A. Traag, L. Waltman and N. J. van Eck, "From Louvain to Leiden: guaranteeing
  well-connected communities", Scientific Reports 9, 2019.
- V. Batagelj and M. Zaversnik, "An O(m) algorithm for cores decomposition of networks",
  2003.
- D. J. Watts and S. H. Strogatz, "Collective dynamics of 'small-world' networks",
  Nature 393, 1998.
- H. Chen et al., "Fast and Accurate Network Embeddings via Very Sparse Random
  Projection", CIKM 2019.
- A. Grover and J. Leskovec, "node2vec: Scalable Feature Learning for Networks", KDD 2016.
- W. W. Zachary, "An information flow model for conflict and fission in small groups",
  Journal of Anthropological Research 33(4), 1977, for the karate club graph of A9.
- The Sparkles code: `sparql/pathsearch.rs`, `sparql/history_svc.rs`, `sparql/cache.rs`,
  `sparql/plan.rs`, `access.rs`, `task.rs`, `vector/config.rs`, and the specs C01, C11,
  C12, C12b, F04, F07 and P06.

## Outcome

Node2vec now checks bounded work inside its own loops and admits every owned buffer's
fixed capacity before allocation. Controlled cancellation and low-memory tests cover
inner walks and training, and retained seeded fixtures preserve the earlier f32 bits.
Overflow and allocation failures release temporary reservations. This repair does not
complete incoming-CSR accounting or establish a performance result.

**Delivered** on 2026-10-13, as Phases 1 and 2 and the embeddings of Phase 3, with the
gaps listed under "Not built".

The counter confinement, unit-weight sum, Graphalytics clustering, empty-result
statistics, transaction projection-cache and operation-control repairs have targeted
regression coverage. The
remaining findings from the resumed review and the lane's integration and acceptance
gates are still pending.

* **Library.** `crates/sparkles-core/src/algo/` holds the algorithms on compressed
  sparse rows. `graph.rs` builds the projection, and the modules `pagerank`,
  `components` (weakly connected components by an atomic union-find and strongly
  connected components by an iterative Tarjan), `labelprop`, `kcore`, `triangles`,
  `traverse` (breadth-first search and single-source shortest paths), `betweenness`
  (Brandes, sampled when asked), `community` (Louvain, and Leiden with refinement and a
  connectivity split at each level), `similarity`, `fastrp` and `node2vec` hold one
  algorithm each. `pool.rs` holds the control handle (cancel, deadline, memory budget)
  and the thread pools. Every algorithm takes a seed where it needs one, so a run is
  repeatable, and tests check that PageRank and FastRP give the same result at any thread
  count. Production admission includes one-thread calls, which keep one process slot
  while running on the caller without creating a Rayon pool. Cancellation and deadline
  checks apply before free-slot acceptance and after waking. A reservation guard exists
  before pool construction, so unwinding releases slots and sequential construction
  fallback retains its slots until the call ends. Pool regression fixtures isolate
  configuration in subprocesses and prove waiting, control errors and release using
  boundary handshakes.
* **SPARQL.** `sparql/algosvc.rs` plans `SERVICE algo:<name>` as an `AlgoCall` leaf, as
  `path:search` is planned, so the outbound policy and `--no-service` never apply to it.
  A variable `algo:source` makes the leaf read the rest of its join group, and `GRAPH
  ?g` runs the call once for each named graph. The projection is read from the
  permutations of the query's snapshot, so the caller's graph view and masked triples
  decide which edges exist. EXPLAIN shows the counters `nodes`, `edges`, `projectionMs`,
  `runMs`, `peakBytes` and `cachedProjections`.
* **Projection cache.** `algo/cache.rs` keeps built projections under a byte budget
  (`--algo-cache-mb`), keyed by the query's view and the projection's definition, and
  evicts the least recently used. A committed write changes the key, so the next
  call rebuilds. Transaction views explicitly disable lookup and insertion, including
  through masked and derived snapshots, so pending changes neither reuse committed
  projections nor enter their cache. Committed views retain projection reuse when
  the query result cache is disabled. Patching a projection with a commit stays the work of F13, as
  question 4 of §11 decided.
  Sum aggregation retains multiplicity weights even without an explicit weight
  predicate. Each resulting edge uses one additional `f32`, including its reverse
  entry when built. Build reservations and cache accounting include those weights,
  and a sum that is not finite in the stored `f32` representation is refused.
* **Named projections.** `algo/catalog.rs` keeps `<db>/algo.json` as
  `{"format": 1, "projections": {name: {revision, updated, author?, definition}}}`,
  written the way `queries.json` is, with a revision for `If-Match`. A file that cannot
  be read leaves the catalog empty and read-only, so it is never overwritten. The file
  is part of backups, clones and branch copies. `algo:projection "name"` is resolved
  when the query is planned, also inside SPARQL Update, so a query at an older commit
  uses today's definitions.
* **Runs and write-back.** `sparql/algorun.rs` turns a JSON request into the query it
  stands for and runs the batched write-back. The first commit of a run clears the graph
  in replace mode and writes the run record. Every commit holds whole nodes, updates the
  record's `spk:batchesWritten` and `spk:triplesWritten`, and carries the message
  `algo run <uuid> batch i/n`. Counter replacement is confined to the requested
  destination graph. Values for the same run in other graphs are preserved, so
  replacing the counters needs write grants only for the destination graph.
  The last commit adds `prov:endedAtTime` and
  `spk:status`. Task and query cancellation sources share checks in computation, result
  preparation and writer admission. A relative timeout starts at operation entry and is
  intersected with the absolute and task deadlines once, without phase renewal.
  Built-in SHACL and ShEx guards carry those controls through validation and SPARQL
  evaluation. History SERVICE lookups carry composed cancellation and the same absolute
  deadline into the native change-log scan checks. Native history queries accept that
  composition alongside their existing single flag. These checks do not preempt
  history indexing, reads or flushing. Custom guards remain responsible for checking
  the full `WriteOptions`.
  A partial cancelled or failed run attempts one optional status commit with no writer
  wait and a separate 100 ms cooperative cleanup budget. The budget cannot interrupt a
  durability call, and recording failure never replaces the original error. A control
  error observed after the final batch preserves its durable complete record. A cancel
  can still race the final check. In merge mode the old values of the written predicates are deleted inside the
  batch's own commit. Louvain and Leiden communities are written as resources
  `urn:x-sparkles:algo:{run}:{level}:{id}` with a level and a `skos:broader` link.
  The writer of C23 does not exist yet, so the loop is built on the store's
  `try_write_with`, and a later C23 writer can take its place.
* **Surfaces.** `sparkles::Dataset::algorithms()` holds `run`, `stats`, `run_write` and
  the projections. `POST /$/algo/{ds}/run` runs a request, as rows or as a cancellable
  task of kind `algo-run`, and `/$/algo/{ds}/projections[/{name}]` manages projections
  with `ETag` and `If-Match`. `sparkles algo run` and `sparkles algo projections` work
  on a database directory. The MCP tool `graph_algorithm` is read-only and returns at
  most 100 rows with the run's summary. The Python and Node.js bindings have the same
  `algorithms` handle. The server flags are `--algo-threads`,
  `--algo-max-threads-per-call`, `--algo-cache-mb` and `--algo-exact-betweenness-max`.
  The OpenAPI description, `docs/openapi.json` and the generated TypeScript types
  describe the routes.
* **Tests.** The acceptance examples A1 to A14 run as tests: `tests/graph_algorithms.rs`
  for the queries and the budgets, `tests/graph_algorithm_runs.rs` for write-back, and
  the unit tests of each module, which compare PageRank with dense iteration,
  betweenness with counting shortest paths, components with repeated search, triangles
  with a triple loop, k-core with repeated peeling and similarity with set operations.
  A9 holds with seed 1 (modularity at least 0.41, every community connected, the same
  result twice), and on random planted-community graphs Leiden's modularity is within
  0.01 of Louvain's with every community connected. Further tests cancel a running
  node2vec call by flag and by deadline, run eight calls at once, and check the EXPLAIN
  counters. The HTTP, MCP, Python and Node.js surfaces have a test each.

**Measurements.** One run on a 20-core Intel host with nothing else running, on a
synthetic graph of 2,000,000 nodes and 10,000,000 undirected edges (20,000,000 stored
edges) with a skewed degree distribution
(`tests/graph_algorithms_scale.rs`, `ALGO_NODES=2000000 ALGO_EDGES=10000000`).

| | 1 thread | 2 | 4 | 8 |
|---|---|---|---|---|
| PageRank, 20 iterations | 1343 ms | 792 ms | 431 ms | 326 ms |
| Weakly connected components | 342 ms | 174 ms | 87 ms | 68 ms |
| Label propagation | 1478 ms | 884 ms | 399 ms | 258 ms |
| Triangle count | 1353 ms | 952 ms | 585 ms | 469 ms |

K-core takes 650 ms on one thread. Louvain takes 88 s and Leiden 113 s, both on one
thread, and reach modularity 0.2345 and 0.2372 on this graph, which has no planted
structure. Building the projection from edge arrays on 8 threads takes 0.80 s, 25
million stored edges per second, and the projection takes 107 MiB, 5.6 bytes per stored
edge. The build rate does not include the scan of the permutations that feeds it, so a
query's `projectionMs` is higher. These numbers are not from the LDBC SNB SF10 `knows`
graph or the ten-million-triple benchmark graph that §9 names.

**Deviations.**
* A1 says `{h}` appears as a third component "with `algo:nodeClass ex:N`". `nodeClass`
  keeps the nodes of the class, so the result is the single component `{h}`, not three.
  The test expects that, and the nodes of a class include those without edges.
* `algo:limit` keeps the best rows by the algorithm's main measure before the call's
  rows are joined with the rest of the group. Ties go to the lower term id, and BFS and
  SSSP return the nearest nodes first.
* `algo:nodeSimilarity` returns each pair in both orders, with the `topK` best partners
  of each node. A caller who wants one row per pair uses `SELECT DISTINCT` with an
  ordered filter. A later `algo:distinctPairs` parameter could do it in the leaf.
* A double result has its four lowest mantissa bits cleared, so that every score is an
  inline id. A caller who compares two scores for equality across runs gets the same
  values, and one who compares with a value computed elsewhere should allow a relative
  difference of 2^-48.
* The default `algo:predicate` leaves `rdf:type` out, as question 1 of §11 decided, and
  the API reference says so.
* The summary is an `rdf:JSON` literal in the `algo:summary` variable.
* `GRAPH ?g` runs the call once for each named graph, as path search does.
* Embeddings reach a vector index through `INSERT { ?n ex:emb ?e } WHERE { SERVICE
  algo:fastRP … }` or through a write-back run. The call has no direct write into an
  index, because F04 indexes follow the commits either way.

**Risks that stay open.**
* A replace run commits its first batch before the others, so a reader sees part of the
  new results until the run ends. `atomic` avoids it at the price of one large commit.
* Merge mode with hierarchical output leaves the community resources of an earlier run
  in the graph, because they are not the written predicate of any node.
* Nothing reports a run as `interrupted` after a restart. Its record stays at the last
  batch it wrote, without an end time.
* The default `batchSize` of 100,000 is below the store's bulk threshold of 250,000
  quads, so a run's commits stay ordinary commits. A larger batch takes the bulk path.

**Not built.**
* The LDBC Graphalytics harness and its validation runs. X06 specifies the harness and
  has not built it, and no Graphalytics datasets are in the repository. The
  `algo:graphalytics` switches on PageRank, BFS, SSSP, clustering and the others do what
  the benchmark's definitions say, and the tests cover them on small graphs only.
* The openCypher procedures, which wait for F01.
* The web UI: colouring a graph view by community, and `algo:` completions in the
  editor.
* The JVM binding. `SERVICE algo:…` works through it, but there is no typed handle.
* The routing of write-back through the writer of C23.

