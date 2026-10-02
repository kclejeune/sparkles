# C12b: Protections of triples (C12 Phase 2)

> **Status:** implemented
>
> **Phases:** shipped on 2026-10-02 in one phase, with all three tiers: protections by
> predicate, by the class of the subject, and by a SPARQL pattern with the caller bound.
>
> **User docs:** [API: Protections of triples](../API.md#protections-of-triples) ·
> [Usage: Hiding some triples from some users](../USAGE.md#hiding-some-triples-from-some-users) ·
> [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This spec is Phase 2 of [C12](C12-graph-access-control.md). C12 lets an operator limit a
grant to some named graphs and endpoints of a dataset. Its non-goals excluded rules finer
than a graph and rules that depend on data. This spec adds both. It was written clean-room
from the documentation and source of Apache Jena's retired `jena-permissions` module, the
W3C Solid access control specifications, W3C ODRL, OASIS XACML, and the public
documentation of AllegroGraph's security filters and MarkLogic's element-level security.

## 1. Summary

**Before this feature**, the smallest unit of access is a named graph. A dataset that
keeps salaries next to names, or patient records next to staff records, has to move the
sensitive triples into graphs of their own before some users can be kept from them.

**This spec adds protections.** A protection names some triples of a dataset in three
ways, which may be combined:

1. **By predicate.** `predicates = ["http://ex/salary"]` covers every salary triple.
2. **By the class of the subject.** `classes = ["http://ex/Patient"]` covers every triple
   whose subject is an instance of `ex:Patient` or of one of its subclasses.
3. **By a pattern with the caller bound.** `pattern = "?s ex:owner ?user"` lets a covered
   triple through when the pattern matches its subject with `?user` bound to the caller.

A protected triple is hidden from every caller unless one of the caller's grants lifts the
protection, or its pattern lets the triple through. Grants lift a protection by name, in
the graphs they cover and at their level, so a `read` grant lets its holder read the
triples and a `write` grant lets it write them too.

**Without protections, nothing changes.** A dataset no protection names, and a caller
whose grants lift every protection of a dataset, run the same plans with the same caches
and speed as before.

### 1.1 Threat model

C09 §1.1 and C12 §1.1 still hold. This spec adds one adversary.

| Adversary | Can | Must not be able to |
|---|---|---|
| A principal with protections in force on a dataset | Query, read and write the triples its grants and the patterns leave it, through every endpoint its grants allow. | Read a protected triple, count protected triples, find one through a search, a path, an aggregate, a plan or a statistic, learn whether a given protected triple exists, or insert or delete a protected triple. |

The guarantee is about answers. It is weaker than C12's existence guarantee in one
documented way. Whether a triple is protected by class or by pattern depends on data, so
a caller can learn that a subject it already knows is protected, because its triples are
missing from answers and writes to it are refused (§9). Out of scope, as in C12:

- timing side channels;
- the shapes of plans, whose estimates come from statistics of every triple (their
  numbers are removed, §8.3);
- full-text scores, which use term statistics of the whole index;
- the commit sequence;
- the configuration itself.

### 1.2 Goals and non-goals

**Goals**

- Protections are set by the operator per dataset, and grants lift them. Grants stay a
  union, and protections act like deny rules that every matching one must pass. Neither
  depends on the order of the rules.
- The engine enforces the rules, not the HTTP handlers, as in C12. Every operator of a
  query, every search index and every statistic sees only the visible triples.
- Rules are compiled into sets once per commit and view. No rule is ever evaluated per
  triple during a query.
- A caller with full access pays nothing.
- Writes are refused by the triples asked for, never by the triples that exist.

**Non-goals**

- Hiding the existence of IRIs. A protection hides triples. An IRI that is the object of
  a visible triple stays visible there, as in MarkLogic's and AllegroGraph's models.
- Protections that depend on the object's class or on graph contents other than through a
  pattern.
- Inference per caller. Materialized inferences are hidden wholesale (§7).
- Protections in the token scopes of C09 §2.3. A token sees its owner's view.
- Protections on `admin`. Admin operations act on the whole dataset, so `admin` lifts
  every protection, as it covers every graph in C12.

## 2. Model

### 2.1 Protections

| Field | Meaning |
|---|---|
| `name` | The name grants lift it by. |
| `dataset` | A dataset name or `*` pattern. |
| `predicates` | Optional. Predicate IRIs, and IRI patterns with `*`. Absent means every predicate. |
| `classes` | Optional. Subject classes. Absent means every subject. |
| `subclasses` | Whether instances of subclasses count, through `rdfs:subClassOf` in any graph. Default `true`. |
| `graphs` | Optional. The graphs it applies in, written as in C12 §2.1. Absent means every graph. |
| `pattern` | Optional. A SPARQL group graph pattern that lets a covered triple through (§2.3). |
| `prefixes` | Prefixes of the pattern. |
| `hide_inferences` | Whether a caller it applies to loses the inferred graph (§7). Default `true`. |

A protection **covers** a quad `(s, p, o, g)` when `p` is in its predicates, `g` in its
graphs, and `s` an instance of one of its classes. A field that is absent does not
narrow the scope, so a protection with only a pattern covers every triple of the dataset.

**Classes.** A subject is an instance of class `C` when the dataset holds
`s rdf:type C'` in any graph, where `C'` is `C` or, with `subclasses`, any class below
`C` through `rdfs:subClassOf` in any graph. The closure is computed from the stored
triples at the commit read, and the materialized inferences count as stored triples.
Following subclasses is the default because a protection of `ex:Patient` is meant to
cover `ex:InPatient` as well, and relying on materialized types would leave a new
subclass instance unprotected until the reasoner runs. A rule that should cover exactly
one class sets `subclasses = false`. OWL equivalences and other entailments are not
followed.

### 2.2 Lifting

A restricted grant of C12 gains a list `lifts` of protection names. A grant lifts the
protections it names in the graphs it covers, through the endpoints it applies to, at its
level. A `write` grant also counts at `read`, as in C12.

```
lifted_read(p, N, E, P)  = ⋃ { graphs(g) | g ∈ G(p, N, E), level(g) ≥ read,  P ∈ lifts(g) }
lifted_write(p, N, E, P) = ⋃ { graphs(g) | g ∈ G(p, N, E), level(g) ≥ write, P ∈ lifts(g) }
```

An entry of C09's `datasets` maps lifts nothing. `admin` on the dataset (or
`server-admin`) lifts every protection everywhere.

### 2.3 Patterns

A pattern is a SPARQL group graph pattern. Its variables `?s` (or `?this`), `?p` and `?o`
stand for the covered triple's subject, predicate and object, and the reserved variables
stand for the caller:

| Variable | Value |
|---|---|
| `?user` | The caller's name as a string: the user, the OIDC or proxy account, the owner of a minted token, or a static token's name. Unbound for anonymous callers. |
| `?role` | Each role the caller holds, one solution per role. |
| `?group` | Each group of an OIDC or proxy identity. |

A pattern lets a covered triple through when it has a solution whose `?s`, `?p` and `?o`
equal the triple's, for those of the three it uses. A pattern that uses none of them, such
as `ex:config ex:open true`, lets every covered triple through or none. A pattern that
uses a caller variable the caller lacks has no solution. The pattern is matched against
the union of every graph of the dataset as its default graph, and `GRAPH` reaches the
named graphs. Other claims of an identity are not kept after sign-in, so they are not
available.

### 2.4 Evaluation

For a principal `p`, a dataset `N` and an endpoint `E`, with C12's read graphs `R`:

```
visible(q)  = graph(q) ∈ R
              ∧ ∀ P covering q:  graph(q) ∈ lifted_read(p, N, E, P)  ∨  pattern_P(q, p)
writable(q) = C12's writable(graph(q))
              ∧ ∀ P covering q:  graph(q) ∈ lifted_write(p, N, E, P) ∨  pattern_P(q, p)
```

So:

- **Default.** A triple no protection covers is governed by C12 alone.
- **Combination.** Every protection that covers a triple must be passed. This is XACML's
  deny-overrides for protections, while the grants that lift them stay a union. A salary
  of a patient stays hidden from a caller who may read salaries but not patients.
- **Order.** Neither protections nor grants depend on their order.
- **No deny in grants.** A grant only ever adds. C09 §14's argument against deny rules
  was that they make evaluation order-dependent, which protections are not.

A protection is **in force** for a caller when some graph does not lift it. A view with no
protection in force is a C12 view, and a view that is all graphs with no protection in
force is no view at all.

## 3. Configuration

### 3.1 Syntax

```toml
[[protections]]
name = "salaries"
dataset = "hr"
predicates = ["http://example.org/salary"]

[[protections]]
name = "patients"
dataset = "clinic-*"
classes = ["http://example.org/Patient"]

[[protections]]
name = "own-documents"
dataset = "docs"
classes = ["http://example.org/Document"]
pattern = "?s ex:owner ?user"
prefixes = { ex = "http://example.org/" }

[protection_limits]
max_hidden_quads = 5000000
max_pattern_rows = 1000000

[[roles.hr.grants]]
dataset = "hr"
level = "write"
lifts = ["salaries"]
```

### 3.2 Validation

Startup and reload refuse a configuration when a protection has an invalid or duplicate
name, an invalid dataset pattern, an empty list, a predicate that is not an absolute IRI
(with `*`), a class that is not an absolute IRI, an invalid graph name, a prefix that is
not an absolute IRI, or a pattern that does not parse with every caller variable bound.
They refuse a grant that lifts an unknown protection, and limits of 0. The warning of
C12 §3.2 about restricted grants without effect leaves out grants that lift protections.

### 3.3 Reload

Protections and lifts are part of the policy and reload with it, as in C12 §3.3.

## 4. The masked snapshot

The engine applies protections where it applies C12's view: to the snapshot a request
reads. For a view with protections in force for reads, the engine works out the quads it
hides at the snapshot's commit, as sets:

1. **Predicates.** The predicate IRIs, and those of the snapshot's predicates that match a
   pattern, become ids. Their quads are read from the PSO index.
2. **Classes.** The class closure is read from the POS index (`rdfs:subClassOf` by
   object), then the members (`rdf:type` by class). The members' quads are read from the
   SPO index, keeping those of the protected predicates.
3. **Patterns.** The pattern runs once as a SPARQL query, with the caller's values bound
   by `VALUES`, and yields the set of `(s)`, `(s, p, o)` or other keys it lets through.
4. **Graphs.** A quad of a graph the view does not read needs no mask. A graph that lifts
   the protection keeps its quads.

The hidden quads are then applied to the snapshot as deletions: those of the base index go
into the delta's deletions, and those the delta inserted leave its insertions. The result
is an ordinary state of the store. Every scan merges it, every exact count and statistics
correction accounts for it, and the spatial and vector indexes already drop deleted quads.
No query operator knows about protections. Full-text hits are checked against the mask,
because the full-text index of a commit holds documents for every quad of that commit.

**Caching.** The masked snapshot is kept with the snapshot, keyed by the view's read
graphs and its protections in force. The caller is part of the key only when a pattern may
use it. A second request at the same commit with the same key reuses it, and two callers
with the same key share it. A new commit builds it again on first use.

**Budgets.** `max_hidden_quads` bounds the hidden quads of one view at one commit, and
`max_pattern_rows` the solutions of one pattern. Past either, the request fails with a
budget error (`hidden-quads` or `rows`), never with a partial view. The cost is linear in
the quads hidden and the pattern's solutions, and does not grow with the queries run.

## 5. Reads

Every read path reads the masked snapshot. The paths and what they show:

| Path | Through the mask |
|---|---|
| SPARQL queries: patterns, `GRAPH`, `FROM`, paths, `EXISTS`, `MINUS`, subqueries, aggregates, `DESCRIBE`, `CONSTRUCT`, `ASK` | The visible triples. `ASK` of a hidden triple answers like a missing one. |
| `COUNT(*)` and grouped counts answered from the index or the statistics | Exact for the visible triples. The statistics shortcuts correct for the mask as for any delta, or fall back to scans when the correction costs more. |
| Characteristic sets, sampled filters and other estimates | Taken from every triple. They choose plans and never answers, and plans lose their numbers (§8.3). |
| `text:query`, `/{ds}/text`, `spk:hybridSearch` | Hits of visible triples. Limits are filled from them. Scores use the whole index. |
| `spk:vectorSearch`, spatial filters, spatial joins, nearest neighbours, `/{ds}/geo` | Visible triples, through the deletions the indexes already honour. |
| Graph Store reads and the whole-dataset export | The visible triples. A graph whose every quad is hidden answers like a missing graph. |
| `/{ds}/explain`, plans in results | Planned on the masked snapshot, redacted as in C12 §7.3. |
| RDFS on read | The closure of the visible triples. A schema graph's hidden triples are not read. |
| Schema reports, VoID, drafted shapes | Computed over the visible triples. |
| `/{ds}/diff` | The difference of the two masked states, so a triple that became hidden counts as removed. |
| `/{ds}/changes` | With protections by predicate and graph only, each change is filtered by its quad. With protections by class or pattern the feed answers 403, because it would need the view of every commit. The diff covers that case. |
| Stored queries | As SPARQL queries. |
| MCP tools | As the HTTP routes their tools stand for. |
| Service Description | Counts are left out, as for every limited view of C12. |
| `/$/datasets/{ds}` and MCP listings | `quads` counts the visible triples. |
| Routes C12 §5.4 refuses for a limited view (`/$/stats`, index status, reasoning diagnostics, backup listings, history settings) | Refused the same way, since a view with protections in force is limited. |
| Validation endpoints | Refused, as in C12 §5.5. |
| `whoami` | `restricted.{ds}.triples` is `true`. Protection names are never listed. |

## 6. Writes

Each quad a write asks to insert or delete is checked against the protections, both at
the state the write starts from and at the state it leaves, before the commit. The quads
asked for are checked, not the changes that took effect, so a delete of a protected triple
fails in the same way whether or not it exists. A refused write changes nothing and
answers 403 `write access to the triple <s> <p> <o> required`. The message names the
triple the caller sent and never the protection.

- **Both states.** Checking the state before keeps a caller from deleting a protected
  triple. Checking the state after keeps a caller from creating one, such as a new
  patient, or from inserting a triple into a document that becomes someone else's.
- **The class hierarchy.** A class protection also covers `rdfs:subClassOf` triples
  whose subject or object is in its class closure, in any graph. Without that, a caller
  could unprotect every instance of a subclass by deleting one schema triple.
- **WHERE clauses** of updates read the masked view, so `DELETE WHERE` never matches a
  hidden triple.
- **CLEAR, DROP and Graph Store `PUT`** act on the triples the view sees. Hidden triples
  stay. Replacing the whole dataset is refused when protections are in force.
- **`DELETE DATA` of terms the store lacks** is checked too, by the predicate's IRI and
  the subject's classes.
- **Dry runs** are refused like the writes they preview.
- **Bulk loads** by a caller with protections in force go through the transaction, so
  that each quad is checked.

A pattern is only as strong as the write protection of the triples it reads. A pattern
`?s ex:project ?pr . ?pr ex:member ?user` lets anyone who may write `ex:member` grant
themselves access. The operator protects such triples for writing too.

## 7. Inference

Materialized inferences live in `urn:x-sparkles:inferred` and may restate hidden facts in
other words. An inference that is visible only when all its premises are visible would
need the derivation of every inferred triple, which the reasoner does not keep, and
inferring per caller would repeat the reasoning per view and commit. A structural rule,
such as hiding inferred triples about subjects of hidden triples, is sound for some RDFS
and OWL RL rules but not for others (an `owl:allValuesFrom` restriction derives a type for
the object of a visible triple from a hidden type of its subject), nor for Jena rules.

So a protection in force with `hide_inferences` (the default) takes the inferred graph out
of the caller's read graphs. An operator whose rules cannot derive anything from a
protection's triples sets `hide_inferences = false`, and the inferred triples are then
masked by the same protections as the others. RDFS on read needs no such rule, because it
derives from the triples a query reads, which are the visible ones.

## 8. Caching, statistics and plans

### 8.1 The result cache

A masked snapshot adds its key to every result-cache key, so a cached result of the full
data never answers a masked query, nor the reverse. Callers with the same key share
entries.

### 8.2 Statistics

Exact counts from the statistics already correct for a snapshot's delta, within a work
budget. The mask is part of the delta, so the corrections stay exact and the planner
reads the index when a correction would cost more. Per-snapshot caches (exact counts,
delta statistics, visible graphs) start empty for a masked snapshot.

### 8.3 Plans

A view with protections in force has its plans redacted as in C12 §7.3.

## 9. Accepted leaks

- **Protected subjects.** Class and pattern protections depend on data. A caller that
  knows an IRI, for example as the object of a visible triple, sees that its triples are
  missing and that writes to it are refused, and so learns that it is protected.
- **Timing and plan shapes**, as in C12.
- **Full-text scores and the `maxHits` limit.** Scores use the statistics of the whole
  index, and a search without a limit that matches more than `maxHits` documents fails
  even when most of them are hidden.
- **Vector dimensions.** A query vector whose dimension matches no visible vector gets the
  same dimension error as before, which names the dimensions of every vector of the
  predicate.
- **The pattern's reach.** A pattern reads every graph and every triple, so the
  protection depends on triples the caller may not see. That is its purpose. The answer
  only shows whether the caller passed.

## 10. Design sketch

**Engine (`crates/sparkles`).**

- `access::triples` holds `Protection`, `Caller`, `Rule` (a protection with its lifts for
  one caller), `Limits` and `TripleRules`. `GraphAccess` gains `triples`, a
  `with_triples` constructor that keeps the rules in force and drops the inferred graph,
  and `masked(snapshot)`.
- `Snapshot` gains `mask`, the hidden quads of a masked snapshot. `CountCache` keeps the
  masked snapshots per view key.
- `sparql::make_ctx`, the update's WHERE clauses, schema discovery, shape drafts, the
  spatial map, diffs and Graph Store replaces read the masked snapshot. The result cache
  and RDFS on read key on the mask.
- `WriteTxn` records the quads asked for when protections limit writes, and checks them
  against `WriteCheck` at the start and end states before committing.

**Server (`crates/sparkles-server`).**

- `auth::config`: `[[protections]]`, `[protection_limits]` and `lifts` on grants.
- `auth::Grants` carries the policy's protections and the caller's role names.
  `Access::view` builds the `TripleRules` of a dataset and endpoint, and
  `Principal::caller` supplies `?user`, `?role` and `?group`.
- The Graph Store, listings and the MCP listing read the masked snapshot. Every other
  path takes the view through the engine.

## 11. Acceptance examples

**Fixture.** Dataset `hr` holds `ex:alice ex:salary 100 ; ex:name "Alice"`,
`ex:bob a ex:InPatient`, `ex:InPatient rdfs:subClassOf ex:Patient`, and two documents
owned by `tcarol` and `dave`. Protections `salaries` (by predicate), `patients` (by
class) and `docs` (class `ex:Doc` with `?s ex:owner ?user`). Users `tadmin` (admin),
`tstaff` (write, lifts nothing), `thr` (lifts `salaries` for writing), `tdoc` (lifts
`patients` for reading) and `tcarol` (read).

- **D1. Differential.** For random data, random protections, random lifts and random
  callers, every query of a fixed list answers through the view exactly what it answers
  on a store holding only the visible triples, after a bulk load and with a delta, with
  and without a union default graph, and after the result cache holds the full answer.
- **D2. Tiers.** `tstaff` sees neither salaries nor bob nor documents. `thr` sees
  salaries. `tdoc` sees bob. `tcarol` sees her document only.
- **D3. Existence.** `DELETE DATA` of alice's salary, and of a salary that does not
  exist, give identical 403s for `tstaff`.
- **D4. Writes.** `thr` inserts a salary. `tcarol` cannot take over dave's document. A
  non-doctor cannot create a patient or delete the subclass link.
- **D5. Inference.** An inferred type derived from a salary is hidden from `tstaff`.
- **D6. Routes.** `/$/stats/hr` is 403 for `tstaff`, `/$/datasets/hr` counts 3 quads,
  `whoami` says `triples: true`, and the change feed is 403.
- **D7. Full access.** `tadmin` and callers whose grants lift every protection run the
  plans of a caller without a view.

## 12. Rejected alternatives

- **Per-triple checks during evaluation**, as `jena-permissions` does through its
  `SecurityEvaluator` and the filters its query rewriter adds to every basic graph
  pattern. They cost a call per triple read, defeat the index-only fast paths, and need
  every operator to know about them.
- **Rewriting queries with filters.** Paths, statistics shortcuts and search indexes do
  not go through triple patterns, so a rewrite misses them.
- **Disabling the fast paths for protected callers.** The mask keeps them exact.
- **Allow and disallow filters per grant**, as AllegroGraph attaches them to users and
  roles. A grant without the filter would see the triples, so every grant of a dataset
  would have to repeat it. Protections fail closed: a new role does not see salaries until
  a grant lifts them.
- **Readers listed in the protection.** Grants already say who gets what. Lifting by name
  keeps grants the only place that gives access, at a level and in some graphs.
- **Inference per caller**, or keeping derivations, for the reasons of §7.
- **Exact per-commit masks for the change feed.** A feed page would build a mask per
  commit. The diff between two commits builds two.
- **Hiding IRIs as well as triples.** It would need a filter on every term of every
  result, and RDF data names resources in many places.

## 13. Open questions

1. Should the change feed serve data-dependent views, by masking each commit's states
   from a cached mask and the commit's changes?
2. Should a mask be kept up to date across commits instead of rebuilt, for workloads with
   many commits and many protected readers?
3. Should validation run on a masked view instead of being refused?
4. Should protections cover objects as well as subjects by class?

## 14. Sources

- **Sparkles repository:** `crates/sparkles/src/access.rs`, `store.rs` (`Delta`,
  `Snapshot`, `WriteTxn`), `sparql/` (`mod.rs`, `stats.rs`, `cache.rs`, `update.rs`,
  `rdfs.rs`), `text/search.rs`, `geo/index.rs`, `vector/search.rs`, `store/diff.rs`,
  `store/changes.rs`, `schema.rs`, and `crates/sparkles-server/src/auth/`; the specs
  C09, C12, C15, C16 and F06.
- **Apache Jena `jena-permissions`** (Apache-2.0), retired after 5.6.0. Source read at the
  `jena-5.6.0` release: `SecurityEvaluator` (the CRUD actions, graph-level and
  triple-level checks, the `ANY`, `VARIABLE` and `FUTURE` nodes), `SecuredGraph`,
  `query/rewriter/OpRewriter` (a filter per basic graph pattern), and the module's
  `readme.md`. Used as the per-triple design this spec rejects for evaluation, and for
  the separation of read and write checks.
- **AllegroGraph security filters**, public documentation fetched 2026-10-02,
  https://franz.com/agraph/support/documentation/security.html. Used for allow and
  disallow filters on subject, predicate, object and graph, their merging over a user's
  roles, and their independence of order.
- **MarkLogic element-level security**, public documentation found 2026-10-02 under
  https://docs.progress.com/bundle/marklogic-server-secure-12/ ("Element Level Security",
  "Protected Paths", "Protected Path Sets"). Used for protected paths that hide content
  from users without a role of a query roleset, and for combining several protections of
  one item.
- **W3C Solid** Web Access Control and Access Control Policy (access modes, matchers with
  `allOf`, `anyOf` and `noneOf`, deny overriding allow in ACP), cited from working
  knowledge.
- **W3C ODRL Information Model 2.2** (permissions, prohibitions, constraints, conflict
  strategies), cited from working knowledge.
- **OASIS XACML 3.0** (attribute-based policies, subject attributes, the deny-overrides
  and permit-overrides combining algorithms), cited from working knowledge.
- **SPARQL 1.1** Query (`VALUES`, group graph patterns), Update and Protocol, and RDF
  Schema (`rdf:type`, `rdfs:subClassOf`), cited from working knowledge.
- **Not consulted:** anything from Fluree, including its policy language, source,
  documentation and design notes.

## Outcome

**Delivered on 2026-10-02**, with all three tiers in one phase.

- The engine's `sparkles::access::triples` module holds `Protection`, `Caller`, `Rule`,
  `Limits`, `TripleRules`, `Mask` and `WriteCheck`. `GraphAccess` gained `triples`,
  `with_triples`, `hides_triples`, `reads_everything` and `masked`, and `Graphs` gained
  `without_iri`, which removes the inferred graph from a view.
- `Snapshot` gained `mask`. A masked snapshot is the snapshot with the hidden quads
  moved into its delta, with empty per-snapshot caches. The snapshot's `CountCache` keeps
  one masked snapshot per view key, built once even when requests arrive together.
- Queries, explain, update `WHERE` clauses, `CLEAR` and `DROP`, Graph Store replaces,
  schema discovery, shape drafts, the spatial map, diffs and the change feed apply the
  view. Full-text hits are checked against the mask. The result cache and RDFS on read's
  schema cache key on the mask.
- `WriteTxn` records the quads asked for when protections limit writes, checks them at
  the start and end states at the start of the commit (also for dry runs), and sends
  bulk inserts of such a caller through the delta. `DELETE DATA` checks quads of terms
  the store lacks too.
- The server's configuration gained `[[protections]]`, `[protection_limits]` and `lifts`
  on grants, with validation and a warning for a protection no grant lifts. `Grants`
  carry the policy's protections and the caller's role names, `Access::view` builds the
  rules per dataset and endpoint, and `Principal::caller` gives `?user`, `?role` and
  `?group`. `whoami` reports `triples: true`. The Graph Store, dataset listings and MCP's
  `list_datasets` read the masked snapshot.
- A new budget kind, `hidden-quads`, reports a view past `max_hidden_quads`.

**Deviations from the design.**

- The refusal of routes that cover every graph now says "limited to some graphs or
  triples".
- A view whose protections are lifted for reading but not for writing has no mask and
  plans like an unlimited view. Only its writes are checked.

**Tests at landing.**

- `crates/sparkles/tests/triple_access.rs` compares 56 queries through random views
  with a store holding exactly the visible triples, which the test works out from the
  rules' definitions without the engine. Each case draws random data (a compacted base
  and a delta of inserts and deletes over five graphs, including the inferred graph), one
  to three protections from a pool of eleven (by predicate, by predicate pattern, by
  class with and without subclasses, by graph, and by patterns that use `?user`, `?role`,
  `?this`, `?o`, other graphs' triples and no triple variable at all), random lifts, a
  random `hide_inferences`, random read graphs and a random caller. 24 cases run without
  and 12 with a union default graph, each query after the result cache holds the full
  answer and again from the cache. A sweep of 450 cases passed before landing. The
  queries cover patterns, `GRAPH ?g`, `FROM` and `FROM NAMED`, the union graph, paths,
  counts with and without patterns, grouped and distinct counts, aggregates, `DESCRIBE`,
  `CONSTRUCT`, `ASK`, `EXISTS`, `NOT EXISTS`, `MINUS`, `OPTIONAL`, subqueries, `ORDER BY`
  with `LIMIT`, string filters, `VALUES`, `spk:vectorSearch`, `text:query`,
  `spk:hybridSearch` and `geof:sfWithin`. The same file covers counts from the
  statistics, writes that are refused alike whether or not the triple exists, lifts for
  writing and patterns for owners, class hierarchy edits, `CLEAR`, Graph Store
  replaces, dry runs, hidden inferences, RDFS on read, plan redaction, mask caching,
  diffs whose visibility changes, schema reports and the hidden-quads limit.
- `router_tests::auth::triples` is a matrix of five users (admin, a writer under every
  protection, a role that lifts salaries for writing, a role that lifts patients for
  reading, and a document owner) against queries, counts, the result cache, `ASK`, the
  Graph Store, explain, `DESCRIBE`, full-text search, updates, Graph Store writes, dry
  runs, refused routes, listings, `whoami`, schema reports, drafted shapes, diffs, the
  change feed and stored queries, and validates configurations.
  `router_tests::mcp::auth::tools_follow_triple_protections` covers the MCP tools.
- Crate and server tests, Clippy over all targets, `mise run lint:features`, the W3C
  suites (SPARQL 1.0 482/482, 1.1 query 328/328, 1.1 update 157/157, 1.2 269/269), the
  SHACL suites (98/98 and 20/20), shexTest (validation 1062 passed with its one known
  failure), `mise run test:jena-clients` (138 checks) and `mise run ci` pass.

**Performance.** Measured over HTTP on the 1.05M-quad benchmark data
(`scripts/gen-data.py 100000`), with the result cache off, through keep-alive
connections, as medians of 60 rounds that run every caller once per round in a rotating
order. The machine was shared with other builds (load average about 20 on 16 cores), so
differences under about 0.2 ms are noise. `main` is the commit this work started from,
and the full callers are an `admin` token and, on this change, a token whose grants lift
every protection. The protected callers hide salaries (100,000 quads, by predicate), the
triples of managers (81,295 quads of 10,000 subjects, by class), or books that the caller
did not write (56,118 quads, by class and a pattern over `ex:authorOf` and `foaf:name`).

| Query (median ms) | `main`, full | This change, full | Lifted | Salaries hidden | Managers hidden | Books by pattern |
|---|---|---|---|---|---|---|
| `COUNT(*)` of everything | 0.56 | 0.46 | 0.39 | 1.01 | 0.96 | 0.79 |
| average salary | 6.65 | 6.54 | 6.54 | 1.30 | 7.57 | 6.65 |
| instances per class | 0.85 | 0.81 | 0.75 | 0.79 | 2.25 | 2.41 |
| a star join on managers | 2.75 | 2.76 | 2.64 | 2.75 | 2.29 | 2.67 |
| one subject | 0.24 | 0.20 | 0.19 | 0.19 | 0.19 | 0.18 |
| a three-hop `foaf:knows` path | 0.35 | 0.27 | 0.27 | 0.26 | 0.70 | 0.27 |
| a numeric filter | 0.55 | 0.44 | 0.44 | 0.44 | 0.93 | 0.43 |
| books and titles | 1.23 | 0.89 | 0.91 | 0.82 | 0.88 | 0.90 |
| a join with salaries | 1.65 | 1.44 | 1.38 | 0.88 | 3.21 | 1.35 |
| the ten lowest salaries | 3.74 | 3.62 | 3.55 | 1.08 | 3.84 | 3.48 |

A caller without protections in force runs as before: its options carry no rules, the
snapshot has no mask, and the plans of the graph-view tests' full views are unchanged.
The differences between the first two columns are noise of the shared machine. A
protected caller pays once per commit for its mask: the first request after a commit took
0.16 s to 0.6 s for these views across runs, and later requests reuse the masked
snapshot. On the mask, counts from statistics correct for the hidden quads (0.4 ms more
for `COUNT(*)`), and scans through blocks with hidden quads merge them like any deleted
quads, which costs up to twice the time where the hidden quads are spread over the
subjects a query reads (the managers' class counts, paths, filters and joins). Queries
over hidden predicates get faster, since there is less to read.
