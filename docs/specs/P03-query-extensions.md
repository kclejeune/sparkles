# P03: Registered query functions, property functions and aggregates

> **Status:** partially implemented (Rust scalar, aggregate and property registries)
>
> **Phases:** Rust scalar registry first; property functions and aggregates next;
> foreign-language callbacks after the Rust execution contract is tested. Rust
> direct-IRI scalar registration and explicit single-argument `AGG` registration
> and captured-view property functions are implemented; dynamic registered dispatch
> and foreign callbacks remain open. Existing built-in
> extension IRIs continue to work.
>
> **User docs:** [Registered scalar functions](../USAGE.md#registered-scalar-functions),
> [registered aggregates](../USAGE.md#registered-aggregates),
> [registered property functions](../USAGE.md#registered-property-functions).

## 1. Purpose and scope

An embedding application should be able to supply an IRI-addressed function and
execute the surrounding query in Sparkles. Today the engine dispatches a fixed
extension catalog, and Jena must execute queries with application functions in
ARQ. Registration must preserve SPARQL errors, dataset access restrictions,
snapshot consistency, budgets and cancellation.

This design adds immutable registries to query options. It does not load code from
query text, install packages, persist executable callbacks in dataset settings or
expose a remote registration endpoint. Registration is an embedding capability.
Registered callbacks are trusted application code, not a sandbox.

SPARQL supplies the IRI-based extension-function syntax and expression-error
rules. [SPARQL 1.1 Query](https://www.w3.org/TR/sparql11-query/#extensionFunctions)
is the semantic reference. Jena distinguishes scalar functions from property
functions that can produce bindings; that distinction is retained here.
[Jena scalar functions](https://jena.apache.org/documentation/query/writing_functions.html),
[Jena property functions](https://jena.apache.org/documentation/query/writing_propfuncs.html).

## 2. Registry ownership and resolution

`ExtensionRegistry` is an immutable, reference-counted value. A builder registers
absolute IRIs with scalar, property-function or aggregate descriptors and refuses
duplicates within each category. Scalar functions and aggregates share expression
syntax, so an IRI cannot be registered in both categories; those collisions also
include built-ins. Property functions may reuse an expression IRI because their
triple-pattern syntax is distinct. Registering an application callback cannot
replace a built-in. Independent registries can use the same application IRI.

`QueryOptions.extensions` holds an optional registry. Planning captures that
registry for the lifetime of the query and its result iterator. A prepared query
retains parsed syntax, not a callback registry: each run receives query options
and plans against that run's registry identity. Replacing an application's
registry affects later queries;
already running queries retain their original callbacks. No global mutable
registry or process-wide application callback lookup is introduced.

Descriptors include the IRI, supported arity, volatility, an application-defined
description and an implementation handle. Registry identity is unique within a
process. Reusing a registry preserves identity; building another creates a new
identity even when its IRIs match. Plans and cache entries cannot cross registry
identities. Unknown function IRIs retain the existing expression-error behavior.
Unknown property-function IRIs remain ordinary RDF predicates.

## 3. Scalar execution contract

The initial public contract uses owned RDF terms, not internal vocabulary IDs.
A `ScalarFunction: Send + Sync` receives a bounded batch of evaluated argument
rows and a read-only execution context and returns one result per input row.
Each row's result is either an RDF term or a SPARQL expression error. A default
adapter supports a scalar `call` implementation; native or foreign implementations
can override the batched method. Output cardinality is checked.

Evaluation retains existing short-circuit behavior for IF, COALESCE, AND and OR.
An unbound argument or argument-expression error follows the normal SPARQL rules
before invoking the callback. A callback domain/type error is an expression error:
BIND leaves that variable unbound and FILTER applies the existing error rules.
Cancellation, exhausted budgets and invalid callback protocol results abort the
query; they are not converted into a filtered-out row. Rust panics at a callback
boundary become a query execution error when unwinding is available. There is no
guarantee of recovery from aborts or foreign process faults.

A fatal callback failure also prevents an enclosing captured write transaction
from committing, even if application code suppresses the query error. Earlier
uncommitted changes are rolled back. Ordinary expression errors do not abort the
transaction, and a new transaction remains usable after a fatal failure.

The context exposes the query timestamp, cancellation checks and budget charging.
Scalar callbacks receive no raw store, writer, catalog or network client. Scalar
callbacks must not open nested queries or writes on the same execution family.
An execution-family token includes the captured dataset and branch identities,
including aliases and any transaction owner. Rust callback dispatch and foreign
dispatch install that token on the actual callback thread or asynchronous context;
all supported embedding operations check it before waiting for writer locks or
dispatching nested queries. A caller-thread-only ThreadLocal is insufficient for
a foreign callback dispatched from a native worker. The callback's own external
work remains the application's responsibility.

The first Rust slice resolves registered scalars through direct IRI calls. Dynamic
dispatch through `fn:apply` or `afn:eval` retains the existing built-in catalog;
registered dynamic dispatch requires a separate optimizer/cache activation design.

Input batches and decoded output batches have both row and byte ceilings. The
engine charges input/output terms and retained property-function buffers against
the query budget before exposing them to another operator. Accumulators explicitly
charge retained memory. A callback retaining application-owned copies outside that
accounting is trusted code; the engine cannot bound its private heap. Inputs remain
valid only for the documented call or owned-copy lifetime, and cancellation drops
pending batches and streams. Stored blank-node identity survives conversion; newly
minted callback blank nodes use a query-local namespace and cannot forge stored
blank nodes by returning an arbitrary vocabulary ID or label.

## 4. Volatility and optimizer rules

Descriptors choose `Immutable`, `Stable` or `Volatile`, with `Volatile` the default.
Immutable results depend only on arguments. Stable results may depend on this
query's captured context. Volatile results may change between evaluations. These
names follow an established query-engine distinction, but Sparkles' rules below
are the contract.
[DataFusion volatility](https://docs.rs/datafusion/latest/datafusion/logical_expr/enum.Volatility.html).

Initial implementation performs no custom-function constant folding or callback
memoization. All queries invoking callbacks bypass the result cache. Later cache
support requires evidence that registry identity, captured dataset/view identity
and volatility are represented correctly in the key. Optimizations may not
duplicate, eliminate or move a volatile call across operators if doing so changes
its observed evaluations. A batch preserves input order; it does not promise a
global evaluation order across independent algebra operators.

Empty registries preserve the current dispatch path. Queries that never reference
a registered extension must avoid per-row callback allocations. EXPLAIN records
extension kind, IRI, volatility and batching eligibility, without serializing
callback objects or application secrets.

## 5. Property functions

A registered fixed predicate IRI is translated into a property-function operator.
Descriptors declare subject/object term-or-list shapes, required bound positions,
positions they may bind and an optional conservative cardinality estimate. The
planner may place the operator only after its required bindings are available.
Registration does not reinterpret a variable predicate or a property path.

The callback receives input solutions and a view of the active graph. Its output
is a bounded, pull-based stream of solution extensions associated with the input
row. It must preserve existing bound values and repeated-variable equality;
incompatible outputs are rejected by the engine rather than overwriting inputs.
Bag semantics are preserved. OPTIONAL, UNION, subqueries and graph selection use
the ordinary algebra rules. The engine charges produced rows and checks cancel
between pulls and batches; closing results closes the callback stream.

The view can scan only the captured snapshot with the caller's graph and triple
filters and FROM/FROM NAMED selection applied. It cannot reveal hidden graph
names, unfiltered statistics, raw storage IDs or a writable dataset handle.
Reads share the query budget. A nested SPARQL executor is excluded initially.

## 6. Aggregates

An `AggregateFactory` creates one accumulator per query group. No accumulator is
shared between groups or query invocations. It accepts bounded argument batches,
reports retained memory through the budget context and produces one RDF term or
expression error on finalization. Empty-group behavior is explicit and tested.

The engine applies DISTINCT and the existing argument-error rules before passing
values to the accumulator. Aggregate descriptors state arity and volatility.
Registration does not override standard aggregates or change GROUP BY/HAVING
scope rules. Parallel partial aggregation is disabled initially; a future merge
capability requires an explicit associative contract and separate tests.

The parser already supports custom aggregate IRIs for built-in extensions. The
implementation must test application IRIs through parsing, algebra formatting and
execution without hard-coding registered IRIs into the parser.

## 7. Binding callbacks and lifecycle

JVM callbacks use UniFFI foreign interfaces and the existing batched RDF-term
encoding. Registration retains strong callback references until the last query
using that registry closes. Adapter methods translate Jena nodes and expression
errors, with one foreign call per batch. Jena selects native execution only for
callbacks representable by this contract; other Java extensions keep ARQ fallback.
No JNI transport rewrite is implied by callback support.

Python callbacks run with the GIL held for the callback only. Node callbacks run
on the originating environment through its supported thread-safe dispatch, never
directly from a Rust worker. Node registries belong to one environment and cannot
be used from a different Worker. Callback references and dispatch queues are owned
by that environment, with explicit batch-byte and in-flight limits and backpressure.
Its closure cancels outstanding requests without waiting for JavaScript execution
and releases callback references. The first Node adapter supports synchronous
callback results only; Promise results fail with a protocol error. A later async
adapter must release Rust waits on cancellation/deadline and discard late results.
Cancellation cannot interrupt JavaScript that blocks its event loop. Immutable
asynchronous-context owner sets propagate nested callback families. Unsupported
synchronous/reentrant execution
must fail promptly rather than waiting on its own worker or event loop. Foreign
exceptions become bounded, sanitized expression or execution errors according to
the distinction in section 3. Sanitization does not invoke arbitrary user-object
stringification. Property-function scans use the dedicated captured view, avoiding
ordinary dataset admission queues; closing results disposes callback iterators and
accumulators. These adapters need their own design and tests before
their public APIs are enabled.

## 8. Acceptance examples and validation

* A1: two registries register the same IRI with different implementations; parallel
  queries return the appropriate result without leaking callbacks across registries.
* A2: a query keeps its callback after the application's registry is replaced or
  dropped; closing the result releases the final reference.
* A3: wrong arity, unknown IRI and callback expression errors retain SPARQL behavior
  in FILTER, BIND, OPTIONAL, IF and COALESCE, including short-circuit cases.
* A4: zero arguments, unbound inputs, language/datatype literals, quoted triples,
  stored blank nodes and query-created blank nodes survive batch conversion.
* A5: a volatile callback is neither cached nor evaluated on an unselected IF arm.
* A6: a property function cannot see a hidden graph/triple, overwrite a bound value
  or escape FROM/FROM NAMED. Output multiplicity is preserved.
* A7: aggregation covers empty groups, DISTINCT, argument errors, HAVING and
  simultaneous groups; cancellation and memory limits release every accumulator.
* A8: callback output-length violations, cancellation, foreign exceptions,
  environment closure and attempted reentrancy terminate without deadlock.
* A9: existing SPARQL conformance tests pass with no registry and an unused registry.
* A10: quiet benchmarks compare no-registry overhead, tiny scalar calls, realistic
  query calls and batched versus scalar foreign dispatch. Public claims use measured
  results; no transport change is selected from loaded-machine timings.

## 9. Delivery and outcome

Phase 1 implements Rust registries, scalar calls, error mapping, cache bypass and
planner safeguards with A1–A5, A8–A10. Phase 2 adds property functions and aggregates
with the access-filtered context and their lifetime/budget tests. Phase 3 adds JVM
callbacks first, then Python/Node adapters once their dispatch contracts are proved.
Each phase updates API docs, extension descriptions and binding-map coverage.

**Outcome:** the Rust scalar slice provides immutable registry builders and identities,
arity/collision checks, owned RDF terms, volatile-by-default descriptors, callback
error/panic handling, bounded argument/output terms and cancellation/memory charging.
Queries that reference callbacks bypass caches and unsafe optimizer rewrites;
queries without referenced registrations retain their existing plans. Captured
writer families propagate to worker callbacks, and fatal failures prevent a caller
from suppressing an error and publishing partial transaction changes.

The batch interface currently receives singleton rows; cross-row batching is not
implemented. Twenty focused regressions and the full core suite pass, including
short-circuit evaluation, ORDER BY callback counts, cache isolation, RDF/blank-node
validation and bounded transaction deadlock/rollback cases.

The Rust aggregate slice adds immutable descriptors/factories and owned per-group
accumulators. Explicit `AGG <iri>(expr)` syntax supports one argument and keeps
prepared syntax independent of execution registrations. Factory, add, finalization
and destruction share the guarded panic, fatal-error, cancellation and budget
boundaries. DISTINCT, empty groups, domain errors, retained state and received
blank-node identity are covered. Non-keyword custom aggregate formatting preserves
`AGG` across parse/format/reparse without a mutable parser registry. Execution
supplies singleton batches, with no parallel partial aggregation. Twenty-four
focused aggregate regressions pass alongside the scalar and existing ARQ suites;
the full core and formatter suites and default/minimal/featured strict lint pass.

The Rust property slice adds term/query-written-list descriptors, required and
produced positions, query-borrowed contexts/read views and owned synchronous pull
streams. An opaque local-BGP attachment barrier runs ordinary patterns and prior
input first, then orders dependent property calls without moving them across
algebra operators. Every input row remains a separate invocation, including
OPTIONAL/LATERAL duplicates. Schema-missing inputs fail before that operator opens;
EXISTS errors occur when its dynamically planned pattern is entered, rather than
through whole-query preflight. Runtime-unbound required positions yield no
extensions. An open/next expression error discards that invocation's provisional
outputs; fatal failures abort the query and prevent transaction commit. Returned
streams are installed in guarded ownership before cancellation postchecks, and
all destruction runs inside the same guarded boundary.

Captured SPO reads enforce graph, triple and dataset filters, deduplicate merged
graphs and expose bounded query-borrowed pages/cursors without an executor or
storage handle. Consumed, matching, permitted raw candidates count toward work
before deduplication; excluded traversal checks cancellation/deadline without
reporting hidden cardinality. Input slots, scan patterns/decoded batches, blank
ledgers, unique permission projections, provisional output capacity and retained
terms are charged. Domain-error truncation reuses charged column capacity while
discarded work remains counted.

Shared callback conversion preserves received blank identity, including CDT
contents in aggregate arguments and permitted property reads. Arbitrary returned
labels cannot forge stored identities. Original label-map keys are charged once,
separately from actual mapped vocabulary bytes; converted terms are checked
before admission. Callback-only CDT preflight bounds bare/quoted nesting and
conservatively accounts materialized parser/relabel storage. Its one-MiB term
ceiling does not promise that a finite query memory budget admits the term;
ordinary CDT operations retain their existing path. The separate checked Charge
correction prevents local/global accounting overflow without partially updating
live counts.

Thirty-five property, twenty-four aggregate and twenty scalar regressions pass
(79 focused), alongside 820 full core tests with 11 existing ignored tests and
95 feature-focused tests. Strict default/minimal/featured all-target lint,
formatting and diff checks pass. Ordinary/unused-registry layout remains unchanged;
no callback-CDT throughput improvement is claimed.

Dynamic registered dispatch, foreign-language callbacks, cross-row batching,
broader aggregate arities and direct-IRI aggregate classification remain open.
Quiet callback overhead measurements remain open. This implementation adds no
dependency.
