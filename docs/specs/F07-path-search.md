# F07: Path search

> **Status:** implemented in part (Phases 1 and 2)
>
> **Phases:** Phase 1 shipped. It covers `SERVICE path:search` with the four modes,
> predicate sets and directions, ends bound by the query, weights on reifiers, the
> limits and the masked view. Searches per named graph under `GRAPH ?g`, planned for
> Phase 2, came with it, and the MCP tool `find_paths` came later. The rest of Phase 2
> shipped on 2026-10-03: edges from a nested pattern, a direction per predicate and the
> `path:edge` binding. Phase 3 waits for the Cypher frontend of [F01](F01-cypher.md),
> which is not built.
>
> **User docs:** [API: Path search](../API.md#path-search) · [Usage: Finding paths](../USAGE.md#finding-paths) · [Features](../FEATURES.md#sparql-arq-equivalent)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

The design depends on the budgets of [C01](C01-observability-and-budgets.md) and on the
filtered dataset views of [C12](C12-graph-access-control.md) and
[C12b](C12b-triple-access-control.md). [F01](F01-cypher.md) plans to compile Cypher's
`shortestPath` and GQL's `ANY SHORTEST` to the search described here.

## 1. Summary, goals, non-goals

SPARQL 1.1 property paths answer reachability. `?x foaf:knows+ ?y` says that a path
exists, but the query cannot see the path, its length or its edges. A path search returns
the paths themselves as solutions. Each solution row holds the path's start and end, a
path number, and either one edge of the path or the path as a whole.

The search is a local `SERVICE` with a reserved IRI and a block of configuration triples,
the form QLever and GraphDB use for their path services. It needs no new grammar, so it
parses with the vendored spargebra and with any SPARQL 1.1 tool.

```sparql
PREFIX path: <urn:x-sparkles:path#>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>
SELECT ?path ?i ?s ?o WHERE {
  SERVICE path:search {
    [] path:source <http://example.org/alice> ;
       path:target <http://example.org/dave> ;
       path:predicate foaf:knows ;
       path:algorithm path:shortest ;
       path:pathIndex ?path ;
       path:edgeIndex ?i ;
       path:edgeSubject ?s ;
       path:edgeObject ?o .
  }
}
ORDER BY ?path ?i
```

**Goals**

1. Paths as bindings. A path has a start, an end, a number, a length, a cost and an
   ordered list of edges, and each edge is a stored triple.
2. Four modes. One shortest path per pair (GQL's `ANY SHORTEST`), all shortest paths per
   pair (`ALL SHORTEST`), the k shortest paths per pair (`SHORTEST k`, by Yen's
   algorithm), and all paths up to a length.
3. Edges given by a set of predicates or by every predicate, followed forward, backward
   or in both directions.
4. Sources and targets as constants or as variables bound by the rest of the group, so
   that a `VALUES` block or a pattern chooses the pairs.
5. Optional edge weights read from a numeric property of each edge's RDF 1.2 reifier,
   with Dijkstra's algorithm for the cheapest paths.
6. Limits on length, on the number of paths and on the nodes a search visits, inside the
   query's existing timeout, row and memory budgets.
7. Exact answers under access control. A path never uses a triple the caller cannot see.
8. Searches that read the permutations directly, with bidirectional breadth-first search
   for unweighted shortest paths, so that a path query costs no more than the property
   path query that tests the same reachability.

**Non-goals**

- New SPARQL grammar, such as Stardog's `PATHS` query form or Virtuoso's `TRANSITIVE`
  options.
- Edges given by an arbitrary graph pattern. QLever and GraphDB allow a nested pattern
  that binds the two ends of an edge. Phase 2 adds it (§6).
- Graph algorithms beyond paths, such as centrality, communities or connected components.
- Paths across a remote SERVICE.
- Walks and trails. Every path is simple, as §4.1 defines.

## 2. User-visible behavior

### 2.1 Syntax

All terms live in the namespace `PREFIX path: <urn:x-sparkles:path#>`. The search is
`SERVICE path:search { … }`. The block holds triples whose subject is one blank node, or
any single term, and whose predicates are the parameters below. A parameter outside the
table is an error, as is a parameter given twice when it takes one value.

| Parameter | Value | Default | Meaning |
|---|---|---|---|
| `path:source` | a variable or a constant | required | The first node of each path. |
| `path:target` | a variable or a constant | required | The last node of each path. |
| `path:algorithm` | `path:shortest`, `path:allShortest`, `path:kShortest` or `path:all` | `path:shortest` | The mode (§4.2). |
| `path:predicate` | an IRI, repeatable | every predicate | The predicates whose triples are edges. |
| `path:direction` | `path:forward`, `path:backward` or `path:both` | `path:forward` | How an edge's triple is followed (§4.3). |
| `path:minLength` | a non-negative integer | 1 | The fewest edges of a path. |
| `path:maxLength` | a non-negative integer | none, required for `path:all` | The most edges of a path. |
| `path:k` | a positive integer | required for `path:kShortest` | Paths per pair in `path:kShortest`. |
| `path:limit` | a positive integer | none | The most paths the call returns, over all pairs. |
| `path:maxVisited` | a positive integer | 10,000,000 | The most nodes one search may visit (§4.7). |
| `path:weight` | an IRI | none | The property of an edge's reifier that holds its weight (§4.5). |
| `path:defaultWeight` | a non-negative number | 1 | The weight of an edge without one. |
| `path:pathIndex` | a variable | — | The path's number in the result, from 0. |
| `path:edgeIndex` | a variable | — | The edge's position in its path, from 0. |
| `path:edgeSubject`, `path:edgePredicate`, `path:edgeObject` | variables | — | The edge's triple as it is stored. |
| `path:length` | a variable | — | The number of edges, an `xsd:integer`. |
| `path:cost` | a variable | — | The sum of the weights, an `xsd:double`. Without `path:weight` it equals the length, as an `xsd:integer`. |

Parameter values other than the sources, the targets and the output variables must be
constants. A blank node in a source or target position is a variable, as everywhere in a
pattern.

### 2.2 Rows

If any of `path:edgeIndex`, `path:edgeSubject`, `path:edgePredicate` or
`path:edgeObject` is given, the call returns one row per edge, like GraphDB, Stardog and
QLever. Otherwise it returns one row per path. A path of length zero has one row, with the
edge variables unbound.

The source and target variables are bound to each path's first and last node. The other
output variables are bound as the table says. Rows come in path order, and within a path
in edge order, but a query that needs an order should say `ORDER BY ?path ?i`.

### 2.3 Sources and targets from the rest of the query

The service joins with the rest of its group like any pattern. When `path:source` or
`path:target` is a variable that another pattern of the same group binds, the search runs
from the values that pattern produces, and each of its rows is extended with the paths
between its own source and target. A row whose source is bound and whose target is not
gets the paths to every reachable node. A row with a target and no source gets the paths
from every node that reaches the target. A row with neither is an error, as is a search
whose source and target are both variables that nothing binds.

```sparql
# shortest friend chains from each of two people to anyone who works for org/7
SELECT ?a ?b ?len WHERE {
  VALUES ?a { ex:person/1 ex:person/2 }
  ?b ex:worksFor ex:org/7 .
  SERVICE path:search {
    [] path:source ?a ; path:target ?b ; path:predicate foaf:knows ;
       path:length ?len .
  }
}
```

### 2.4 Graph scope

The search reads the active graph, as a triple pattern in the same place would. Inside
`GRAPH <g>` it reads `<g>`. In a merged default graph a triple stored in several graphs is
one edge. `GRAPH ?g` around the service is an error in Phase 1, because a path whose edges
come from several graphs has no single graph to bind.

### 2.5 Plans

`EXPLAIN` and the query plan show a `PathSearch` operator with its mode, predicates,
direction and limits. After execution it carries counters for the searches run, the nodes
visited and the paths found.

## 3. Standards and sources

**SPARQL 1.1 property paths.** SPARQL 1.1 Query §9 defines `p+`, `p*` and the other
path operators by their reachability, with no binding for the path itself. The working
group's "PathLength" and "Path binding" feature pages record the requests for lengths and
paths that the standard left out. Arbitrary-length paths are evaluated with set semantics,
so their cost is a reachability search, and the search here reads the same index ranges.

**QLever.** QLever's `pathSearch` service (Apache-2.0 source in `src/engine/PathSearch.*`
and `src/parser/PathQuery.*`) takes `source`, `target`, `start`, `end`, `pathColumn`,
`edgeColumn`, `edgeProperty`, `algorithm` (only `allPaths`), `cartesian`,
`numPathsPerTarget` and `maxDepth`. Edges come from a nested subquery. It returns one row
per edge with a path column and an edge column, and its sources may be bound by the rest
of the query. The magic-service form, the one-row-per-edge result and the binding of
sources from outside come from here.

**GraphDB.** GraphDB's graph path search (`SERVICE path:search`, namespace
`http://www.ontotext.com/path#`) has the modes `path:shortestPath`, `path:allPaths`,
`path:distance` and `path:cycle`, the bindings `path:sourceNode`, `path:destinationNode`,
`path:pathIndex`, `path:resultBindingIndex`, `path:resultBinding`, `path:startNode`,
`path:endNode` and `path:propertyBinding`, and the limits `path:minPathLength` and
`path:maxPathLength`, whose default is 8. `path:bidirectional true` follows edges both
ways. Edges are a nested `SERVICE <urn:path> { … }` pattern. The parameter names
`pathIndex` and `edgeIndex`, the indexes from 0, the lengths and the both-ways direction
come from here.

