# G10: Jena's SERVICE enhancer

> **Status:** implemented
>
> **Phases:** One phase, shipped on 2026-10-03: the options `loop`, `bulk` and `cache` in
> front of a SERVICE IRI, `urn:x-arq:self`, bulk requests as VALUES blocks or as Jena's
> UNION form, and a per-dataset cache of remote results scoped to the caller. Roadmap
> item 38 added the slice-aware cache, a time to live, a disk tier, `se:cacheLs` and
> `se:cacheRm`, and `parallel+n` on 2026-11-28
> ([Outcome](#slices-functions-disk-and-parallel-requests)).
>
> **User docs:** [API: SERVICE options](../API.md#service-options-loop-bulk-and-cache) ·
> [Usage: correlated, bulk and cached SERVICE](../USAGE.md#correlated-bulk-and-cached-service) ·
> [Features](../querying/README.md#sparql-queries-and-updates) ·
> [Comparison with Jena](../COMPARISON.md#vs-apache-jena--fuseki)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This spec builds on `LATERAL` and its operator ([G06 §3](G06-arq-query-extensions.md)),
on the outbound policy and its per-request budgets, on the result cache of each dataset,
and on the caller's identity and graph view from access control
([C09](C09-dataset-access-control.md), [C12](C12-graph-access-control.md)).

## 1. Summary, goals, non-goals

Jena's `jena-serviceenhancer` module changes how a SERVICE clause calls its endpoint.
Options written at the front of the SERVICE IRI turn a SERVICE into a correlated join
that runs once per input solution (`loop`), group many inputs into one request (`bulk`)
and cache what an endpoint returned for each input (`cache`). A SERVICE without an
endpoint, or with `urn:x-arq:self`, reads the dataset the query runs on.

```sparql
SELECT ?s ?l {
  VALUES ?s { wd:Q1686799 wd:Q54837 wd:Q54872 wd:Q54871 wd:Q108379795 }
  SERVICE <cache:loop:bulk+5:https://query.wikidata.org/sparql> {
    SELECT ?l { ?s rdfs:label ?l FILTER(langMatches(lang(?l), 'en')) } ORDER BY ?l LIMIT 1
  }
}
```

In this query each of the five items gets its own first English label, all five go to
Wikidata in one request, and a second run of the query sends nothing.

Goals:
- Parse Jena's option syntax, so that queries written for a Fuseki with the module run
  unchanged.
- Run `loop` as a correlated join on the existing `Lateral` operator, with Jena's
  substitution rules.
- Send `bulk` requests in a shape that Fuseki, QLever, Wikidata's Blazegraph and any
  SPARQL 1.1 endpoint answer, and give each input exactly the solutions it would get
  from a request of its own, even from an endpoint that cuts responses short.
- Cache remote results per input in a bounded, memory-budgeted cache, without ever
  serving one caller's entries to another caller, and with controls to clear it.
- Keep every request under the outbound policy and the request's budgets, and keep the
  `federate` permission in force for cached results.

Non-goals:
- Jena's slice-aware cache, which serves overlapping LIMIT and OFFSET ranges from
  stored pages and learns each endpoint's result size limit (§10).
- The management functions `se:cacheRm` and `se:cacheLs` (§10).
- A disk-backed cache. Jena ships only an in-memory one too.
- Cross-dataset SERVICE on one server, which is a separate roadmap item. Only the
  dataset of the query itself is reachable without HTTP.

## 2. What Jena does

The module's documentation, its sources and its tests describe the following behaviour.

**Options.** A SERVICE IRI is read as `(key[+value]:)* (key[+value][:] | IRI)`. The keys
`loop`, `bulk`, `cache` and `optimize` are options, and the first segment that is not an
option starts the endpoint's IRI. `SERVICE <loop:bulk+10:https://…>` therefore has two
options and the endpoint `https://…`. When no IRI follows, the endpoint is
`urn:x-arq:self`, the active dataset. When a SERVICE has only options and its pattern is
one SERVICE, the options apply to that inner SERVICE. The first value of a repeated key
wins.

**`loop`.** An algebra transform turns `join(L, service <loop:…>)` into a sequence, which
evaluates the SERVICE once per solution of `L` with the solution's values substituted,
and turns a left join into a conditional, its OPTIONAL counterpart. Unlike `LATERAL`, the
substitution ignores sub-select scope. Any variable of the SERVICE pattern with the name
of a visible variable of `L` is joined, so in
`BIND(rdf:type AS ?p) SERVICE <loop:> { SELECT (COUNT(*) AS ?c) { ?s ?p ?o } }` the count
is per `?p`. Jena's documentation recommends `LATERAL` for local correlated joins and
`loop` for bulk requests.

**`bulk`.** `bulk` groups `serviceBulkBindingCount` inputs (default 10) into one request,
and `bulk+n` groups `n`, capped at `serviceBulkMaxBindingCount` (default 100). The
request is a UNION of the pattern substituted with each input, each member binding
`?__idx__` to the input's position, plus a member that binds only an end marker,
`?__idx__ = 1000000000`. The whole is ordered by `?__idx__`, so the marker comes last,
and a response without it was cut short by the endpoint. Jena then learns that limit
and asks again for what is missing.

**`cache`.** `cache` and `cache+default` read the cache and write to it, `cache+clear`
drops the entries of the current inputs, and `cache+off` bypasses it. An entry's key is
the service IRI, the pattern and the input binding projected to the join variables, so
inputs are cached one by one even in bulk requests. The cache keeps pages of bindings,
300 entries of at most 15 pages of 10,000 bindings by default, and it is global unless a
dataset configures its own. There is no notion of a caller.

**Management.** With `enableMgmt` set in the context, `se:cacheRm()` drops every entry,
`se:cacheRm(id)` one entry, and the property function `se:cacheLs` lists entries.

## 3. Syntax

No grammar changes. Every option form is an absolute IRI whose scheme is `loop`, `bulk`,
`cache`, `optimize` or one of these with a `+value`, so the vendored spargebra already
parses it. Such an IRI was never a valid SERVICE endpoint, because only `http` and
`https` pass the outbound policy, so no query that ran before changes meaning. The
options therefore apply in both syntax modes.

The planner reads the options from the IRI with Jena's rules (§2), including the merge
with a SERVICE directly inside. `bulk+x` with `x` not a positive integer, and `cache+x`
with `x` other than `default`, `clear` and `off`, are errors that name the option.
`optimize` is accepted and has no effect. A SERVICE with a variable endpoint has no
options.

## 4. `loop`

A group element `SERVICE <loop:…> { R }` with patterns `L` before it in the group plans as
`Lateral(L, R)`. An `OPTIONAL { SERVICE <loop:…> { R } }` plans as the optional mode of
the same operator, which keeps the inputs without solutions and applies the OPTIONAL's
filter per input. Jena's transform applies to joins and left joins only, and so does
this one. A loop SERVICE with nothing before it runs once, as a plain SERVICE does.

The keys of the loop are the variables of `L` that `R` mentions anywhere, inside
sub-selects, expressions and `EXISTS` included. That is Jena's join key. Substitution
puts an input's values in place of the keys everywhere in `R`, with three exceptions:
- a variable that `R` assigns (`BIND`, `VALUES`, an aggregate, ARQ's `LET` and `UNFOLD`)
  is not replaced, because a value cannot stand where a variable is assigned;
- a literal does not replace a variable in the subject or predicate position of a triple
  pattern, which could not match anyway;
- a blank node is never sent, because a blank node in a query is a variable.

A key that is not replaced stays in the request, and the join with the input compares
its value. Since remote blank nodes are new nodes of the query, an input whose key is a
blank node joins nothing, which is also what a substituted request would return.

`GROUP BY ?k` with `?k` substituted keeps the variable and binds it with a `BIND` under
the grouping, so an empty group stays empty. A projected key stays in the projection and
comes back unbound.

**Over the dataset itself** (`SERVICE <loop:>` or `<loop:urn:x-arq:self>`), the operator
is G06's `Lateral` with one switch: the substitution also reaches the variables of
sub-selects that do not project them. `R` reads the query's default graph and the
caller's view, as a separate query on the same dataset would.

**Over a remote endpoint**, the operator's right side is not planned locally. A loop
groups `L`'s rows by their values of the keys, turns each distinct group into an input,
removes duplicate inputs, reads the cache, and sends the rest in requests of the bulk
size. It then joins each group of rows with its input's solutions.

The operator checks the deadline and cancellation between requests, and the request's
row and memory budgets as it joins. EXPLAIN shows the endpoint, the keys, the bulk size
and form and the cache mode, and after execution the numbers of inputs, requests, cache
hits and retried requests.

The same substitution fixes a gap of G06. A plain SERVICE inside a `LATERAL` group that
runs per row, or under initial bindings, was sent as written. It is now sent with the
values in force, respecting the sub-select scope of `LATERAL`.

## 5. Bulk requests

A request with one input is the pattern with that input's values,
`SELECT * WHERE { R[μ] }`, exactly what a loop without `bulk` sends. A request with
several inputs takes one of two forms.

**The VALUES form** is used when joining `R` with the inputs gives each input the same
solutions as substitution. That holds when `R` is built from basic graph patterns, paths
that cannot match a zero-length path, joins, `GRAPH` and deterministic FILTERs, and no
key is used by a FILTER over a pattern that may leave it unbound. This is G06's test for
when `LATERAL` equals a join.

```sparql
SELECT * WHERE {
  { VALUES (?d ?__idx__) { (<urn:dept1> 0) (<urn:dept2> 1) } { R } }
  UNION { BIND(1000000000 AS ?__idx__) }
} ORDER BY ?__idx__
```

The pattern is written once, so the request grows by one row per input, and every SPARQL
1.1 endpoint evaluates VALUES. A key that an input leaves unbound is `UNDEF`.

**The UNION form** is Jena's, for any other pattern, such as a sub-select with ORDER BY
and LIMIT that must run per input:

```sparql
SELECT * WHERE {
  { { R[μ0] } BIND(0 AS ?__idx__) } UNION { { R[μ1] } BIND(1 AS ?__idx__) }
  UNION { BIND(1000000000 AS ?__idx__) }
} ORDER BY ?__idx__
```

In both forms, each solution carries the index of its input, and the marker sorts last.
If `R` already uses `?__idx__`, the name gets a number. A response without the marker
was cut short. Its inputs are then sent again one by one, so each gets what its own
request returns, and the cut response is not cached. Jena instead learns the endpoint's
limit and fetches the rest of each input with OFFSET, which needs the slice-aware cache
of §10.

The bulk size is `bulk+n`, or the server's `--service-bulk-size` (10) for `bulk`, capped
at `--service-bulk-max` (100). These are Jena's defaults. Library users set
`OutboundPolicy::service_bulk_size` and `service_bulk_max`.

## 6. The cache

Each dataset keeps a cache of remote results next to its result cache, bounded by
`--service-cache-mb` (64 MiB, 0 turns it off). An entry holds the solutions of one input
as terms, because remote terms have no ids in the store's dictionary. Its size is
estimated from the terms' lengths, and an entry larger than an eighth of the budget is
not kept.

The key is the caller's scope, the endpoint's URL, the SERVICE pattern as written, and
the values substituted into it. The scope is new. The server sets it to the caller's
principal and the endpoint the request came through, so callers with different
credentials, grants or graph views never read each other's entries. Without the scope,
one caller could learn which values another caller's query had looked up from the
timing of a cache hit. Credentials in the endpoint's URL are part of the key too. A
library user without a scope shares one.

`cache` and `cache+default` read and write, `cache+clear` drops the inputs' entries and
writes what it fetches, and `cache+off` neither reads nor writes. A request with the
`no_cache` option skips the cache as it skips the result cache. A SERVICE with `cache`
and without `loop` has one input, so its whole result is one entry.

A caller without `federate`, or a server with SERVICE turned off, is refused before the
cache is read, so a cached entry never answers a request that could not have made the
call. `POST /$/cache/clear/{ds}` empties the cache with the result cache, and the
dataset's statistics report its entries, bytes, capacity, hits and misses.

On `urn:x-arq:self`, `cache` and `bulk` are accepted and have no effect. Local results
are cached by the result cache, which is keyed by commit and view and is never stale.

## 7. `urn:x-arq:self`

`SERVICE <urn:x-arq:self> { R }`, and a SERVICE whose IRI has options and no endpoint,
evaluate `R` against the dataset of the query, in its default graph, under the caller's
view. They need neither the `federate` permission nor `--allow-service`, because no
request leaves the server.

## 8. Code layout

- `sparql/enhancer.rs` parses the options, substitutes values into a pattern, writes
  the requests, plans remote loops and runs them.
- `sparql/svccache.rs` holds the cache of remote results.
- `sparql/lateral.rs` gains the unscoped mode and the remote mode of `LateralSpec`.
- `sparql/exec.rs` splits the HTTP part of SERVICE into `fetch`, used by plain SERVICE,
  cached SERVICE and loops.
- `QueryOptions::service_scope`, `StoreOptions::service_cache_bytes`, and the server's
  flags `--service-cache-mb`, `--service-bulk-size` and `--service-bulk-max`.

## 9. Acceptance examples

Against an endpoint that the test runs in process over Jena's test data (department `d`
of `n` employs persons 1 to `n - d + 1`):

- **A1.** Jena's `testLoopJoinWithScope`. A loop over a sub-select with
  `ORDER BY DESC(?o) LIMIT 1` gives each of 9 departments its own top employee. With
  `bulk+5` it sends 2 requests in the UNION form, and over `urn:x-arq:self` it sends
  none. Without `loop`, every department gets person 9, as in `testStdJoinWithScope`.
- **A2.** Jena's `testScopeSimple`. `SERVICE <loop:>` counts the `rdf:type` triples
  through a sub-select that does not project `?p`, and `LATERAL` counts every triple.
- **A3.** A pattern of a triple and a FILTER goes out in the VALUES form, 4 inputs per
  request, with the same solutions as a plain SERVICE. `bulk` alone takes the configured
  size, and `bulk+100` is capped.
- **A4.** Jena's `AbstractTestServiceEnhancerResultSetLimits`. An endpoint that stops
  after 1, 2 or 10 rows gives 4, 7 and 10 solutions with `loop:`, `loop:bulk+5:` and
  `loop:bulk+5:cache:`, in ascending and descending order.
- **A5.** A repeated cached query sends nothing. Fewer inputs read the cache, and one
  more input sends one request with only that input. `cache+clear`, `cache+off`,
  `no_cache`, another scope and a cleared cache each fetch again. A caller without
  `federate` is refused with a warm cache.
- **A6.** An OPTIONAL loop keeps the inputs without solutions. A failing endpoint fails
  the query, and with SILENT keeps each input once.
- **A7.** A plain SERVICE in a per-row `LATERAL`, and under initial bindings, is sent
  with the values.
- **A8.** The W3C SPARQL suites still pass 482/328/157/269, with Jena's CDT and property
  function suites at their counts.

## 10. Rejected alternatives

- **Jena's slice-aware cache and result-size learning.** It saves requests when queries
  differ only in LIMIT and OFFSET, at the cost of a paging layer and of requests that
  rewrite the user's slices. Re-sending a cut bulk request per input gives the same
  answers with far less machinery.
- **`se:cacheRm` and `se:cacheLs`.** Functions with side effects inside read queries
  would let any reader clear or list a shared cache. The admin route and the statistics
  cover the same needs under `admin`.
- **One cache per server, shared by callers.** It would raise hit rates, and it would
  leak which values other callers looked up. The scope keeps the sharing within a
  caller.
- **Only the UNION form.** It repeats the pattern once per input, so requests grow with
  the pattern's size, and some endpoints plan large unions poorly. VALUES is the shape
  that federated engines send, and Wikidata and QLever handle it well.
- **Caching `urn:x-arq:self`.** The result cache already does it correctly per commit.
- **Requests in parallel.** One request at a time keeps the per-request budget simple
  and the endpoint's load predictable. Bulk requests remove most of the round trips.

## 11. Sources

- Apache Jena's `jena-serviceenhancer` (Apache-2.0): `ServiceOpts`,
  `TransformSE_JoinStrategy`, `TransformSE_EffectiveOptions`, `BatchQueryRewriter`,
  `ServiceEnhancerConstants` and the assembler vocabulary, read for syntax and
  behaviour, and its tests `TestServiceEnhancerMisc`,
  `AbstractTestServiceEnhancerResultSetLimits` and
  `TestServiceEnhancerBatchQueryRewriter`. No code was copied.
- Jena's documentation page of the module (`documentation/query/service_enhancer.md` in
  `jena-site`).
- SPARQL 1.1 Query and SPARQL 1.1 Federated Query.
- The Sparkles code: G06's `Lateral` operator and join test, the outbound policy, the
  result cache and access control.

## Provenance

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

- **Spec:** [`G10-service-enhancer.md`](G10-service-enhancer.md), written on 2026-10-03
  independently from:
  - the Sparkles code: the `Lateral` operator and join test of G06, the SERVICE executor,
    the outbound policy, the result cache and access control;
  - Apache Jena's `jena-serviceenhancer` sources (Apache-2.0): `ServiceOpts`,
    `TransformSE_JoinStrategy`, `TransformSE_EffectiveOptions`, `BatchQueryRewriter`,
    `ServiceEnhancerConstants` and the assembler vocabulary, read for syntax and
    behaviour. No code was copied;
  - the module's tests `TestServiceEnhancerMisc`,
    `AbstractTestServiceEnhancerResultSetLimits` and
    `TestServiceEnhancerBatchQueryRewriter`, whose data and cases are ported with
    citations to `crates/sparkles-core/tests/service_enhancer.rs`;
  - the module's documentation page in `jena-site`
    (`source/documentation/query/service_enhancer.md`);
  - SPARQL 1.1 Query and SPARQL 1.1 Federated Query.
- **Implementation** (2026-10-03): from the spec and the Sparkles code. The cache uses
  `quick_cache`, already a dependency. **Dependencies:** none new.
- **Implementation of roadmap item 38** (2026-11-28): from the follow-up sketches, the
  Sparkles code and the module's documentation page, which gives the `se:` namespace,
  the signatures of `se:cacheLs` and `se:cacheRm` and the `enableMgmt` context symbol.
  No Jena code was read or copied for it. **Dependencies:** none new. `sha2` names and
  seals the disk files.
- **Rejected** (spec §10):
  - Jena's slice-aware cache and result-size learning;
  - the management functions `se:cacheRm` and `se:cacheLs`;
  - one cache per server shared by callers;
  - the UNION form for every pattern;
  - caching `urn:x-arq:self`;
  - requests in parallel.

## Outcome

**Delivered.** The design landed on 2026-10-03 as §3 to §8 describe it:
- `sparql/enhancer.rs` with the option parser, the merge with an inner SERVICE, the
  substitution of values into the algebra, both bulk forms, the planning of remote loops
  and their execution;
- `sparql/svccache.rs`, held by each dataset's `ResultCache` as its `service` member, so
  that every snapshot of the dataset shares it;
- the unscoped and remote modes of `LateralSpec`, the planner's rewrite of joins and
  left joins whose right side is a loop SERVICE, and the substitution of the planner's
  constants into plain SERVICE requests;
- `QueryOptions::service_scope`, which the server sets from the principal and the
  endpoint in the same place it sets the graph view;
- `OutboundPolicy::service_bulk_size` and `service_bulk_max`, and the flags
  `--service-bulk-size`, `--service-bulk-max` and `--service-cache-mb`;
- `serviceCache` in the dataset statistics, and `serviceCleared` and `serviceBytes` in
  the answer of `POST /$/cache/clear/{ds}`.

**Deviations and decisions.**
- The bulk size settings live on `OutboundPolicy`, which every query path already
  carries, rather than in a new options struct. The local `query` and `update` commands
  take the flags too, because they share the `--outbound-*` arguments.
- Without `bulk`, a loop sends one request per distinct input. Inputs that differ only
  in blank nodes share one request, since blank nodes are not sent.
- The cache's statistics are in the JSON statistics only. The Prometheus and
  OpenTelemetry series of the result cache have no service cache counterpart.

**Tests.** `crates/sparkles-core/tests/service_enhancer.rs` runs an endpoint in process over
a second store and records the queries it receives. Its seven tests cover A1 to A7, and
port `testLoopJoinWithScope`, `testStdJoinWithScope`, `testScopeSimple` and the five
result-limit cases of `AbstractTestServiceEnhancerResultSetLimits` in three modes. Unit tests cover the option
parser, the merge, substitution through sub-selects, groups, filters and assigned
variables, both request shapes (each parsed back by spargebra), the split of bulk
responses and the cache's bounds and keys.

**Measurements.** The ignored test `measure_bulk_and_cache` runs two correlated queries
over Jena's test data with 200 departments (20,100 employments), against the in-process
endpoint, in a release build, with the machine at a load average of about 45 from other
builds. The times are the best of three runs.

| Query | Requests | 0 ms per request | 5 ms per request |
|---|---|---|---|
| Top label per department, `loop:` | 200 | 467 ms | 1,492 ms |
| Same, `loop:bulk+10:` (UNION form) | 20 | 73 ms | 163 ms |
| Same, `loop:bulk+100:` | 2 | 17 ms | 24 ms |
| Same, `loop:bulk+100:cache:`, warm | 0 | 0.5 ms | 0.5 ms |
| Employees per department, plain SERVICE | 1 | 90 ms | 74 ms |
| Same, `loop:` | 200 | 1,136 ms | 1,513 ms |
| Same, `loop:bulk+10:` (VALUES form) | 20 | 419 ms | 269 ms |
| Same, `loop:bulk+100:` | 2 | 197 ms | 124 ms |
| Same, `loop:bulk+100:cache:`, warm | 0 | 20 ms | 19 ms |

The first query has no plain form, because a sub-select with LIMIT gives every input the
same answer without `loop`. Bulk requests cut its cost 27 to 62 times, and a warm cache
answers in half a millisecond. The second query is one a plain SERVICE answers in one
request. With the inputs sent in bulk, it takes 1.7 to 2.2 times the plain request,
because the endpoint sorts the result by `?__idx__` and each row carries its index. Its
warm cache is about 4 times faster than the plain request. The machine's load makes the
differences between the two delay columns noise for the faster rows.

**Gates.** The W3C SPARQL suites pass 482/328/157/269, with Jena's CDT and property
function suites at 642 of 655 and 39 of 48. `mise run ci` passes.

**Not built.** Jena's slice-aware cache and result-size learning, `se:cacheRm` and
`se:cacheLs`, a disk cache, requests in parallel, Prometheus series for the cache, and
cross-dataset SERVICE.

### Slices, functions, disk and parallel requests

**Delivered** on 2026-11-28 for roadmap item 38, from the follow-up sketches below:
- `svccache.rs` keys an entry without the outermost slice of its request. The entry
  (`Entry`) holds the slice of the request that got its solutions, and answers that
  request and every request with a limit whose solutions are all among its rows
  (`Entry::range`). A plain SERVICE records the request text without the slice and the
  slice on its plan node (`ServiceKey`), and a LIMIT that the planner passes into the
  request narrows that slice. The streamed SERVICE stores the response to a request with
  a LIMIT once it holds that many solutions and one more read finds the end of the
  response, even when the query reads no further. A row past the limit or a failure on
  that read leaves the cache as it was, and a SILENT call stores its response only once
  it has read all of it. A
  remote loop splits the slice off its pattern once
  (`enhancer::outer_slice`). A miss sends the request as the query writes it, and a
  response replaces the entry of its request only when the entry cannot answer it and
  holds fewer solutions.
- Every entry carries its scope, endpoint and pattern as lengths into its key
  (`Parts`), so listing and removal compare the caller's scope as a whole and never
  search the key for a separator. The server's scope names the credential with the
  bound of its OIDC scopes (`Principal::access_id`), the endpoint, the dataset with its
  branch, and a SHA-256 of the request's graph view. A view with guards adds the
  SHA-256 of the view as its guards are decided for the request's snapshot, so two
  requests whose guards let different graphs through never share an entry.
- An entry answers for a time to live, `--service-cache-ttl` (3,600 s) or the dataset's
  `ttlSeconds`, in memory and on disk, and an expired entry is removed when it is read.
- `svcdisk.rs` keeps a persistent dataset's entries in `service-cache/` when its
  `service-cache.json` sets `disk`. Files are named by the SHA-256 of the key, written
  under a temporary name and renamed, sealed by the SHA-256 of their body, and evicted
  least recently used first under `diskMb` (default `--service-cache-disk-mb`, 256 MiB).
  The tier's index and files change under one lock, every file carries a generation that
  a read checks after it reads, and the removals of `se:cacheRm`, a clear and turning the
  tier off sync the directory before they return. A removal takes entries off disk, then
  moves a counter that lookups check, and then takes them out of memory, so a lookup
  running beside it does not put a removed entry back. An entry whose file cannot be
  removed stays in memory too and is not counted, and a failed sync fails the query that
  removed. A keyed removal, which `cache+clear` makes, leaves a file of another version
  alone, and an entry too large to write takes the older file of its request away. `formats.rs` registers the files as
  the kind `service-cache` (version 1) and the settings file `service-cache.json`
  (format 1). `Store::set_service_cache_settings`, `service_cache_settings` and
  `service_cache_status` read and change the setting, one change at a time. The setting is
  checked, and a tier is opened where none is in use, before the file is written. A new
  cap applies to the tier in use once the file is written, so no second tier indexes the
  directory, and turning the tier off retires it, after which a query that still holds
  it writes nothing. `StoreOptions::service_cache_files`, which a
  read-only server turns off, leaves the directory untouched.
- `svcfn.rs` implements `se:cacheLs` as a VALUES leaf that the planner builds from the
  caller's entries, and `se:cacheRm` as a function that removes within the caller's scope.
  `QueryOptions::service_cache_mgmt` grants removal, and the server sets it under
  `--service-cache-mgmt` for a caller with `admin` on the dataset through the endpoint
  of the request. Both functions work in the WHERE clause of an update, whose view of
  uncommitted data shares the store's SERVICE cache and keeps no other result. The
  Fuseki converter reports a server-wide `se:enableMgmt true` with that flag, `false` as
  the default, and a dataset's own `true`, or its `false` under a server-wide `true`, as
  unsupported.
- `parallel` and `parallel+n` keep up to n requests of a remote loop in flight, capped by
  `OutboundPolicy::service_parallel_max` (`--service-parallel-max`, 4).
  `RemoteRun::send` sends each wave of requests on scoped threads in the caller's span
  and takes their answers in input order. Each request draws on a share of the outbound
  budget's time (`RequestBudget::share`), which is reserved in the budget until the
  request ends, and admits each solution on a charge of its own as it arrives. A cut
  bulk response is retried on the calling thread with the existing admission of each
  answer.

**Deviations and decisions.**
- An entry never answers past its rows. The sketch let a response with fewer solutions
  than its limit answer every later slice, but an endpoint may cap its responses without
  saying so, and the later slice would then miss solutions that the endpoint has.
- `se:cacheLs` takes Jena's three positions, the endpoint, the pattern and the values,
  and three more, the solutions, the bytes and the age in seconds. The sketch named a
  pattern hash. The pattern text keeps Jena's columns and shows the caller only its own
  entries. The endpoint is listed, and written to disk, without the user information,
  query string and fragment of its URL, parsed as the outbound client parses it before a
  request, so every spelling the client accepts is cleaned alike. A URL that is not HTTP
  or HTTPS is listed as `urn:x-sparkles:unlisted-endpoint`. The disk files hold the
  SHA-256 of the caller's scope, never the scope itself. The query result cache keeps no
  result of a query that calls `se:cacheLs`, and none of a LATERAL or loop over the
  dataset whose pattern calls `se:cacheLs` or `se:cacheRm`, which the planner treats as
  property function and side effect wherever it meets them.
- `se:cacheRm` called by name is refused when the query is planned, so a caller without
  the right gets `403` and not an unbound value. The refusal covers a call in an EXISTS
  pattern that no row reaches, but not one inside a SERVICE that goes to another
  endpoint, which answers it as it likes. A call through `fn:apply` or `afn:eval`
  is an error of the expression that removes nothing, since the planner cannot see its
  function. It is marked as having side effects, so no cache of results or expressions
  answers a call of it.
- The dataset setting is a settings file with a library API. Its HTTP route is roadmap
  item 1339, and the metrics sketched below are roadmap item 1340.
- `parallel` alone takes the cap, as `bulk` alone takes the configured size. The shares
  of a wave split the time the budget has left equally. A wave whose time left gives a
  request no share is not sent and fails the query, as does a request that fails once
  its share's time is spent, under SILENT too, as a request out of the budget's time
  does. The budget's spent and reserved time change together under one lock. A wave of
  one request draws on the budget directly, as every other outbound request does. The budget's bytes need no share, because they are counted
  atomically as responses stream in and bound concurrent requests as they bound one. A
  loop over an in-process endpoint sends one request at a time. The eager loop now
  holds its answers on a memory charge until it has joined them, where it used to admit
  only the joined rows.
- A cancelled query sends no later wave, and every solution and dispatch checks for
  cancellation, but a request already waiting for its response ends only when it
  arrives or the request's share of time passes, as a plain SERVICE does. Roadmap item
  1341 passes cancellation to the outbound client.
- The time to live changes the behaviour of the memory tier, whose entries used to live
  until they were evicted. `--service-cache-ttl 0` keeps that behaviour.
- A lookup admits a slice copied out of an entry, with its variable names, before it
  copies it. A disk read admits the file's bytes before it reads them, more if the file
  grew since it was indexed, and each decoded row with its terms before it allocates
  them, so a file that fits the query's memory while its rows do not is refused before
  the rows exist. A listing admits its own storage before it grows. Decoding allocates at
  most 32 times the largest file the tier keeps, an eighth of its cap. A remote loop
  keeps the answers it takes from the cache charged for as long as it holds them, and
  every fetched solution is admitted before it is copied.
- A read-only server neither reads nor changes `service-cache/`, so its SERVICE cache
  is in memory only.
- Clones and backups carry neither the directory nor `service-cache.json`, so a clone
  starts with an empty cache and the default setting.

**Tests.** `crates/sparkles-core/tests/service_enhancer.rs` adds eighteen tests against the
in-process endpoint. They cover slices answered inside an entry and sent past it, an
endpoint that caps its responses without saying so, a LIMIT pushed into a plain SERVICE
in both execution modes, `se:cacheLs` and `se:cacheRm` across two scopes with and
without the right, `parallel+n` against the requests and the answer of a run one request
at a time with the endpoint's peak of requests in flight, the deadline that stops
further waves, cancellation during a wave, the shared outbound time with and without
SILENT, the memory admission of parallel answers in both modes and of a listing,
`se:cacheRm` through `fn:apply` and `afn:eval`, listings kept out of the query result
cache, listings and removals inside a correlated LATERAL and a loop over the dataset
across two callers, a wave out of time under SILENT, removal in an update, a removal in
an EXISTS refused when the query is planned, a guarded view whose exemptions change, and
a disk tier read back after the store is opened again.
`crates/sparkles-core/tests/service_cache_alloc.rs` counts the reading thread's
allocations to show that a disk entry is refused before it is decoded. Unit tests in `svccache.rs`, `svcdisk.rs`, `store/service_cache.rs`,
`outbound.rs`, `enhancer.rs`, the server's `auth/api.rs` and the Fuseki converter cover
slice service, expiry, scope isolation for scopes that are prefixes of one another or
contain the key's separator, scopes by credential, view and branch, removal through the
request's endpoint, round trips of every term kind, eviction under the cap, torn,
changed, misnamed and unknown-version files, files that cannot be removed, admission
before reads, counts past the end of a file, backups and clones without the tier, a
refused setting, a read-only store, time shares, the `enableMgmt` values and the option
parser. They also cover every spelling of an HTTP URL the outbound client accepts with
credentials, a lookup between the two tiers of a removal, a refresh beside a newer file,
a streamed answer that fails or runs on after its limit, a tier held across a change of
its cap and across turning it off, a settings write that fails, and shares taken by many
threads at once.

**Measurements.** The ignored test `measure_bulk_and_cache` ran at commit `de30fe83` in
a release build on a Boat worker with 8 vCPUs and 16 GiB, at a load average of about 1,
with 200 departments against the in-process endpoint. Boat timings are approximate and
are not compared with the measurements above, which came from another host. The times
are the best of three runs, and every arm returned the same 200 or 20,100 rows.

| Query | Requests | 0 ms per request | 5 ms per request |
|---|---|---|---|
| Top label per department, `loop:` | 200 | 288.2 ms | 1,320.7 ms |
| Same, `loop:parallel+4:` | 200 | 88.6 ms | 349.8 ms |
| Same, `loop:bulk+10:` | 20 | 39.0 ms | 141.1 ms |
| Same, `loop:bulk+10:parallel+4:` | 20 | 11.5 ms | 38.1 ms |
| Employees per department, `loop:` | 200 | 266.5 ms | 1,316.2 ms |
| Same, `loop:parallel+4:` | 200 | 96.2 ms | 345.9 ms |

With 5 ms per request, `parallel+4` cuts the top-label loop 3.8 times, which meets the
acceptance of at least 3 times in the sketch below.

### Roadmap follow-ups (2026-10-12)

**Cache, parallel requests and metrics** (item 38, Tier 4, [ROADMAP](../ROADMAP.md)).
*Not built* above lists these; §10 rejected three of them for the first version. Each
sketch below states how it answers that objection.

* **Slice-aware cache.** §10 rejected Jena's paging layer, and a cut bulk response is
  re-sent per input instead (§5). The narrower form keeps that: a cache entry records
  whether it is complete or holds the first n rows of its request. A later request for
  the same pattern and input with `LIMIT m` is answered from a complete entry, or from a
  partial one with n ≥ m, without a request; `OFFSET o LIMIT m` the same when
  o + m ≤ n. A miss sends the user's request as written, so no slice is rewritten, and
  result-size learning stays out. Keys drop the outer slice of the pattern for this
  lookup and keep it as the entry's bound.
* **Cache functions.** §10 rejected `se:cacheRm` and `se:cacheLs` as side effects that
  any reader could use. They are added as read-only and scoped: `se:cacheLs` lists the
  caller's own entries (the scope already in every key, `svccache.rs`), with endpoint,
  pattern hash, rows, bytes and age, and `se:cacheRm` is accepted only for callers with
  `admin` on the dataset and only with `enableMgmt` set in the query's context as in
  Jena, and removes entries within the caller's scope. Everyone else gets an error,
  not a silent no-op.
* **Disk cache.** The cache is in memory, bounded by `--service-cache-mb`, and lost on
  restart. A dataset can set `serviceCache.disk` in its settings to keep entries in
  `service-cache/` under the dataset's directory: one file per entry holding its solutions as
  length-prefixed term strings, named by the key's hash, with a size cap and least-recently-used eviction. A `ttl` setting (default one
  hour) bounds staleness for both tiers, since a remote endpoint's data changes without
  telling Sparkles. Scope prefixes stay in the key, and the directory is excluded from
  backups and clones.
* **Requests in parallel.** §10 kept one request at a time for the budget and the
  endpoint's load. A `loop:` or bulk plan gains `parallel+n` (default 1, capped by
  `--service-parallel-max`, default 4) that keeps up to n requests in flight to one
  endpoint. Each request reserves its share of the per-request time and bytes before
  it starts, and the shared outbound budget of the query still bounds the total, so the
  budget stays as simple to reason about. Results are merged in input order, so the
  answer does not depend on timing. Acceptance: the "Top label per department, `loop:`"
  row of the measurements with 5 ms per request drops at least 3 times with
  `parallel+4`, with the same answer.
* **Metrics.** The service cache has JSON statistics only. `/$/metrics` gains
  `sparkles_service_cache_hits_total`, `…_misses_total`, `…_entries`, `…_bytes` and
  `…_capacity_bytes` per dataset label, named as the result cache's series in `obs.rs`, and `sparkles_service_requests_total` and
  `sparkles_service_request_seconds` per endpoint host, bounded by
  `--metrics-max-datasets` and a cap of 100 hosts with the rest under `$other`.
  OpenTelemetry follows the same names.

#### Interop and query surfaces workstream

These phases belong to [ROADMAP](../ROADMAP.md) item 465, the interop and query surfaces workstream. Each phase gets its own numbered item when it is ready to start.

**Cross-dataset SERVICE on one server.** Let `SERVICE` name another local dataset and read it in process, under the caller's permissions on the target. G10 calls it a separate roadmap item, but none exists. This is local, not cross-server federation. It depends on item 38.
