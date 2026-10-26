# C29: Bounded preparation of read views

> **Status:** specified.
>
> **Phases:** Additive bounded history reads come first, followed by controlled
> permission preparation and adoption by callers that require an operation limit.
> No phase has shipped.

## Goals

A request can need substantial work before its query or fact lookup begins. Retained
state may require WAL replay, while a permission view may require class closure,
pattern evaluation and a mask. A query execution limit alone does not bound that
preparation. A shared history-cache capacity also does not express one request's limit.

Provide explicit memory, cancellation and deadline limits across this work. A
successful bounded read preserves the exact requested state, RDF terms and access
policy. A refused read returns no partial view. The same preparation supports evidence
references, guarded analysis inputs, exports and ordinary historical queries.

For example, an advisory analysis can verify an inventory fact at a retained commit
under its caller's grants. It either obtains that exact authorized view within its
limit or refuses. It cannot substitute the latest inventory or omit a costly policy.

## Existing behavior and boundary

C12b already limits hidden quads and pattern results. F06 bounds its shared history
cache and accepts cancellation and deadlines for historical operations. This design
adds a request-scoped preparation contract, including work before those existing
checks and fast paths that reuse state.

Existing entry points retain their established store-level limits when no request
cap is selected. A bounded caller must never fall back to them after a refusal. No
on-disk layout, source authority, managed-writer permission or historical selector
meaning changes.

## Shared preparation budget

Use one cap across retained delta state, replay and decode buffers, permission work,
and derived indexes or masks needed by the view. Sequential helpers consume the
remaining allowance rather than each receiving an independent full allowance.
Immutable index mappings and the store's shared caches retain their existing store
accounting. Their reuse cannot bypass the bounded view's declared retained-state cap.

Charge conservative peak allocation before growth. Account for vector relocation,
sparse maps, temporary decode storage and ownership transfer. Release temporary
charges when their storage is dropped. Cached views carry enough internal accounting
to check the same cap without rebuilding them or creating one cache entry per cap.

Check cancellation and deadlines before locks, while waiting, through long loops and
before every successful early return. A failure installs no partial cache entry and
publishes no data. Limit errors must not disclose hidden physical cardinalities or
exact private-control storage sizes. A limit-plus-one requested value is sufficient
when a precise value would disclose such information.

## Bounded historical reads

Add bounded variants of `Store::snapshot_at` and `Store::snapshot_at_with_branch`.
Keep existing `HistoryOptions` construction compatible. The new variants accept an
explicit per-read memory cap and share an internal optional-cap implementation with
the existing methods.

The effective memory cap cannot exceed the store's existing cache capacity. Do not
shrink that global capacity or evict unrelated entries to serve a smaller request.
Check live and cached results as well as cold reconstruction. Skip a nearby seed
that cannot fit before cloning it when a valid bounded reconstruction is available.
Preflight transaction buffers before reading them into memory, and check replay
incrementally rather than only after a large transaction has been retained.

Preserve the existing branch and inherited-origin rules. Resolve the requested
selector exactly, return its actual origin, and refuse unavailable retained state.
Never search another branch or select head because reconstruction failed. Managed
control indexes, transition indexes and origin rebinding share the same budget when
present, even though their rows are hidden from ordinary reads.

## Controlled permission preparation

Add a controlled bounded variant of `GraphAccess::masked`. Propagate its budget and
Control through graph and predicate enumeration, class closure, pattern evaluation,
caller attributes, exemptions, hidden-key ordering and the final mask structures.
A cached mask must satisfy the request cap before use.

Pattern-query planning, execution and results consume this same allowance. Preserve
all rules and existing visibility semantics. Do not approximate a class or pattern
policy with point checks merely because that would be cheaper.

Guard providers need an explicit controlled preparation contract. A compatible
default may refuse a provider that cannot guarantee it. It must not call an
uncontrolled callback and present the result as bounded. Adopt the built-in providers
used by the supported bounded entry points. Unsupported providers produce a clear
incomplete or limit refusal without a partial authorized view.

## Caller adoption

Expose bounded Dataset reads and use them for operations that promise a complete
preparation limit, including exact retained evidence resolution under C25. Wire the
same contract into query entry points that already accept the relevant request
limits. Preserve term, graph, branch and error semantics under sufficient budgets.

A returned view or verification result is data, not a managed-write or authenticity
token. C25 still verifies record identity and any typed admission origin separately.
Provider-specific inability to prepare a bounded view cannot be treated as proof
that a requested fact is absent.

## Phases and acceptance

Phase 1 delivers bounded history entry points. Tests cover live, cached and cold
states, oversized seeds, transaction buffers, lock contention, cancellation, missing
retained state and inherited branch origins. Sufficient limits return the same state
as the existing read. Actual allocation controls prove early refusal where a final
budget error alone would miss an oversized temporary allocation.

Phase 2 delivers controlled permission preparation and provider support. Differential
tests compare all current graph, predicate, class, subclass, caller, pattern and
exemption behavior. Exercise low caps, cached masks, temporary-charge release, large
enumerations and cancellation within preparation. Failure returns no partial mask.

Phase 3 adopts the contract in bounded Dataset and query paths and C25's exact
reference reader. Test caller changes, historical selectors, guarded views and uniform
hidden-or-missing behavior. Old signatures retain their documented scope. The item
is complete only when the adopted bounded callers use the real preparation contract.

Run affected history and access suites, strict Clippy, the full integration gate and
the regression screen. Measure material changes to preparation memory and latency.
Record accepted costs and follow-ups under the repository's performance rules.

## Provenance

This design derives from Sparkles' existing C01 resource limits, C12b permission
semantics, F06 history APIs and C25 reference requirements. It extends the current
implementation rather than importing an external engine or dependency. No external
implementation source is used.

## Outcome

No phase has shipped. Existing hidden-quad and pattern-row limits and the shared
history cache remain available. Request-scoped historical and permission preparation,
its built-in provider adoption and the bounded caller integration remain item 395.