**Stardog.** Stardog's `PATHS [SHORTEST | ALL] [CYCLIC] START ?s [= iri | { pattern }]
END ?e … VIA pattern | ?p | path [MAX LENGTH n] [LIMIT n]` is a query form of its own. It
returns one edge per row, separates paths with an empty row, and returns shortest paths by
default. Its `START` and `END` may be patterns, and a free end variable means any node.
Unbound ends here follow that reading. The empty separator row is not adopted, because
`pathIndex` numbers the paths.

**Neo4j and GQL.** Neo4j's `shortestPath` and `allShortestPaths`, and the ISO GQL path
selectors `ANY SHORTEST`, `ALL SHORTEST`, `SHORTEST k`, `ANY` and the path modes `WALK`,
`TRAIL`, `SIMPLE` and `ACYCLIC`, give the four modes their meaning. GQL's `SIMPLE` mode
defines the paths here: no node repeats, except that the last node may be the first.

**Virtuoso.** Virtuoso's `OPTION (TRANSITIVE, t_in(…), t_out(…), t_min, t_max,
t_direction, t_shortest_only, t_distinct, t_step('path_id'), t_step('step_no'), t_no_cycles,
t_cycles_only, t_end_flag)` binds a path id and a step number per row. It confirms the
row-per-step shape, the direction values (forward, backward, both) and the cap on length.

**Algorithms.** Breadth-first search, and its bidirectional form, which meets in the
middle and expands the smaller frontier (Pohl 1971). Dijkstra's algorithm (1959) for
non-negative weights. Yen's algorithm (1971) for the k shortest loopless paths. The
pruning of the all-paths search by the distance to the target is the standard bound for
bounded simple-path enumeration.

## 4. Semantics

### 4.1 Paths

A path from `s` to `t` is a sequence of edges `e0 … en-1`, each a triple of the active
graph that matches the predicates, where edge `i` leads from node `i` to node `i + 1` in
the direction of §4.3, node 0 is `s` and node `n` is `t`. No node occurs twice, except
that node `n` may equal node 0. So a path from a node to itself is a cycle, and the
shortest path from `s` to `s` is the shortest cycle through `s`, which matches
`s p+ s`. The path of length zero from `s` to `s` exists when `path:minLength` is 0.

Literals are nodes. A literal object can end a path, and no edge leaves it.

Two edges are the same when their triples are the same. A path that uses `(a p b)` and one
that uses `(a q b)` are different paths.

### 4.2 Modes

- **`path:shortest`.** For each pair, one path of the smallest length, or of the smallest
  cost with weights. Among equal paths the search returns the one it finds first, which is
  deterministic for a given snapshot.
- **`path:allShortest`.** For each pair, every path of the smallest length or cost.
- **`path:kShortest`.** For each pair, the `k` paths of smallest length or cost, by Yen's
  algorithm. Fewer exist when the graph has fewer. Ties at the `k`-th place are broken by
  the order of discovery. The target must be bound.
- **`path:all`.** For each pair, every path whose length lies between `path:minLength`
  and `path:maxLength`, in depth-first order. `path:maxLength` is required.

In the two shortest modes `path:minLength` may only be 0 or 1. The other two modes filter
by both lengths.

### 4.3 Direction

With `path:forward`, a triple `(a p b)` is an edge from `a` to `b`. With
`path:backward` it is an edge from `b` to `a`. With `path:both` it is both. The edge
variables always give the triple as stored, so a backward edge reports its subject as the
later node.

### 4.4 Pairs and results

The pairs searched are the distinct (source, target) values of the input rows, or the one
pair of constants. A row of the input is extended with the paths of its own pair, and
compatible output variables follow SPARQL's join: a variable the input binds must hold
the same value in the path's row.

`path:limit` caps the paths of the whole call. The search stops once it has found that
many, so which paths are returned depends on the order of the pairs, which is the order
of their term ids. `pathIndex` numbers the paths in the order they were found.

### 4.5 Weights

`path:weight ex:w` reads the weight of the edge `(s p o)` from the RDF 1.2 reifiers of the
triple term `<<( s p o )>>`. The weight is the value of `ex:w` on a reifier `r` with
`r rdf:reifies <<( s p o )>>`, which is what the Turtle annotation
`:a :road :b {| :km 12 |}` writes. With several reifiers or values the smallest value is
used. An edge without one has `path:defaultWeight`. A weight that is not a number, is
negative or is NaN fails the query.

With weights, `path:shortest` and `path:allShortest` use Dijkstra's algorithm and
`path:kShortest` uses Yen's algorithm with Dijkstra's as its inner search. Lengths still
count edges, and `path:maxLength` and `path:minLength` filter the paths the search
enumerates in cost order.

### 4.6 Access control

The search reads the caller's view of the dataset. A grant limited to some graphs limits
the active graph as for any pattern ([C12](C12-graph-access-control.md)). A protection that
hides triples hides them from the masked snapshot the query runs on
([C12b](C12b-triple-access-control.md)), so a path never crosses a hidden triple, and a
weight on a hidden reifier is never read. No separate check is needed, and the answers are
the ones a caller would get on a dataset that held only the visible triples.

### 4.7 Budgets

Each search counts the nodes it visits. A search that visits more than
`path:maxVisited` fails with an error that names the parameter, rather than returning a
partial answer. The visited sets and parent lists are charged to the query's memory
budget, the rows to its row budgets, and the search checks the timeout and cancellation
at every level of its frontier. Enumeration in `path:allShortest` and `path:all` can
produce exponentially many paths, which `path:limit` and the row budget bound.

### 4.8 Errors

Every malformed call fails at planning time with a `400` that starts with
`path:search:`. The cases are an unknown parameter, a missing source or target, a
variable where a constant is required, a value of the wrong kind, `path:kShortest`
without `path:k`, `path:all` without `path:maxLength`, a `path:minLength` above 1 in a
shortest mode, and `GRAPH ?g` around the call. An unbound source and target, a
`path:kShortest` row without a target, too many visited nodes and a bad weight fail at
execution.

## 5. Design sketch

**Planning.** The planner recognizes `SERVICE <urn:x-sparkles:path#search>` before it
would plan a remote call and builds a `PathSearch` leaf from the configuration triples.
Constants become ids. Predicates absent from the store are dropped, and a call whose
predicates are all absent has no edges. When a source or target is a variable, the leaf
is attached to the rest of its join group as the group's last operator, the way
`spk:vectorSearch` with a variable query is attached. The leaf never reaches the remote
SERVICE executor, so `--no-service` and the outbound policy do not apply to it.

