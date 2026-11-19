# Charged buffers

## Problem

A cursor operator charges the query's memory budget for the state it keeps between and
within pulls. Several operators have charged a size computed by a formula, such as the
bytes a hash table of a given capacity should allocate, and grown their state at several
places, each with its own charge. Three kinds of defect follow from this. A formula
can fall short of the allocation it stands for. A growth site can allocate before it
charges, or not charge at all. A charge can be released while its storage is still
allocated. None of them changes an answer, so the tests that compare answers do not see
them, and each has been found separately in a different operator.

## Design

### Charged buffers

`Charged<V, A>` in `crates/sparkles-core/src/sparql/charged.rs` owns one collection `V`
and the account `A` that holds its charge. The collection is a `Vec`, a hashbrown
`HashTable` or a hashbrown `HashSet`, and its size is read from the collection itself,
as a vector's capacity times its element size or a table's `allocation_size`. The
account is the type the operator already used, a `RetainedCharge` or an `OwnedCharge`,
so moving an operator onto `Charged` buffers does not move it between the query's lock-free
account and its ledger. An eager context without a cursor owner has no retained charge,
and a buffer on it is uncharged.

A buffer grows only in `Charged::grow`. It admits the old allocation and the new one
together before it allocates, since both are live while the entries move, and then sets
the charge to the size the collection reports. A vector doubles where the budget allows
and grows to its exact need where it does not. A hash table's new size is not known
before it allocates, so the caller passes an upper bound, `table_bound`, which is
admitted and then replaced by the real size. A refused admission leaves the buffer and
its charge as they were.

The charge is lowered only after storage is freed, by `shrink`, `clear_free` or drop.
The collection is declared before the account, so on drop it is freed first. Callers
reach the collection through `Deref` and through named mutators, never through a `&mut`
to it, so no caller has a second growth site. Debug builds assert that the charge equals
the allocation at each growth and that an insert has room reserved for it.

Each buffer has its own charge. An operator with several buffers, such as DISTINCT's
hash table and row arena, grows them one after the other, so its transient peak is the
larger of the two growths and not their sum.

### Cost

A charge moves only when a buffer's allocation changes, which a doubling vector does a
logarithmic number of times in its length. Operators reserve the room for a whole batch
before they insert its rows, so the per-row path is a plain insert with a debug
assertion. The growth path is marked cold. No buffer allocates a fixed-size cache.

### The audit

`crates/sparkles-core/tests/common/heap.rs` is one harness for every audited operator.
It counts the blocks that a counted thread allocates inside a window, by address, so
a free lowers the count only for a block the window counted, whichever thread frees it.
The counted threads are a one-thread Rayon pool, which takes the query's parallel work,
and the engine's block decoders. While a cursor's pulls run, the allocator compares the
counted heap with the bytes the query has admitted, read exactly through the hidden
`QueryCursor::memory_meter`, at every counted allocation. After every pull it compares
both sides again. A pool grant's `used` grows in steps of 64 KiB and is given back only
1 MiB late, so the audit never reads it.

The audit reports three excesses. The heap above the ledger inside or after a pull is
an undercharge. The ledger above the heap after a pull is an overcharge. The ledger's
peak above five quarters of the heap's peak is an overcharge inside a pull, which
refuses budgets that the operator's real state fits.

The harness also has a budget-parity sweep, `heap::parity`. For every budget of a range
at which the run without a change fits, the run with the change must fit within a
stated slack and give the same answer. A sweep that crosses no threshold fails, since
it checks nothing.

`crates/sparkles-core/tests/cursor_memory_audit.rs` runs one fixture for each cursor
operator, under the query's own account and a pool grant, with batches of 256 and 4,096
rows. An operator whose charges follow its allocations is held to the exact allowance of
16 KiB on each side, which covers its plan and cursor, and 64 KiB at its peak. A fixture
with a known finding is held to its measured excess rounded up, under the item that owns
it, so a change that makes it worse fails.

## Findings

These readings come from the first run of the audit over the operators as they were.

| Fixture | Reading | Item |
|---|---|---|
| leaf scan | The ledger stands about 1 MiB above a heap of 12 KiB. | 1184 |
| group count, ordered group | The heap passes the ledger by up to 108 KiB and 750 KiB inside a pull. | 1185 |
| group concat | The ledger stands about 1 MiB above the heap after its pull. | 1185 |
| top k | The ledger stands about 62 KiB above the heap after its pull. | 1186 |
| sort | The ledger peaks at 11.1 MB for a heap peak of 2.9 MB. | 1187 |
| transitive path | The heap passes the ledger by up to 45 KiB inside a pull. | 1188 |
| path search | The heap passes the ledger by up to 340 KiB inside a pull. | 1189 |
| history | The heap passes the ledger by up to 410 KiB inside a pull. | 1190 |
| text | The heap passes the ledger by up to 450 KiB inside a pull. | 1191 |

The scans, filters, BIND, the merge and hash joins, UNION, MINUS, EXISTS, DISTINCT, GRAPH,
sequence paths and slices are within the exact allowance.

## Acceptance criteria

1. Every cursor operator has a fixture in the audit, and a fixture without a named item
   holds the exact allowance.
2. For each operator moved onto `Charged` buffers, removing one of its charges fails its
   fixture.
3. A moved operator fits every budget that it fitted before the move within a slack that
   its tests state, as `heap::parity` checks.
4. Instruction counts per request of a query that runs a moved operator show no
   increase beyond the host's noise floor when no budget binds, and a timing run on an
   Intel host confirms it.

## Outcome

The `Charged` type, the shared audit and a fixture for each cursor operator are in
place. The first run's findings are recorded above with their items. Moving DISTINCT and
the external sort onto `Charged` buffers remains under item 1183, and the operators that
other work is rewriting move under item 1192.

## Provenance

The design comes from Sparkles' own operators and their audits. It uses no outside
source beyond the documented `allocation_size` of the hashbrown crate, a workspace
dependency.
