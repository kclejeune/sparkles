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
`HashTable`, a hashbrown `HashSet` or `Rows`, a set of rows that pairs a table of row
numbers with a flat arena of the rows' IDs. Its size is read from the collection itself,
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

A buffer starts empty, from `Charged::empty` or `Charged::rows`, and is never built over
a collection that already holds entries. A hash table's growth is predicted from its
length and room, and a table with removed entries grows sooner than that prediction.

The charge is lowered only after storage is freed, by `clear_free` or drop. The
collection is declared before the account, so on drop it is freed first. Callers reach
the collection through `Deref` and through named mutators, never through a `&mut` to it,
so no caller has a second growth site. The mutators that add entries to reserved room
check that room in release builds too and panic before they change anything, since an
insert past the room would allocate what no admission covered. Debug builds also assert
that the charge equals the allocation at each growth.

Each buffer has its own charge. Buffers that grow together for each batch are one
buffer. `Rows` holds DISTINCT's hash table and row arena on one charge, and its growth
admits the pair before either allocates. The table grows first, while the old arena is
live, and the arena second, while the new table is live. With the old table `t`, the
table's bound `T`, its size afterwards `T'` and the arena's growth `A`, the pair admits
`max(T, T' - t + A)` bytes beyond what it holds. It tries the arena at the table's room
and then at its exact need, and a refusal leaves both buffers as they were. Two buffers
on separate charges would let the table grow and the arena be refused, which spends room
that the caller then needs for something else.

### Cost

A charge moves only when a buffer's allocation changes, which a doubling vector does a
logarithmic number of times in its length. Operators reserve the room for a whole batch
before they insert its rows, so the per-row path is a plain insert with a room check. The growth path is marked cold. No buffer allocates a fixed-size cache.

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

The meter holds its lock across the system call of every counted allocation, and of
every free and reallocation while a counted block may be live. A counted block's life is
then one ordered sequence of system calls and counts, so the live bytes are exactly what
the system holds of the counted blocks, whatever a second counted thread does at the
same moment. A meter that counts after the system call, or uncounts before it, reads
low when another counted thread frees during an allocation or a reallocation, or
allocates during a free. The meter's own test stops a counted thread at each of the
three sites and has a second counted thread make the opposite call there, and it fails
when the system calls move outside the lock. The window opens and closes under the same
lock, and its peak is read as it closes, so no call is half counted at either edge. A
block the window counted stays counted when any thread reallocates it, as a free lowers
the count whichever thread frees it. The meter's pointer to the audited query's counter
is set and cleared under the lock too, so no allocation reads a counter that is gone.
The lock serializes the counted threads'
allocations while a window is open, which slows the audited fixtures and nothing else.

A reallocated block counts its old and new sizes together, whether the allocator moves
it or grows it in place. This is the rule `Charged::grow` admits by, since whether a
block can grow in place depends on what the allocator holds beside it, which earlier
tests and queries set. Under a rule that counted a block grown in place at its larger
size, a `Charged` growth that stayed in place would read as an overcharge at its peak,
and the two-column DISTINCT fixture's heap peak read 1.28 MB or 1.73 MB from one run to
the next. `AUDIT_REALLOC=exact` selects the larger-size rule. An excess that shows only
under the default rule comes from an operator that grows a block without admitting its
old size beside the new one, and it belongs to item 1192. An excess that shows under
both rules is an undercharge for the operator's own item.

The streamed merge's allocator audit, which item 570's work adds as `cursor_alloc_audit.rs`, has
its own copy of the meter, which counts a block grown in place at its larger size and
counts outside the lock. It moves onto `tests/common/heap.rs` once both are on the
main branch, and its fixtures then read old and new sizes for blocks that grow in place.

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
rows, and a spilled fixture for each operator that spills. An operator whose charges follow its allocations is held to the exact allowance of
16 KiB on each side, which covers its plan and cursor, and 64 KiB at its peak. A fixture
with a known finding is held to its measured excess rounded up, under the item that owns
it, so a change that makes it worse fails.

## Findings

These readings come from the first run of the audit over the operators as they were.
The spilled fixtures run each spilling operator under a budget its state outgrows, so
that the sort writes and merges runs and DISTINCT passes rows on to later passes.

| Fixture | Reading | Item |
|---|---|---|
| leaf scan | The ledger stands about 1 MiB above a heap of 12 KiB. | 1184 |
| group count, ordered group | The heap passes the ledger by up to 108 KiB and 750 KiB inside a pull. | 1185 |
| group concat | The ledger stands about 1 MiB above the heap after its pull. | 1185 |
| top k | The ledger stands about 62 KiB above the heap after its pull. | 1186 |
| sort | The ledger peaks at 11.1 MB for a heap peak of 2.9 MB, from the decode of the sort's keys. | 1187 |
| transitive path | The heap passes the ledger by up to 45 KiB inside a pull. | 1188 |
| path search | The heap passes the ledger by up to 340 KiB inside a pull. | 1189 |
| spilled sort | The heap passes the ledger by up to 88 KiB inside a pull, and the merge keeps up to 269 KiB charged above the heap. | 1193 |
| spilled DISTINCT | The heap passes the ledger by up to 56 KiB inside a pull in the spill passes. | 1194 |
| history | The heap passes the ledger by up to 410 KiB inside a pull. | 1190 |
| text | The heap passes the ledger by up to 450 KiB inside a pull. | 1191 |

The scans, filters, BIND, the merge and hash joins, UNION, MINUS, EXISTS, DISTINCT, GRAPH,
sequence paths and slices are within the exact allowance.

The audit also ran on the tips of five branches that change cursor operators, under both
realloc rules, with the allowances above. Three of them read beyond an allowance, and
each excess shows under both rules, so it belongs to the operator's own item.

| Branch tip | Reading | Item |
|---|---|---|
| bf0a4c6e, native leaf cursors | The history reader keeps about 223 KiB charged above its heap after a pull, and its ledger peaks 143 KiB above five quarters of its heap's peak. | 1190 |
| 71695554, paths | The path search keeps about 71 KiB charged above its heap after a pull at batches of 256 rows. | 1189 |
| e23b0b6e, merge traps | The spilled DISTINCT holds about 68 KiB above its ledger inside a pull at batches of 4,096 rows, past its allowance of 64 KiB. | 1194 |

The tips ace44fca and 023b0d6c of the native leaf cursors and 368a6fc1 of the merge
fan-out are within every allowance.

### DISTINCT

DISTINCT keeps the rows of unordered and segmented input in a set of `Charged` buffers
on the query's retained account. A single column is a hash set of its IDs. Wider rows
are a hash table of row numbers and an arena of their IDs, which grows toward the
table's room so that the two stay in step, both in one `Rows` buffer. A query that may
spill stops growing the set before a growth that would take the predicted size of its
buffers to the pass's threshold, before a growth whose new allocations taken whole
exceed the room left, or when a growth is refused. The prediction is the bound each
buffer would grow to, so a one-column set is predicted as its table alone and no longer
with the bytes of rows it does not keep, and it freezes later than it did.

The rule on new allocations keeps the room that the set left before the move, when it
admitted its old and new buffers at once. A wider set's growth needs less than its new
allocations, since its table is replaced before its arena grows. Without the rule the
set grew into that room, and the sort above the numbered DISTINCT fixture was refused at
294,912 bytes, a budget it had fitted.

A set that grows where a formula of its final size froze it holds more when it freezes
later, so less room is left for the scratch of a frozen batch. A frozen batch therefore
passes its rows on in ranges whose scratch fits. A range whose scratch is refused before
any of its rows are written is halved, so the rows of each partition keep their order,
and a refusal of the scratch of one row is the query's.

`crates/sparkles-core/tests/charged_parity.rs` sweeps six DISTINCT fixtures, single
and wider rows, segmented, numbered below an ORDER BY and two spilled ones, over a range
of budgets in steps of 4 KiB or 32 KiB. Each fixture pins the budgets of its sweep that
it fitted before the move, read at commit 74f229b9, and the budgets it fits now and did
not then, together with its answer before the move as a row count and a digest of the
rows, which follows their order in the numbered fixture. Every pinned budget must fit
with that answer and no slack, and a budget that fits or misses outside the pins fails,
so a hole inside a range is seen as well as a higher smallest fit. The plan must run one
native DISTINCT, numbered in the numbered fixture, and the spilled fixtures must spill
from that node. The wider-row fixture fits from 1,769,472 bytes where it needed
1,933,312, the one-column spilled fixture from 40,960 where it needed 57,344, and the
wider spilled fixture from 98,304 where it needed 131,072. A change of fit between two
steps of a sweep is not seen.

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
place, with spilled fixtures for the sort and DISTINCT. The first run's findings are
recorded above with their items. DISTINCT is on `Charged` buffers. Removing the charge
of its arena or of its one-column set, or admitting a growth only after it allocates,
fails its fixtures. The external sort moves under item 1193, and the operators that
other work is rewriting move under item 1192.

## Provenance

The design comes from Sparkles' own operators and their audits. It uses no outside
source beyond the hashbrown crate, a workspace dependency pinned at 0.16.1. Its
documented `allocation_size` gives a table's size, and the bound a table is admitted at
follows that release's bucket rule, which raises a small table of one-byte entries to 14
entries and of two- or three-byte entries to 7. A hashbrown upgrade rechecks
`the_bound_covers_every_table_size` and `a_table_of_bytes_grows_within_its_bound`.