**Adjacency.** The neighbours of a node come from index scans. With predicates, forward
neighbours come from `PSO [p, x]` and backward ones from `POS [p, x]`. Without
predicates, from `SPO [x]` and `OSP [x]`. A large frontier of a breadth-first level is
expanded by one merged pass over the predicate's range, as the transitive path operator
does. Rows of graphs outside the active graph are skipped, and equal triples from several
graphs are merged. Searches that revisit nodes, such as Yen's spur searches, the
depth-first enumeration and Dijkstra's algorithm, cache each node's neighbours for the
call.

**Searches.**

- Unweighted shortest paths for a single pair use bidirectional breadth-first search. The
  side with the smaller frontier expands a whole level, and the search stops at the first
  level where the two visited sets meet. All meeting nodes then lie at the same total
  distance, and the paths are the products of the forward and backward parent lists of
  each meeting node.
- A source with several targets, or with an unbound target, uses one forward
  breadth-first search that stops when every target has been reached. A target with an
  unbound source uses one backward search.
- Weighted searches use Dijkstra's algorithm from the source, with a binary heap, and
  stop when every target is settled.
- `path:kShortest` uses Yen's algorithm. Its spur searches exclude the root's nodes and
  the edges that earlier paths with the same root took, and with no weights they are
  breadth-first searches bounded by the remaining length.
