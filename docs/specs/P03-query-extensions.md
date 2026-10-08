# P03: Registered query functions, property functions and aggregates

> **Status:** implemented in part (Rust scalar, aggregate and property registries)
>
> **Phases:** The Rust scalar registry comes first. Property functions and aggregates
> follow it. Foreign-language callbacks come after the Rust execution contract is
> tested. Rust scalar registration by direct IRI, single-argument aggregates called
> with `AGG`, and property functions over the captured view are implemented. Dynamic
> registered dispatch and foreign callbacks remain open. Existing built-in extension
> IRIs continue to work.
>
> **User docs:** [Registered scalar functions](../USAGE.md#registered-scalar-functions),
> [registered aggregates](../USAGE.md#registered-aggregates),
> [registered property functions](../USAGE.md#registered-property-functions).

## 1. Purpose and scope

An embedding application should be able to supply a function addressed by an IRI and
still run the surrounding query in Sparkles. Today the engine dispatches a fixed
catalog of extensions, so a query that uses application functions must run in Jena's
ARQ. Registration must preserve SPARQL errors, dataset access restrictions, snapshot
consistency, budgets and cancellation.

This design adds immutable registries to query options. It does not load code from
query text, install packages, persist executable callbacks in dataset settings or
expose a remote registration endpoint. Registration is a capability of the embedding
application. Registered callbacks are trusted application code and do not run in a
sandbox.

SPARQL defines the syntax of IRI-based extension functions and the rules for
expression errors, and [SPARQL 1.1 Query](https://www.w3.org/TR/sparql11-query/#extensionFunctions)
is the semantic reference. Jena distinguishes scalar functions from property
functions, which can produce bindings, and this design keeps that distinction. See
[Jena scalar functions](https://jena.apache.org/documentation/query/writing_functions.html)
and [Jena property functions](https://jena.apache.org/documentation/query/writing_propfuncs.html).

## 2. Registry ownership and resolution

`ExtensionRegistry` is an immutable, reference-counted value. A builder registers
absolute IRIs with scalar, property-function or aggregate descriptors and refuses
duplicates within each category. Scalar functions and aggregates share expression
syntax, so one IRI cannot be registered in both categories, and this collision check
includes the built-ins. A property function may reuse an expression IRI because its
triple-pattern syntax is distinct. An application callback cannot replace a built-in.
Independent registries can use the same application IRI.

`QueryOptions.extensions` holds an optional registry. Planning captures that registry
for the lifetime of the query and its result iterator. A prepared query keeps its
parsed syntax but no callback registry. Each run receives query options and plans
against the identity of that run's registry. Replacing an application's registry
affects later queries, and queries that are already running keep their original
callbacks. There is no global mutable registry and no process-wide lookup of
application callbacks.

A descriptor holds the IRI, the supported arity, the volatility, a description that
the application defines and an implementation handle. Each registry has an identity
that is unique within the process. Reusing a registry keeps its identity, and building
another creates a new identity even when the IRIs match. Plans and cache entries never
cross registry identities. Unknown function IRIs keep the existing expression-error
behavior. Unknown property-function IRIs remain ordinary RDF predicates.

## 3. Scalar execution contract

The initial public contract uses owned RDF terms rather than internal vocabulary IDs.
A `ScalarFunction: Send + Sync` receives a bounded batch of evaluated argument rows and
a read-only execution context, and it returns one result per input row. Each row's
result is either an RDF term or a SPARQL expression error. A default adapter supports
implementations that provide a scalar `call`, and native or foreign implementations can
override the batched method. The engine checks that the output has one result per row.

Evaluation keeps the existing short-circuit behavior of IF, COALESCE, AND and OR. An
unbound argument or an error in an argument expression follows the normal SPARQL rules
before the callback is invoked. A domain or type error from the callback is an
expression error, so BIND leaves the variable unbound and FILTER applies the existing
error rules. Cancellation, an exhausted budget or an invalid result from the callback
protocol aborts the query instead of filtering out the row. A Rust panic at the
callback boundary becomes a query execution error when unwinding is available.
Recovery from aborts or from faults in a foreign process is not guaranteed.

A fatal callback failure also prevents an enclosing captured write transaction
from committing, even if application code suppresses the query error. Earlier
uncommitted changes are rolled back. Ordinary expression errors do not abort the
transaction, and a new transaction remains usable after a fatal failure.

The context exposes the query timestamp, cancellation checks and budget charging.
Scalar callbacks receive no raw store, writer, catalog or network client, and they must
not open nested queries or writes on the same execution family. An execution-family
token identifies the captured dataset and branch, including aliases and any transaction
owner. Rust and foreign callback dispatch install that token on the thread or
asynchronous context where the callback actually runs. Every supported embedding
operation checks the token before it waits for a writer lock or dispatches a nested
query. A ThreadLocal set only on the caller's thread would not be enough, because a
foreign callback can be dispatched from a native worker. The callback's own external
work remains the application's responsibility.

The first Rust slice resolves registered scalars through direct IRI calls. Dynamic
dispatch through `fn:apply` or `afn:eval` keeps the existing built-in catalog.
Registered dynamic dispatch needs a separate design for how the optimizer and the cache
activate it.

Input batches and decoded output batches have both row and byte ceilings. The byte
ceiling is 1 MiB. One argument row whose terms exceed it, or one returned term that
exceeds it, aborts the query in the same way as an exhausted budget. It is not an
expression error that leaves a BIND variable unbound, because the row never reaches
the callback and its answer is unknown. Like other fatal failures, it prevents an
enclosing write transaction from committing.

The engine charges input and output terms and retained property-function buffers
against the query budget before exposing them to another operator. Accumulators charge
their retained memory explicitly. A callback that keeps its own copies outside that
accounting is trusted code, and the engine cannot bound its private heap. Inputs stay
valid only for the documented call or owned-copy lifetime, and cancellation drops
pending batches and streams. Stored blank-node identity survives conversion. Blank
nodes that a callback mints use a query-local namespace, so a callback cannot forge a
stored blank node by returning an arbitrary vocabulary ID or label.

## 4. Volatility and optimizer rules

A descriptor chooses `Immutable`, `Stable` or `Volatile`, and `Volatile` is the
default. Immutable results depend only on the arguments. Stable results may depend on
the captured context of the current query. Volatile results may change between
evaluations. The names follow a distinction that other query engines established, but
the rules below are the Sparkles contract. See
[DataFusion volatility](https://docs.rs/datafusion/latest/datafusion/logical_expr/enum.Volatility.html).

The initial implementation does no constant folding or memoization of custom-function
calls. Every query that invokes a callback bypasses the result cache. Cache support
later needs evidence that the key represents registry identity, the identity of the
captured dataset and view, and volatility correctly. An optimization may not
duplicate, eliminate or move a volatile call across operators if that changes the
evaluations a caller observes. A batch preserves input order, but there is no global
evaluation order across independent algebra operators.

An empty registry keeps the current dispatch path. Queries that never reference a
registered extension must not allocate anything for callbacks per row. EXPLAIN records
the extension kind, IRI, volatility and batching eligibility, without serializing
callback objects or application secrets.

## 5. Property functions

A registered fixed predicate IRI is translated into a property-function operator. A
descriptor declares whether the subject and the object take a term or a list, which
positions must be bound, which positions the function may bind, and an optional
conservative cardinality estimate. The planner places the operator only after its
required bindings are available. Registration does not reinterpret a variable
predicate or a property path.

The callback receives input solutions and a view of the active graph. Its output is a
bounded, pull-based stream of solution extensions tied to the input row. The output
must keep existing bound values and the equality of repeated variables, and the engine
rejects an incompatible output instead of overwriting the input. Bag semantics are
preserved. OPTIONAL, UNION, subqueries and graph selection follow the ordinary algebra
rules. The engine charges produced rows and checks cancellation between pulls and
batches, and closing the results closes the callback stream.

The view can scan only the captured snapshot, with the caller's graph and triple
filters and the FROM and FROM NAMED selection applied. It cannot reveal hidden graph
names, unfiltered statistics, raw storage IDs or a writable dataset handle. Reads share
the query budget. The view offers no nested SPARQL executor at first.

## 6. Aggregates

An `AggregateFactory` creates one accumulator per query group, and no accumulator is
shared between groups or query invocations. An accumulator accepts bounded batches of
arguments, reports its retained memory through the budget context and produces one RDF
term or expression error when it is finalized. The behavior on an empty group is
explicit and tested.

The engine applies DISTINCT and the existing argument-error rules before passing
values to the accumulator. Aggregate descriptors state arity and volatility.
Registration does not override standard aggregates or change the scope rules of GROUP
BY and HAVING. Parallel partial aggregation is disabled at first. A future merge
capability needs an explicit associative contract and its own tests.

The parser already supports custom aggregate IRIs for built-in extensions. The
implementation must test application IRIs through parsing, algebra formatting and
execution without hard-coding registered IRIs into the parser.

## 7. Binding callbacks and lifecycle

JVM callbacks use UniFFI foreign interfaces and the existing batched encoding of RDF
terms. Registration holds strong callback references until the last query that uses
the registry closes. Adapter methods translate Jena nodes and expression errors, with
one foreign call per batch. Jena selects native execution only for callbacks that this
contract can represent, and other Java extensions keep falling back to ARQ. Supporting
callbacks does not imply rewriting the JNI transport.

Python callbacks hold the GIL only while the callback runs. Node callbacks run on their
originating environment through its supported thread-safe dispatch, never directly from
a Rust worker. A Node registry belongs to one environment and cannot be used from a
different Worker. That environment owns the callback references and dispatch queues,
which have explicit limits on batch bytes and on requests in flight, and which apply
backpressure. Closing the environment cancels outstanding requests without waiting for
JavaScript to run and releases the callback references. The first Node adapter supports
only synchronous callback results, and a Promise result fails with a protocol error. A
later async adapter must release Rust waits on cancellation or deadline and discard
late results. Cancellation cannot interrupt JavaScript that blocks its event loop.
Immutable owner sets attached to the asynchronous context carry the execution family
into nested callbacks. Synchronous or reentrant execution that is not supported must
fail promptly rather than wait on its own worker or event loop. A foreign exception
becomes a bounded, sanitized expression error or execution error, following the
distinction in section 3. Sanitization does not call arbitrary stringification on user
objects. Property-function scans use the dedicated captured view and so avoid the
ordinary dataset admission queues. Closing the results disposes callback iterators and
accumulators. These adapters need their own design and tests before their public APIs
are enabled.

## 8. Acceptance examples and validation

* A1: two registries register the same IRI with different implementations. Parallel
  queries each return the right result, and no callback leaks across registries.
* A2: a query keeps its callback after the application's registry is replaced or
  dropped, and closing the result releases the final reference.
* A3: wrong arity, unknown IRI and callback expression errors keep SPARQL behavior in
  FILTER, BIND, OPTIONAL, IF and COALESCE, including short-circuit cases.
* A4: zero arguments, unbound inputs, literals with language tags or datatypes, quoted
  triples, stored blank nodes and query-created blank nodes survive batch conversion.
* A5: a volatile callback is neither cached nor evaluated on an unselected IF arm.
* A6: a property function cannot see a hidden graph or triple, overwrite a bound value
  or escape FROM and FROM NAMED. Output multiplicity is preserved.
* A7: aggregation covers empty groups, DISTINCT, argument errors, HAVING and
  simultaneous groups. Cancellation and memory limits release every accumulator.
* A8: callback output-length violations, cancellation, foreign exceptions,
  environment closure and attempted reentrancy terminate without deadlock.
* A9: existing SPARQL conformance tests pass with no registry and with an unused
  registry.
* A10: quiet benchmarks compare the overhead of having no registry, tiny scalar calls,
  realistic query calls and batched versus scalar foreign dispatch. Public claims use
  measured results, and no transport change is chosen from timings taken on a loaded
  machine.

## 9. Delivery

Phase 1 implements Rust registries, scalar calls, error mapping, cache bypass and
planner safeguards with A1–A5 and A8–A10. Phase 2 adds property functions and
aggregates with the access-filtered context and their tests for lifetimes and budgets.
Phase 3 adds JVM callbacks first, then the Python and Node adapters once their dispatch
contracts are proved. Each phase updates the API docs, extension descriptions and
binding-map coverage.

## Outcome

The Rust scalar slice provides immutable registry builders and identities, arity and
collision checks, owned RDF terms, descriptors that are volatile by default, handling
of callback errors and panics, bounds on argument and output terms, and cancellation
checks and memory charging. Queries that reference callbacks bypass caches and the
optimizer rewrites that would be unsafe for them. Queries that reference no
registration keep their existing plans. Captured writer families propagate to
callbacks on worker threads, and a fatal failure prevents a caller from suppressing
the error and publishing partial transaction changes.

The batch interface currently receives single-row batches, and batching across rows is
not implemented. Twenty focused regression tests and the full core suite pass. They
include short-circuit evaluation, callback counts under ORDER BY, cache isolation,
validation of RDF terms and blank nodes, and bounded cases of transaction deadlock and
rollback.

The Rust aggregate slice adds immutable descriptors and factories, and accumulators
owned by each group. The explicit `AGG <iri>(expr)` syntax supports one argument and
keeps prepared syntax independent of the registrations used at execution. Factory
creation, add, finalization and destruction share the guarded boundaries for panics,
fatal errors, cancellation and budgets. Tests cover DISTINCT, empty groups, domain
errors, retained state and the identity of received blank nodes. The formatter writes
application aggregates with `AGG`, so they survive parsing, formatting and parsing
again without a mutable parser registry. The built-in GeoSPARQL and `afn:` aggregate
IRIs keep the standard `<iri>(…)` call form, which endpoints other than Jena accept in
SERVICE requests. Execution supplies single-row batches, and there is no parallel
partial aggregation. Twenty-four focused aggregate regression tests pass alongside the
scalar and existing ARQ suites. The full core and formatter suites pass, and so does
strict lint in the default, minimal and featured configurations.

The Rust property slice adds descriptors for terms and for lists written in the query,
required and produced positions, contexts and read views borrowed from the query, and
owned synchronous pull streams. An opaque attachment barrier in each local BGP runs the
ordinary patterns and the prior input first. It then orders the dependent property
calls without moving them across algebra operators. Every input row remains a separate
invocation, including duplicate rows from OPTIONAL and LATERAL. An input that lacks a
required schema fails before the operator opens. Inside EXISTS, that error occurs when
the dynamically planned pattern is entered, not in a preflight over the whole query. A
required position that is unbound at runtime yields no extensions. An expression error
from open or next discards that invocation's provisional outputs, while a fatal failure
aborts the query and prevents the transaction from committing. Returned streams are
placed under guarded ownership before the cancellation checks that follow the call, and
all destruction runs inside the same guarded boundary.

Reads of the captured SPO index enforce graph, triple and dataset filters, deduplicate
merged graphs, and expose bounded pages and cursors borrowed from the query, without an
executor or storage handle. Raw candidates that are consumed, match and are permitted
count toward work before deduplication. Traversal over excluded candidates checks
cancellation and the deadline without reporting the hidden cardinality. The engine
charges input slots, scan patterns and decoded batches, blank-node ledgers, unique
permission projections, provisional output capacity and retained terms. When a domain
error truncates output, the charged column capacity is reused and the discarded work
stays counted.

Callback conversion is shared and preserves the identity of received blank nodes,
including those inside CDT values in aggregate arguments and in permitted property
reads. A returned label cannot forge a stored identity. The keys of the original label
map are charged once, separately from the vocabulary bytes actually mapped, and
converted terms are checked before admission. A CDT preflight used only for callbacks
bounds bare and quoted nesting and accounts conservatively for the storage that parsing
and relabelling materialize. Its one-MiB term ceiling does not promise that a finite
query memory budget admits the term. Ordinary CDT operations keep their existing path.
A separate correction to checked `Charge` arithmetic prevents overflow in local and
global accounting without partially updating live counts.

Thirty-five property, twenty-four aggregate and twenty scalar regression tests pass (79
focused), alongside 820 full core tests with 11 existing ignored tests and 95
feature-focused tests. Strict all-target lint in the default, minimal and featured
configurations, formatting checks and diff checks pass. The layout for ordinary queries
and for an unused registry is unchanged. No improvement in callback CDT throughput is
claimed. For A9, the W3C SPARQL 1.0 and 1.1 query evaluation tests also run with an
empty registry installed, both eagerly and through cursors, and match the expected
results.

Dynamic registered dispatch, foreign-language callbacks, batching across rows,
aggregates with other arities and the classification of aggregates called by direct
IRI remain open. Quiet measurements of callback overhead also remain open. This
implementation adds no dependency.
