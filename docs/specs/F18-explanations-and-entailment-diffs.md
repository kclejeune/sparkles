# F18: Explanations and entailment diffs

> **Status:** implemented in part, Phases 1 and 2.
>
> **Phases:** Derivation trees, bounded why-not explanations, entailment diffs between
> retained states and branch heads, write and merge previews, and minimal justifications
> are available. Phase 3's provenance-aware inferred visibility and reviewed-only
> protections remain planned and depend on F13's per-fact provenance.
>
> **User docs:** [Explanations](../API.md#explanations),
> [entailment diffs](../API.md#entailment-diffs),
> [minimal justifications](../API.md#minimal-justifications) and
> [features](../FEATURES.md#reasoning-validation-and-schema).
>
> This is the design as written before implementation. The [Outcome](#outcome) records
> what shipped and the remaining work.

This design was written from the W3C OWL 2 Profiles recommendation (the OWL 2 RL/RDF
rules), Kalyanpur, Parsia, Horridge and Sirin, "Finding all justifications of OWL DL
entailments" (ISWC 2007), Reiter, "A theory of diagnosis from first principles"
(Artificial Intelligence, 1987), Green, Karvounarakis and Tannen, "Provenance semirings"
(PODS 2007), Motik, Nenov, Piro and Horrocks, "Incremental Update of Datalog
Materialisation: the Backward/Forward Algorithm" (AAAI 2015), Apache Jena's public
documentation of rule-engine derivation logging (`InfGraph.getDerivation`), and the
Sparkles code. It builds on materialization and diagnostics
([C08](C08-inference-freshness.md)), query-time reasoning
([F17](F17-query-time-reasoning.md)), materialization at scale
([F16](F16-materialization-at-scale.md)), per-fact provenance
([F13](F13-incremental-views-and-provenance.md)), branches and merges
([F09](F09-branches-and-merges.md)), write previews ([C15](C15-write-previews.md)),
triple protections ([C12b](C12b-triple-access-control.md)), agent memory and its review
([C17](C17-agent-memory.md), [C18](C18-natural-language-questions-and-ingest/README.md)) and the
MCP server ([C11](C11-mcp-server.md)).

## 1. Summary, goals, non-goals

The reasoner keeps no record of why a triple was derived. C08 non-goals left out
"explanations beyond the matched evidence", and the diagnostics report gives the facts a
check matched, some of them inferred, without their origin. C12b §7 hides every
materialized inference from a caller with a protection in force, because "an inference
that is visible only when all its premises are visible would need the derivation of every
inferred triple, which the reasoner does not keep". A user who adds an axiom on a branch
cannot see what it changes in the closure before merging it.

The reasoner already has the pieces to recover derivations. `incremental::Plans`
compiles, per rule head, a plan with the head's variables bound, and `Search::instance`
in `crates/sparkles-reasoner/src/incremental.rs` enumerates the rule instances that
derive a fact. F17's demand transformation derives exactly the facts one triple needs.
F13's provenance annotations keep, per derived row, the sets of input facts that support
it.

**Goals**

1. **Why?** A derivation tree for any triple the reasoning entails: the rule, its
   bindings, and the premises, down to asserted triples and axioms (§3).
2. **Why not?** For a triple that is not entailed, the rules that could derive it and
   small sets of missing facts that would, each verified (§4).
3. **Entailment diffs.** The entailments that appear and disappear between two commits or
   branches, in F09 merge previews and C15 write previews (§5).
4. **Justifications.** Minimal sets of asserted triples that cause a diagnostics finding
   or make a class unsatisfiable (§6).
5. **Provenance-aware visibility.** Protections and `reviewedOnly` that keep an inferred
   triple when the caller can see a set of facts that supports it (§7).
6. The same explanations through HTTP, the CLI, the library, MCP and the UI (§2).

**Non-goals.** Explanations of SPARQL query answers in general (F13's `provenance=why`
does that for SELECT). OWL DL justifications. Natural-language explanations generated in
the server (an agent or C18's assistant can phrase the tree). Explanations for RDFS on
read beyond the RDFS profile of the same schema.

## 2. User-visible behaviour

### 2.1 `POST /$/reason/{ds}/explain`

```ts
type ExplainRequest = {
  triple: { s: Term; p: Term; o: Term };      // Term as in docs/API.md
  mode: "why" | "whyNot";
  source?: "materialized" | "demand";         // default: as F17's auto chooses
  maxTrees?: number;        // why: distinct top-level derivations, default 1, max 10
  maxDepth?: number;        // default 32
  maxMissing?: number;      // whyNot: facts per hypothesis, default 2, max 4
  maxHypotheses?: number;   // whyNot: default 10
  timeout?: number;
};

type Derivation = {
  id: number;                                  // subtrees shared in a DAG refer by id
  triple: { s: Term; p: Term; o: Term };
  kind: "asserted" | "axiom" | "derived" | "ref";
  graph?: string;                              // asserted: the input graph that holds it
  rule?: { name: string; line?: number; profile: string; owl2rl?: string };
  bindings?: Record<string, Term>;
  builtins?: { call: string; holds: true }[];  // builtins the instance passed
  premises?: Derivation[];
  sameAs?: { member: Term; representative: Term }[];   // F16 representatives used
};

type ExplainResponse =
  | { mode: "why"; entailed: true; commit: number; trees: Derivation[]; more: boolean }
  | { mode: "why"; entailed: false; commit: number }
  | { mode: "whyNot"; entailed: false; commit: number;
      rules: { name: string; line?: number; unifier: Record<string, Term> }[];
      hypotheses: { missing: { s: Term; p: Term; o: Term }[];
                    blockedBy?: { call: string; fact?: Triple }[];
                    tree: Derivation }[];
      complete: boolean }
  | { mode: "whyNot"; entailed: true; commit: number };
```

`rule.owl2rl` gives the OWL 2 RL rule id when the rule is one of the profile's (the names
in `rules/owl-rl.rules` follow the profile's tables). It needs `read` on the dataset and
answers on the caller's view (§7). Errors: `400` for a malformed triple, `422` with
`reasoning-budget` when the search passes its limits, and `404` for an unknown dataset.

### 2.2 Other routes

| Route | Meaning |
|---|---|
| `GET /$/reason/{ds}/diff?from=C&to=D` | Entailment diff between two commits or branch heads (§5). `from` and `to` take a commit `seq`, a branch name or `branch@seq`. |
| `POST /$/reason/{ds}/justify` | Justifications for a diagnostics finding (its check, focus and evidence as the report gives them) or for a class (`{"class": IRI}`) (§6). |

`GET /$/reason/{ds}/diagnostics` gains `justify=true`, which adds the first justification
to each finding, within the report's deadline.

### 2.3 CLI

```
sparkles infer --loc DB --why '<http://ex.org/tom> a <http://ex.org/Animal>'
sparkles infer --loc DB --why-not '<http://ex.org/rex> a <http://ex.org/Pet>' [--max-missing 2]
sparkles infer --loc DB --diff main..feature [--format text|json|turtle]
sparkles infer --loc DB --check --justify
```

The text form prints the tree indented, one premise per line, with the rule name and the
OWL 2 RL id.

### 2.4 MCP

The tool `explain_inference` takes `dataset`, `triple` (three terms in N-Triples or
prefixed form), `mode` and the limits of §2.1, and returns the response with a short
`summary` line per tree, such as "ex:tom a ex:Animal because ex:tom a ex:Cat (asserted)
and ex:Cat rdfs:subClassOf ex:Animal (asserted), by cax-sco". The tool
`entailment_diff` takes `dataset`, `from` and `to`. Both are read-only and offered
whenever the reasoning feature is built. C18's `why_empty` gains one step: when the empty
query's first failing pattern is intensional (F17 §3.1), it calls the why-not search for
the pattern's constants and adds the hypotheses to its result.

### 2.5 UI

- **Explore page.** `ui/src/routes/explore/+page.svelte` leaves the inferred graph out of
  its graph list today. A resource's detail panel lists inferred triples with an
  **inferred** badge, from the inferred graph or, under F17 `auto`, from derivation. The
  badge opens a **Why?** panel that shows the derivation tree, collapsible, with each
  asserted premise linked to its resource and each rule name linked to the rule text.
- **Query page.** A result cell whose triple pattern was intensional offers **Why?** on
  the row.
- **Diagnostics sub-panel.** Each finding gains **Justify**, which shows the
  justifications as lists of asserted triples, each with a link to the explorer and, for
  a person with `write`, a **Remove on a branch** action that opens a branch with the
  triple deleted and its entailment diff.
- **Merge and review pages.** `ui/src/routes/datasets/[name]/merge` and the review
  page show the entailment diff of §5 as two lists, added and removed, with counts per
  predicate and class and the new diagnostics findings.
- **Why not.** The detail panel has a **Why not…?** field for a triple about the
  resource, which shows the hypotheses.

## 3. Why?

### 3.1 Finding a derivation

A derivation must be well founded: no fact may appear among its own premises. The engine
records, for each fact, the first rule instance that derived it in a semi-naive
evaluation. Every premise of that instance was in the closure before it, so following
first instances gives a well-founded tree.

The explanation runs F17's demand program for the triple, with the triple as its only
seed, and records first instances while it evaluates. Each `Eval` in
`crates/sparkles-reasoner/src/engine.rs` that emits a new fact also emits the rule index
and the bindings of its instance, and the merge keeps them for the facts it adds. The
demand program touches only what the triple needs, so recording costs little. The tree
is read off the record, sharing repeated subtrees by `id`.

With `maxTrees` above 1, the search asks for different top-level instances: it
enumerates the rule instances whose head is the triple with `Search::instance`, keeps
those whose premises are all in the derived set, and builds a tree for each from the
record. Two trees differ at least at the root's rule instance.

When the triple is materialized and fresh and the server keeps the closure in memory
(`maintain::Cache`), the record could be built over the kept closure instead. The spec
does not do this, because a kept closure's row order no longer gives first derivations
after incremental deletions (backward/forward keeps a fact by a proof that may use later
rows). The demand run is the single method, and its answer is checked against the
materialization: a triple in the inferred graph that the demand run does not derive is a
bug and is reported as one.

### 3.2 What a tree shows

- **Asserted** leaves name the input graph that holds the triple (C08 §11 input graphs).
- **Axiom** leaves are facts from rules with an empty body, such as the RDFS axiomatic
  triples.
- **Derived** nodes give the rule, its bindings, the builtins it passed, and its
  premises in body order.
- Under F16 representatives, a node whose instance used a representative lists the
  members it stood for and the `owl:sameAs` derivation that merged them as a premise.
- Generalized facts (C08 Phase 3: a literal subject, a blank node predicate) can be
  premises and are shown as such, with a note that they are never written to the
  inferred graph.

### 3.3 Limits

`maxDepth` limits the tree's depth. A deeper tree is cut with `kind: "ref"` nodes that
the client can expand with another request on that triple. The demand run uses F17's
budgets.

## 4. Why not?

### 4.1 The search

Why-not is abduction: find facts that, added to the asserted data, would make the triple
entailed. The search unfolds rules backward from the triple:

1. Every rule whose head unifies with the triple is a candidate, and the response lists
   them with their unifiers, whether or not any hypothesis is found.
2. For a candidate instance, each body atom is either satisfied in the current closure
   (computed on demand), or **missing**. A missing atom with a constant predicate that
   no rule derives is a hypothesis fact. A missing intensional atom is unfolded further,
   up to `maxDepth`, or kept as a hypothesis fact itself.
3. A missing atom with a variable that no satisfied atom binds is kept with a fresh
   placeholder (`?y`), shown as "some `?y` such that …".
4. A builtin that fails under the bindings blocks the instance and is reported in
   `blockedBy`. A `noValue` that fails names the fact that blocks it.
5. Hypotheses with more than `maxMissing` facts are dropped. The rest are sorted by size,
   then by the number of placeholders, and the first `maxHypotheses` are kept.

### 4.2 Verification

Each hypothesis is checked: the search adds its facts, with placeholders as fresh blank
nodes, to an overlay on the snapshot and runs the demand program for the triple. Only
hypotheses under which the triple is derived are returned, each with the derivation tree
under the overlay. A hypothesis that also makes the diagnostics checks report a new
inconsistency is marked so.

`complete` is `true` when the unfolding ended within its limits, so every hypothesis of at
most `maxMissing` facts within `maxDepth` was considered. Rules with blank nodes in their
heads or with `makeTemp` are unfolded but their hypotheses are not verified, and they make
`complete` false.

## 5. Entailment diffs

### 5.1 Semantics

The entailment diff from state `A` to state `B` under a rule program is the pair
`(closure(B) \ closure(A), closure(A) \ closure(B))`, restricted to facts that are not
asserted in the state that has them. Asserted changes are the commit diff of F06 and are
reported apart, so the diff shows what reasoning adds to a change.

### 5.2 Computation

- **Both states materialized and fresh, same program.** The diff of the two inferred
  graphs, with F06's commit diff restricted to `urn:x-sparkles:inferred` when the states
  are on one history, or a comparison of the two graphs' sorted scans otherwise. With
  F16's layer, the two layers are compared by their sorted permutations.
- **Otherwise.** The closure of `A` (kept, saved or computed in full) is copied and
  updated with the asserted changes from `A` to `B` by `incremental::update`, without
  writing anything. The update's `deleted` facts and its added rows are the diff. This
  is the computation a C08 incremental run already performs, run as a dry run. The
  fallbacks of C08 and F16 §7.3 apply, and a fallback computes `closure(B)` in full and
  compares.

The program is the dataset's recorded materialization program, or F17's query-time
program, or one given in the request (`profile`, `rules`, `inputs`), so a user can ask
what a new rule would change.

### 5.3 Merges and previews

- **F09 merge previews.** A merge dry run gains `entailments: true`. The response adds
  `entailments: { added: number; removed: number; byPredicate: …; sample: Triple[];
  newFindings: Finding[] }` for the target branch before and after the merge, and the
  merge page shows it. F09 §4.9 leaves the inferred graph out of merges, so the diff is
  of the target's program over the merged asserted data.
- **C15 write previews.** A dry run of an update, a Graph Store write, an upload or an
  RDF Patch gains `entailments=true` with the same member. It is opt-in, because it costs
  a closure update.
- **Branch review.** The review page (`ui/src/routes/datasets/[name]/review/[branch]`)
  shows the diff of the branch against its upstream.

`newFindings` runs C08's diagnostics on both states, restricted to findings whose
evidence includes a fact of the diff, and reports those present only in `B`.

### 5.4 Output

JSON as in §5.3, Turtle as two graphs in TriG (`spk:added`, `spk:removed`), and RDF
Patch with only `A` and `D` rows for the inferred graph, so the diff can be applied to a
copy of the inferred graph.

## 6. Justifications

### 6.1 For findings

A diagnostics finding (C08 §2.3) holds when a set of closure facts holds, such as
`T(x, c1)`, `T(x, c2)` and `c1 owl:disjointWith c2`. A justification is a set of asserted
triples whose closure contains those facts and that has no proper subset with the same
property.

1. **One justification.** The asserted leaves of the derivation trees of the finding's
   facts form a set `J` that entails the finding. `J` is minimized by removing one triple
   at a time and keeping the removal when the finding still holds over `J` minus that
   triple, checked by the demand program over an overlay that contains only `J`. This is
   the black-box contraction of Kalyanpur et al., with the entailment check done by the
   rule engine over a small set.
2. **All justifications, bounded.** Further justifications come from Reiter's hitting
   set tree, as Kalyanpur et al. describe: each node removes one triple of a justification
   already found from the asserted data and asks for a justification of what remains.
   `maxJustifications` (default 5) and `maxChecks` (default 500) bound the tree, and the
   response says whether it was exhausted.

A justification lists asserted triples with their graphs, and for each the derivation
role it plays (a schema axiom or an assertion), so the UI can group ontology and data.

### 6.2 For unsatisfiable classes

C08's `unsatisfiable-class` warning finds classes with `rdfs:subClassOf+ owl:Nothing`.
Under OWL 2 RL, a class is also unsatisfiable when an instance of it would make the
dataset inconsistent, such as a subclass of two disjoint classes. The probe adds
`_:probe rdf:type C` to an overlay, runs the program on demand from it and runs the
diagnostics checks restricted to findings about `_:probe`. A finding means `C` is
unsatisfiable, and its justification (§6.1), with the probe triple removed, is the
justification of the unsatisfiability. `POST /$/reason/{ds}/justify` with
`{"class": IRI}` runs the probe, and `{"classes": "all"}` runs it for every class with a
declared `owl:disjointWith`, `owl:complementOf`, `owl:AllDisjointClasses` or
`owl:Nothing` in its superclass cone, under the request's deadline.

## 7. Visibility by support

### 7.1 Protections

C12b's `hide_inferences` is a boolean: `true` hides the inferred graph from a caller with
the protection in force, `false` shows it masked by the same protections. A third
choice is added as the protection field `inferences`:

| Value | Meaning |
|---|---|
| `"hide"` | As `hide_inferences: true`. The default. |
| `"show"` | As `hide_inferences: false`. |
| `"supported"` | An inferred triple is visible when it has a derivation all of whose asserted leaves the caller can see. |

`"supported"` needs per-fact provenance. With F13 Phase 3, the reasoner's closure is
maintained with `why` provenance (F13 §5.1): each derived fact keeps up to `maxWitnesses`
sets of asserted facts (`FactId`s), each of which derives it. For a view and a commit,
the hidden derived triples are those of which no kept witness is wholly visible. They are
found from a reverse index from `FactId` to the derived facts whose witnesses contain it,
starting from the facts the view's mask hides, so the work follows the hidden facts and
not the closure. The hidden derived triples join the masked snapshot's deletions
(`Snapshot::mask`, C12b §8), and the mask is cached per view and commit as C12b caches
masks today.

A derived triple whose witnesses were truncated and none of the kept ones is visible is
hidden, though another witness might be visible. The rule therefore never shows a triple
that the caller's facts do not support, and may hide one that they do. The status
reports how many triples were hidden for that reason, and a larger `maxWitnesses` lowers
it. Callers who need the exact set use F17's `demand` mode, which derives from the
visible facts alone (F17 §4.5).

### 7.2 `reviewedOnly`

`reviewedOnly` on `POST /{ds}/ask` hides the dataset's `agentGraphs` from the question's
principal (`hiding_graphs` in `crates/sparkles-server/src/ask/http.rs`). The inferred
graph stays readable. When a materialization names agent graphs among its input graphs
(C08 §11), triples derived from unreviewed agent facts are visible under `reviewedOnly`.
With F18, `reviewedOnly` applies `"supported"` to the inferred graph for the agent
graphs: an inferred triple is kept when some witness has no fact in an agent graph. Before
F13 Phase 3, `reviewedOnly` hides the inferred graph whenever the recorded input graphs
include an agent graph, which is the safe choice, and the question header says so.

### 7.3 Explanations under protections

An explanation runs on the caller's view. A derivation whose leaves the caller cannot see
is not shown, and for that caller the triple is entailed only if another derivation from
visible facts exists. Why-not hypotheses never name hidden facts as missing, because the
caller could otherwise probe for them: a hypothesis that is satisfied by a hidden fact is
reported as satisfied, without the fact, only when the protection's `inferences` is
`"show"`, and is dropped otherwise.

## 8. Architecture

| Crate / file | Change |
|---|---|
| `crates/sparkles-reasoner/src/explain.rs` (new) | First-instance recording, tree building, why-not unfolding and verification. |
| `crates/sparkles-reasoner/src/engine.rs` | `Mode::Record` in `Eval`, which emits the rule index and bindings with each new fact. |
| `crates/sparkles-reasoner/src/incremental.rs` | `Search::instance` exposed to `explain.rs`; dry-run updates on a copied closure for diffs. |
| `crates/sparkles-reasoner/src/justify.rs` (new) | Contraction, the hitting set tree and the class probe. |
| `crates/sparkles-reasoner/src/diagnostics.rs` | `justify=true`; checks restricted to a set of facts for `newFindings` and probes. |
| `crates/sparkles-core/src/access/triples.rs` | The `inferences` protection field and the supported-visibility mask from F13 provenance. |
| `crates/sparkles/src/reasoning/` | `Dataset::reasoning().explain`, `.diff`, `.justify`. |
| `crates/sparkles-server/src/reasoning.rs`, `http.rs` | The routes of §2. |
| `crates/sparkles-server/src/http/branches*`, preview code | The `entailments` member of merge and write previews. |
| `crates/sparkles-server/src/mcp/tools.rs` | `explain_inference`, `entailment_diff`, the `why_empty` step. |
| `crates/sparkles-server/src/ask/http.rs` | `reviewedOnly` over inferences. |
| `ui/src/routes/explore/+page.svelte`, `ui/src/routes/query/+page.svelte`, `ui/src/routes/datasets/[name]/…` | Badge, Why? and Why not panels, justifications, merge and review diffs. |

## 9. Phases

**Phase 1 (M).** Why? and why-not with the record mode of the demand engine, the
`explain` route, the CLI, the library, `explain_inference`, the `why_empty` step, and the
UI's badge and panels. It needs F17 Phase 1.

**Phase 2 (M).** Entailment diffs with the routes, merge and write previews and the
review page; justifications for findings and unsatisfiable classes, `justify=true`.

**Phase 3 (M).** `inferences: "supported"` and `reviewedOnly` over inferences from F13
Phase 3 provenance. Until then, `reviewedOnly` uses the safe rule of §7.2.

## 10. Acceptance examples

The examples use `ex:` = `http://ex.org/`.

**A1. Why.** With RDFS and the data `ex:Cat rdfs:subClassOf ex:Mammal . ex:Mammal
rdfs:subClassOf ex:Animal . ex:tom a ex:Cat .`, explaining `ex:tom a ex:Animal` returns
one tree: rule `rdfs9` with premises `ex:tom a ex:Mammal` (itself `rdfs9` from the
asserted `ex:tom a ex:Cat` and `ex:Cat rdfs:subClassOf ex:Mammal`) and the asserted
`ex:Mammal rdfs:subClassOf ex:Animal`, or the equivalent tree through the derived
`ex:Cat rdfs:subClassOf ex:Animal` by `rdfs11`. With `maxTrees: 2`, both are returned.

**A2. Not entailed.** Explaining `ex:tom a ex:Plant` with `mode: "why"` returns
`entailed: false`.

**A3. Why not.** With OWL 2 RL, `ex:owns rdfs:range ex:Pet` absent and `ex:ann ex:owns
ex:rex` asserted, why-not for `ex:rex a ex:Pet` lists `prp-rng` and `cax-sco` among the
candidate rules and returns the hypothesis `{ ex:owns rdfs:range ex:Pet }` with its
verified tree, and the hypothesis `{ ex:rex a ?c . ?c rdfs:subClassOf ex:Pet }` with a
placeholder, among the first three.

**A4. Blocked by negation.** With the Jena rule `[(?x a ex:Person), noValue(?x ex:died)
-> (?x a ex:Living)]` and `ex:bob a ex:Person . ex:bob ex:died "1990"`, why-not for
`ex:bob a ex:Living` reports the rule with `blockedBy` naming `ex:bob ex:died "1990"`.

**A5. Entailment diff on a branch.** On branch `feature`, insert
`ex:Kitten rdfs:subClassOf ex:Cat`. With 1,000 instances of `ex:Kitten` on `main` and
RDFS materialized there, `GET /$/reason/t/diff?from=main&to=feature` reports 1,000 added
`rdf:type ex:Cat` facts and 1,000 `rdf:type ex:Animal` facts and nothing removed. The
merge dry run reports the same counts in `entailments`.

**A6. New finding in a preview.** With `ex:Cat owl:disjointWith ex:Dog` and
`ex:tom a ex:Cat`, a dry run of `INSERT DATA { ex:tom a ex:Dog }` with
`entailments=true` reports one `newFindings` entry, `disjoint-classes` for `ex:tom`.

**A7. Justification.** For C08 example D's finding, the justification is
`{ ex:Cat owl:disjointWith ex:Dog . ex:Kitten rdfs:subClassOf ex:Cat . ex:tom a ex:Kitten
. ex:tom a ex:Dog }`, and no proper subset entails the finding. Adding `ex:tom a ex:Cat`
gives a second justification without the subclass triple, and both are returned.

**A8. Unsatisfiable class.** With `ex:CatDog rdfs:subClassOf ex:Cat, ex:Dog` and
`ex:Cat owl:disjointWith ex:Dog`, justifying `{"class": "http://ex.org/CatDog"}` reports
it unsatisfiable with the justification of those three triples.

**A9. Supported visibility.** A protection hides `ex:salary` triples from `tstaff`, with
`inferences: "supported"`. The rules derive `ex:x a ex:Employee` both from `ex:x
ex:salary 1000` and from `ex:x ex:worksFor ex:acme` with `ex:worksFor rdfs:domain
ex:Employee`, and derive `ex:y a ex:Employee` only from a salary. `tstaff` sees
`ex:x a ex:Employee` and not `ex:y a ex:Employee`. C12b's example D5 still holds under
`"hide"`.

**A10. MCP.** `explain_inference` on A1's triple returns the tree and a summary line
naming `cax-sco` (OWL 2 RL) or `rdfs9` (RDFS).

## 11. Testing and measurement

- **Soundness of trees.** For random programs and data (the generators of
  `incremental.rs`'s tests), every returned tree is checked by replaying it: each derived
  node's rule instance, with its bindings, must produce its triple from its premises, and
  every leaf must be asserted or an axiom. Every triple of the inferred graph must have a
  tree.
- **Why-not.** Every returned hypothesis is re-verified by a full materialization over the
  data plus the hypothesis. For small random programs, an exhaustive search over all
  facts of the vocabulary up to `maxMissing` checks `complete: true` answers.
- **Diffs.** The dry-run diff equals the difference of two full materializations for the
  random steps of C08's differential tests.
- **Justifications.** Each justification entails its finding, and removing any one triple
  makes it not entailed. The W3C OWL 2 RL inconsistency tests give findings whose
  justifications are compared with the test ontologies.
- **Jena.** For RDFS and Jena rule sets, the trees are compared with Jena's derivations
  (`InfGraph.getDerivation` with derivation logging on) for the same triples: the leaves
  of some Sparkles tree equal the leaves of Jena's.
- **Visibility.** C12b's randomized protection tests run with `inferences: "supported"`,
  checking that no visible inferred triple lacks a visible witness and that the mask
  agrees with F17 `demand` on the same view for programs where witnesses are not
  truncated.
- **Cost (X06).** Time to explain LUBM inferred types at 100 universities (target: under
  100 ms at the median), diff time for a one-axiom change, and justification time for the
  W3C test findings.

## 12. Rejected alternatives

- **Storing a derivation per inferred triple in the dataset**, for example as RDF 1.2
  reifiers. It multiplies the inferred graph's size, goes stale with it and, with
  `storage: "layer"` of F16, would have to live in the base indexes.
- **Building trees from the kept closure.** Its row order stops giving well-founded
  derivations after incremental deletions (§3.1).
- **Jena's derivation logging.** It records every derivation during materialization, at a
  cost on every run, to answer the few explanations asked for.
- **Structural rules for visibility**, such as hiding inferred triples about subjects of
  hidden triples. C12b §7 explains why they are unsound for some OWL 2 RL rules and for
  Jena rules.
- **Glass-box justifications with a tableau tracer.** The rule engine is the reasoner, so
  black-box contraction with it as the oracle gives the same sets.

## 13. Open questions

1. Should trees carry a PROV-O rendering (`prov:wasDerivedFrom`, `prov:used`) for export?
   The spec keeps JSON and text, and Turtle for diffs only.
2. Should `inferences: "supported"` become the default once F13 Phase 3 lands? It shows
   more than `"hide"` and never leaks, but costs a mask per view and commit.
3. Should why-not search include hypotheses that delete facts, to undo a `noValue`
   block? The spec reports the blocking fact and does not propose its removal.
4. Should entailment diffs be computed for every merge by default when the target has a
   materialization? The cost is one incremental update per merge preview.

## Provenance

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

- **Spec:** [`F18-explanations-and-entailment-diffs.md`](F18-explanations-and-entailment-diffs.md), written on 2026-10-12 independently from:
  - Kalyanpur, Parsia, Horridge and Sirin, "Finding all justifications of OWL DL entailments" (ISWC 2007), and Reiter, "A theory of diagnosis from first principles" (Artificial Intelligence, 1987), for black-box contraction and the hitting set tree;
  - Green, Karvounarakis and Tannen, "Provenance semirings" (PODS 2007), through F13's provenance model;
  - Motik, Nenov, Piro and Horrocks, "Incremental Update of Datalog Materialisation: the Backward/Forward Algorithm" (AAAI 2015);
  - W3C OWL 2 Web Ontology Language Profiles (the OWL 2 RL/RDF rules), cited from working knowledge and not re-fetched;
  - Apache Jena's public documentation of derivation logging in its rule engines (`InfGraph.getDerivation`), cited from working knowledge; no Jena code was read for this spec;
  - the Sparkles code (`crates/sparkles-reasoner/src/{engine.rs,incremental.rs,diagnostics.rs}`, `crates/sparkles-core/src/access/triples.rs`, `crates/sparkles-server/src/ask/http.rs`, `crates/sparkles-server/src/mcp/tools.rs`, `ui/src/routes/explore/+page.svelte`) and specs C08, C11, C12b, C15, C17, C18, F09, F13, F16 and F17.
- **Implementation:** not built. The spec plans no new runtime dependency.
- **Rejected** (spec §12):
  - storing a derivation per inferred triple in the dataset;
  - building trees from the kept closure;
  - Jena-style derivation logging during materialization;
  - structural visibility rules;
  - glass-box justifications with a tableau tracer.

## Outcome

Phase 1 extends the `why` mode of `POST /$/reason/{ds}/explain` shipped first with
[U10 Phase 3](U10-canvas-lenses.md#phase-3-2026-10-24), and Phase 3 remains
[ROADMAP](../ROADMAP.md) item 18.

### Phase 1, 2026-10-24

The existing explanation route and response gain demand derivations, bounded why-not,
rule bindings, passed builtins, generalized premises and partial trees. The demand
session keeps facts before linear-recursion rewriting so every intermediate fact still
has an instance of an original rule. A backward search over those facts computes
well-founded proof depths rather than relying on row order. Materialized explanations
fall back to that same demand search when a stored conclusion needs generalized
premises. Default geometries have a `geo-default-geometry` proof.

Why-not unfolds candidate rules, shares placeholders across missing atoms, and verifies
only RDF hypotheses on uncached overlays. Repeated atoms count as one missing fact.
Ground builtin checks use the rule evaluator, and `noValue` blockers name their facts.
Blocker text, decoded terms and response copies share the request memory budget.
Intermediate missing atoms are unfolded before the final distinct-fact limit is applied.
Only verified smaller supports prune supersets, and an unverified program-budget case
makes the search incomplete. Geometry overlays recompute synthetic defaults from their
combined asserted facts before verification. The implicit single-geometry rule can
also supply a missing geometry premise.
The search reports truncation through `complete`. It uses additional bounds on matching,
partial instances and verified hypotheses, and it declines hypotheses for term-generating
rules. Candidate rules are still listed. It ranks by size, vocabulary and equality
changes, proof length and placeholders, preserving acceptance example A3's useful range
and subclass hypotheses.

The HTTP route, library, CLI, MCP tool and Python and Node bindings expose both modes.
Explore shows rule details and a Why-not form. `why_empty` attaches verified hypotheses
to a grounded failing intensional pattern at the diagnosis's own visible snapshot.
Query-cell actions remain roadmap item 285, as the phase plan allowed. Pattern abduction
with unbound target variables remains item 286 and needs a target-pattern API beyond
§2.1's ground triple.

RDFS schema scans and closure builds check the query or explanation controls. Mixed
RDFS-on-read derivations graft the materialized premise's proof at its recorded commit,
including an explicitly selected stale materialization. Unsupported physical inferred
leaves are rejected before the RDFS search selects a proof, so a supported mixed
alternative remains available. The response's freshness note
applies to that inferred component. These changes complete roadmap items 135 to 139.

Partial-tree depth and predecessor tables stay aligned with facts even when a deadline
interrupts expansion. Geometry construction uses the compiled session term dictionary
and checks cancellation in both scans and output construction. Generalized references
accept literal subjects and blank-node predicates through JSON and text why requests.
Generalized traversal within a materialized explanation retains the selected historical
snapshot and response provenance. Materialized subtrees carry a read context that refs
preserve when a mixed RDFS tree cuts them, and the panel expands against that context.
Rule-level blank nodes use stable opaque response labels distinct from generated nodes.
Source constants with encoded-looking labels remain distinct. Why-not does not propose
compiler-local blank constants as insertable RDF facts, and blank-head rules make its
search incomplete, as term-generating rules do. Nested builtin failures
do not render blocker text when the candidate is not part of the response.
The generated client types include candidates, blockers, missing-fact hypotheses and ref
read contexts, with a compile check that consumes the explanation fields.

Phase 3 remains below item 18. No on-disk format changes are needed.

### Phase 2, 2026-10-25

Read-only entailment comparisons cover branch heads, retained commits and complete
write or merge candidates. Both sides use the same requested or recorded program and
their own query-visible graph grants. Fresh stored graphs are compared using their exact
terms and retained branch identities. Full fallback evaluations share a bounded semantic
witness map for generated blank nodes and a fixed request clock. Program identity,
builtin occurrence, bound RDF terms and recursive evaluation round preserve generated
witness identity across unchanged states without changing ordinary reasoning semantics.
Stored generalized datatype summaries are read under the same controls. Missing exact
summaries leave the datatype check incomplete rather than recomputing volatile values.

The general comparison uses full private materializations when the existing incremental
API cannot safely preserve the required program or term semantics. It streams source
quads into private views, then retains the two closures and bounded result structures.
This trades memory and repeated reasoning work for isolation. Retained state, witness,
summary and result bounds refuse oversized analyses. No speed claim is made for this
fallback. The source store, materialization records and blank-node allocator stay intact.

Findings and class probes use bounded contraction, single-deletion certification and
hitting-set enumeration. Nonmonotone programs use exhaustive cardinality checks.
Every emitted support is subset-minimal, including grouped diagnostics findings.
Alternative conflicts about a probe remain valid across contraction, and anonymous
classes are included in relevant class discovery. Initial diagnostics supports use the
same visible snapshot and controls as the report. Why-not hypotheses compare their
candidate against baseline inconsistency findings, preserving unknown markers when
the shared budget cannot complete the check.

HTTP, CLI, MCP, Rust, Python, Node, FFI and JVM surfaces expose comparisons and supports.
Merge and write previews use retained full candidate views rather than reconstructing a
state from listed changes. Replay previews validate each source step against its private
predecessor, including graph-backed schemas and insertion/storage admission, and retain
the complete candidate without publishing intermediate states. The UI displays entailment changes on merge and review pages,
minimal supports and class probes in diagnostics, and a removal preview on a fresh branch
with an explicit apply step. The removal patch retains stored blank identity and pins the
draft branch's UUID and head. Generated OpenAPI types describe triples and supports and
keep inference explanation requests separate from query-plan explanations.

### U10 Phase 3, 2026-10-24

U10 Phase 3 built the first engine of §3 for the `why` mode, a backward search over the
materialized closure that accepts a tree only when every fact in it has a well-founded
depth, and an explainer for RDFS on read. It answers with the `Derivation` nodes of §2.1,
shares repeated facts by `id`, cuts nodes past `maxDepth` as `ref`, and answers a hidden
derivation with the same `404` as no derivation, as §7.3 asks. The route, the library call
`Dataset::reasoning().explain`, `sparkles infer --why`, the MCP tool `explain_inference`,
the Python and Node bindings and the UI's Why? panel follow §2 for that mode, and `whyNot`
answers `501`.

Phase 1 can keep the answer types and `finish` in `crates/sparkles-core/src/explain.rs`,
the route and its access and budget answers, and every surface above. It adds `whyNot`
behind the same route and puts its demand engine in place of the search in
`crates/sparkles-reasoner/src/explain.rs`. The tests in
`crates/sparkles-reasoner/tests/explain.rs` check well-foundedness and visible leaves on
any tree, so they can compare the two engines on the same closures. The limits U10 left
are ROADMAP items 135 to 137, a partial tree when the budget runs out, derivations through
generalized triples and default geometries, and the builtins and bindings of a rule in the
tree.

### Roadmap follow-ups (2026-10-23)

[U10 §3.3](U10-canvas-lenses.md#33-phase-3-prerequisite-explanations-over-a-materialized-closure)
proposes a first engine for the `why` mode of `POST /$/reason/{ds}/explain`, so the UI can
explain inferred edges before F17 exists. It answers with the `Derivation` nodes of §2.1
and searches rule instances over the materialized closure by iterative deepening, which
accepts a tree only when every premise is asserted, an axiom or derived at a smaller
depth. §3.1's objection to the kept closure concerns reading first derivations from its
row order, and the search does not depend on that order. `whyNot` answers `501` from
that engine. When Phase 1 lands, its demand engine replaces the search behind the same
route, and the tests compare the two on the same closures. U10 Phase 3 builds the search
before F17, as U10's open question 1 proposed.


### Supported visibility and evidence review

Roadmap item 18's remaining Phase 3 includes the shared evidence profile of
[C25](C25-evidence-and-assessments.md) and maintained assessments of
[F22](F22-analysis-lifecycle.md). These are planned contracts, not behavior shipped
with the explanation and entailment comparison phases.

A derived triple is visible only when a kept complete derivation has asserted leaves
visible to the caller. Under reviewed-only input selection, that same derivation must
use only facts admitted by the selected review policy. Review decisions address exact
claim and source revisions. A reviewed source label cannot admit a later unreviewed
revision, and a reviewed hypothesis does not become an asserted premise without C25's
separate admission step.

Visibility uses the current principal and policy over the selected retained snapshot.
Historical access rights do not grant present access. A cache key includes the dataset
and branch identity, input commit, rule program, access fingerprint and review-policy
revision. Revoking a grant or a review admission invalidates the applicable witnesses
and caches before another result is served. A complete visible alternative may keep a
fact readable after another support disappears. Truncated witness sets remain marked,
and their failure to contain a visible derivation hides the result conservatively.

The implementation must test negative answers, aggregates, path results, diagnostics,
explanations, GraphRAG input and exports as well as individual triples. A hidden
derivation must not leak through counts, missing-premise suggestions or status details.
F22's application support lists do not replace a verified logical derivation. Until the
provenance operator of F13 Phase 3 exists, explicit authorized projections and isolated
datasets remain the way to publish results across different trust boundaries.

Acceptance includes a fact with two independent derivations, one whose support is
revoked and one that remains visible, and a fact whose only review-admitted support
is withdrawn. Differential tests compare filtered answers with a fresh derivation over
only the caller's admitted inputs. Unavailable witnesses must never make the supported
mode silently fall back to showing the entire inferred graph.