- `path:all` enumerates simple paths depth first. With a bound target it first computes
  each node's distance to the target by a backward search bounded by `path:maxLength`,
  and only extends a path to nodes that can still reach the target in time.
- A cycle through the source is found by giving the target a virtual id, so that the
  source can be both the first node and the last.

**Output.** Each search yields paths as edge lists. The executor numbers them, builds one
table of the path columns and extends each input row with the rows of its pair.

## 6. Phasing

**Phase 1.** Everything in §2 and §4. The `PathSearch` operator, the four modes, the
predicate sets and directions, constant and bound sources and targets, weights from
reifiers, the limits, documentation, tests and a measurement against the property path
query of the same reachability.

**Phase 2.** Edges from a nested pattern, as QLever's subquery and GraphDB's
`SERVICE <urn:path>` give them. A per-predicate direction. `GRAPH ?g` around the call,
with a path per graph. A `path:edge` binding of the edge as a triple term. An MCP tool
for paths between two resources.

**Phase 3.** Cypher's `shortestPath`, `allShortestPaths` and GQL's `ANY SHORTEST`
compiled to this operator by [F01](F01-cypher.md) Phase 3.

## 7. Acceptance examples

The graph for A1 to A6 has the triples `a→b`, `b→c`, `c→d`, `a→e`, `e→d`, `d→a` and
`b→d`, all with `ex:p`, and `x→y` with `ex:q`.

- **A1.** `path:shortest` from `a` to `d` returns one path of length 2, through `b` or
  `e`.
- **A2.** `path:allShortest` from `a` to `d` returns two paths, `a b d` and `a e d`.
- **A3.** `path:kShortest` with `k` 3 from `a` to `d` returns `a b d`, `a e d` and
  `a b c d`, the last of length 3.
- **A4.** `path:all` with `path:maxLength` 3 from `a` to `d` returns the same three paths.
- **A5.** `path:shortest` from `a` to `a` returns the cycle `a b d a` or `a e d a`, of
  length 3. With `path:minLength 0` it returns the path of length zero.
- **A6.** `path:shortest` from `x` to `a` returns nothing, and with no predicate and
  `path:direction path:both` it still returns nothing, because `x` and `y` are not
  connected to `a`.
- **A7.** With `VALUES ?s { a b }` and an unbound target, each source gets one path to
  each node it reaches, and the reachable sets equal those of `?s ex:p+ ?t`.
- **A8.** With weights `a→b` 1, `b→d` 5, `a→e` 1, `e→d` 1 on reifiers, weighted
  `path:shortest` from `a` to `d` returns `a e d` with cost 2.
- **A9.** A caller whose protection hides `e→d` gets `a b d` as the only shortest path,
  and `path:allShortest` returns one path.
- **A10.** A search with `path:maxVisited 2` over a larger graph fails with an error that
  names `path:maxVisited`.

## 8. Rejected alternatives

- **A `PATHS` query form or `TRANSITIVE` options.** Both need grammar changes in the
  vendored parser, and queries written for them would not parse anywhere else.
- **A property function with list arguments**, in the style of `spk:vectorSearch`. A
  search has about fifteen parameters, which are hard to read as positional list
  elements. Named triples in a `SERVICE` block read better and match QLever and GraphDB.
- **Binding the whole path as one value**, such as a list literal or a JSON string. SPARQL
  1.1 has no list type, and rows per edge join with the rest of the query.
- **Walk or trail semantics.** Walks make `path:all` infinite on cyclic graphs, and trails
  allow a node to repeat, which surprises users who think of routes. GQL's `SIMPLE` mode
  gives the paths people expect and keeps the result finite.
- **Weights on the target node or on the predicate.** A weight belongs to an edge, and
  RDF 1.2 reifiers are the standard place for statements about a triple. F01 maps
  relationship properties to reifiers in the same way.
- **Truncating a search that visits too many nodes.** A truncated shortest-path answer
  would look like a real one. Failing with an error keeps the answers exact.
- **Reusing the transitive path operator.** It keeps only visited sets, without parents,
  and it answers reachability per binding. The searches here need parent lists, cycles
  through the source and enumeration, so they get their own operator that reuses the same
  index access.

## 9. Open questions (defaults chosen)

1. **Default `path:minLength`.** 1, so that a search from a node to itself looks for a
   cycle, as `p+` does. 0 adds the empty path.
2. **Default `path:maxVisited`.** 10 million nodes. A breadth-first search over the whole
   10.5M benchmark graph visits about a million people, and memory is bounded separately.
3. **Every predicate when none is given.** Kept, as in GraphDB and Stardog. The search
   then follows `rdf:type` and every other triple, which the docs warn about.
4. **Zero-weight edges in `path:allShortest`.** Dijkstra's algorithm records the parents
   of a node only until the node is settled. With edges of weight zero between nodes at
   the same distance, some equally cheap paths can be missed. Positive weights are exact.

## 10. Sources

- W3C, SPARQL 1.1 Query Language, §9 Property Paths, and the SPARQL working group's
  feature pages "PathLength" and "Path binding" (2009).
- W3C, RDF 1.2 Concepts and Turtle 1.2 drafts, for triple terms, `rdf:reifies` and
  annotations.
- QLever, `src/engine/PathSearch.h`, `src/engine/PathSearch.cpp`,
  `src/parser/PathQuery.h` and `src/parser/PathQuery.cpp` (Apache-2.0).
- Ontotext GraphDB documentation, "Graph path search" (version 10.8).
- Stardog documentation, "Path Queries".
- Neo4j Cypher manual, "Shortest paths", and public summaries of ISO/IEC 39075:2024 (GQL)
  path selectors and path modes. Deutsch et al., "Graph Pattern Matching in GQL and
  SQL/PGQ" (SIGMOD 2022).
- OpenLink Virtuoso documentation, "Transitivity in SPARQL" and "Transitivity in SQL".
- I. Pohl, "Bi-directional Search", Machine Intelligence 6 (1971).
- E. W. Dijkstra, "A note on two problems in connexion with graphs", Numerische
  Mathematik 1 (1959).
- J. Y. Yen, "Finding the K Shortest Loopless Paths in a Network", Management Science 17
  (1971).

## Outcome

**Delivered** on 2026-10-02, as Phase 1.

* **Planning.** `crates/sparkles-core/src/sparql/pathsearch.rs` holds the operator. The
  planner turns `SERVICE <urn:x-sparkles:path#search>` into a `PathSearch` leaf before
  it would plan a remote call, so the outbound policy and `--no-service` never apply to
  it. A leaf with a variable source or target is attached to the rest of its join group
  after the join order is chosen, the way a vector search with a variable query is. The
  leaf's description in EXPLAIN gives the mode, the ends, the predicates, the direction
  and the limits, and the executed plan adds the counters `searches`, `visited`, `paths`
  and `sweeps`. The result cache keys the leaf on its whole configuration.
* **Adjacency.** Each predicate is a lane of `PSO` or `POS` rows, and a search without
  predicates reads `SPO` and `OSP`. A breadth-first level of at least 64 nodes whose
  size times 512 reaches the lane's row count is expanded by one merged pass over the
  lane, as in the transitive path operator. Smaller levels seek per node. Rows of
  graphs outside the active graph are skipped and equal triples of several graphs are
  merged. In both directions a self-loop is one edge.
* **Searches.** One unweighted pair runs as a bidirectional breadth-first search that
  expands the smaller frontier a whole level at a time. A source with several or
  unbound targets runs one forward search, and a target without a source one backward
  search. Weighted searches use Dijkstra's algorithm with a binary heap.
  `path:kShortest` uses Yen's algorithm, and its unweighted spur searches are
  bidirectional too, around the banned nodes and edges. `path:all` is a depth-first
  enumeration pruned by a bounded backward search from the targets. A cycle through
  the source ends at a virtual id that stands for the source.
* **Output.** A path that no output needs edges for (no edge variable, no weight, one
  path per pair) keeps only its length, which the breadth-first tree already knows.
  A search to every node returns its paths in the order it finished their ends, by
  length and id for a breadth-first search and by cost for Dijkstra's algorithm. Each
  input row is extended with the rows of its own pair, and output variables that the
  input binds are joined, not overwritten.
* **MCP.** No dedicated tool was added. Path searches run through `sparql_query` and
  `explain_query`, and `explain_query` no longer warns that SERVICE is disabled when the
  only SERVICE is a path search.

**Deviations.**
* `GRAPH ?g` around the call is not an error. The planner already evaluates a group
  that is not a plain join group once per named graph, so each named graph is searched
  on its own and `?g` is bound to it. The error of §4.8 remains for a graph variable
  that reaches the leaf any other way.
* In the shortest modes, a weighted search with `path:maxLength` enumerates paths in
  cost order with Yen's algorithm and keeps the cheapest ones within the length, which
  is exact. It therefore needs both ends bound, like `path:kShortest`.
* `path:maxVisited` counts every search of a pair, so the spur searches of Yen's
  algorithm share one budget.
* `path:defaultWeight` without `path:weight` is an error, and so is `path:k` outside
  `path:kShortest`.
* RDFS on read does not add edges. The search reads the stored triples of the view, so
  a triple derived through `rdfs:subPropertyOf` on read is not an edge.

**Tests at landing.** `crates/sparkles-core/tests/path_search.rs` has ten tests. They cover
the acceptance examples A1 to A10, rows per path and per edge, the three directions,
ends from `VALUES`, `BIND` and triple patterns, limits and every planning error,
searches per named graph and in the union graph, weights from annotations, and a
protection that hides an edge. Two differential tests build random graphs with two
predicates and compare every mode and direction with paths enumerated by brute force
in the test, the unbounded search with the `+` property path, and the weighted modes
with brute-force costs. `mise run ci` passes, and the W3C suites pass 482/328/157/269 with no failures.

**Performance at landing.** Measured on the benchmark data's `foaf:knows` graph with a
warm server, 31 interleaved runs per query, while other builds kept the load average
between 33 and 80 on 16 cores ([BENCHMARKS.md](../BENCHMARKS.md#path-search)). The
fastest runs at 10.5M were 3.0 ms for the shortest path between two people 17 edges
apart, against 123 ms for `ASK` with `foaf:knows+` on the same pair, and 2.7 ms for all
three shortest paths. One shortest path from a person to each of the 892,556 nodes it
reaches took 227 ms, against 133 ms for counting the same nodes with `foaf:knows+`, so
returning paths costs 1.7 times the reachability query. The 5 shortest paths took 69 ms,
and every path of up to 4 edges took 0.7 ms, the same as the `UNION` of four fixed-length
property paths. Before Yen's spur searches became bidirectional, the 5 shortest paths
visited more than 10 million nodes and failed. A weighted search from one person to
everyone took 1.6 s, because Dijkstra's algorithm seeks the index once per node instead
of sweeping whole levels.

**Not built at first.** Edges from a nested pattern, a direction per predicate, a
`path:edge` binding of the edge as a triple term and the Cypher frontend's use of the
operator were not built with Phase 1. Zero-weight edges can hide equally cheap paths
from `path:allShortest`, as §9 says.

**Later additions (2026-10-03).** The MCP server gained a dedicated tool, `find_paths`,
which writes the search's block from its arguments and returns the paths with their
edges ([C11](C11-mcp-server.md#outcome)).

**The rest of Phase 2 (2026-10-03).**
* **A direction per predicate.** The spec named the feature but not its syntax. A
  `path:predicate` value can be a list of the predicate and a direction, as in
  `path:predicate ex:knows, (ex:parent path:backward)`. Such a predicate is followed in
  its own direction, and `path:direction` applies to the others. Each predicate's
  direction gives its own index lanes, so a mixed search reads the indexes exactly as a
  search with one direction does. The planner takes a list's `rdf:first` and `rdf:rest`
  triples out of the block before it reads the parameters.
* **`path:edge`.** It binds each edge, in the rows per edge, to the triple term
  `<<( s p o )>>` of its stored triple, built once per edge of the call. The term joins
  with `rdf:reifies` and the triple term functions.
* **Edges from a nested pattern.** The block can hold a graph pattern besides its
  configuration triples: plain triples, groups, `UNION`s or a subquery, and a `FILTER`
  of the block filters it. `path:start` and `path:end` name its variables of an edge's
  ends, which follows QLever's `pathSearch:start` and `pathSearch:end`. The planner
  plans the pattern in the active graph as the leaf's last child, after the input of a
  search with bound ends. The executor evaluates it once and keeps its distinct
  (start, end) pairs in two maps, by start and by end, which the searches read in place
  of the indexes. `path:direction` applies to these edges. Since they have no triple,
  `path:predicate`, `path:edgePredicate`, `path:edge` and `path:weight` are refused with
  a pattern, and so are a pattern without `path:start` and `path:end`, those two
  without a pattern, and an end variable the pattern does not bind.
* **Tests.** `crates/sparkles-core/tests/path_search.rs` gained a test of the three
  features and their errors, a test of edges that are a join in the block with a
  `FILTER`, and a differential test on random graphs. It compares a search with
  `ex:p0` forward and `ex:p1` backward, and the same search over the pattern
  `{ ?x ex:p0 ?y } UNION { ?y ex:p1 ?x }`, with paths enumerated by brute force.
* **Performance.** On the 10.5M-triple benchmark data, with 21 to 31 interleaved runs
  per query against the build before the change, while other builds kept the load
  average between 39 and 49 on 16 cores, the searches over predicates did not change.
  The shortest path 17 edges long took a median of 7.5 ms and 6.1 ms of server CPU,
  against 8.9 ms and 7.1 ms. The 5 shortest paths took 134 ms and 119 ms of CPU,
  against 146 ms and 128 ms, and the paths to everyone 693 ms and 476 ms of CPU,
  against 737 ms and 474 ms. A search with `foaf:knows` forward and `ex:advisor`
  backward took 12 ms. The same shortest path over the nested pattern
  `?x foaf:knows ?y` took 1.4 s, because the pattern's 2.6 million edges are read and
  kept in maps first, so a pattern is for edges that predicates cannot describe.
