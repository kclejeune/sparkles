# Performance experiments not adopted

This page lists optimizations that were built and measured but not kept, because they
measured no faster or slower on the benchmarks of the time. Each entry records what was
tried, how it was measured, why it was not kept, and the conditions under which it could
still pay off, such as larger datasets, colder caches or other hardware. Read the matching
entry before starting work on the same idea, and measure it again under those conditions
when they apply.

Adopted optimizations and their numbers are in [BENCHMARKS.md](BENCHMARKS.md), and the
method behind every measurement is in [BENCHMARKING.md](BENCHMARKING.md).

## How to add an entry

Add an entry in the same change that drops or reverts the experiment. Keep the code
reachable: a reverted commit stays in the history of `main`, and an experiment that never
reached `main` gets a tag named `experiment/<slug>` that is pushed with the change. Each
entry gives the idea, the roadmap item, the code, the measurement with its host and
dataset size, the result, and when it could matter.

## Entries

| Idea | Item | Code | Measurement | Result | When it could matter |
|---|---|---|---|---|---|
| Serializers record where each row ends in a row log, so the result buffer stores only lengths and skips its per-write row check. | [243](ROADMAP.md) | `760fa268`, reverted in `fba971db` | Instructions per request for `optional-chain` at 10.5M on the laptop. | 3,431 M instructions against 3,376 M with the per-write check, so no cheaper. | Answers with very many short rows, where the per-write check grows with the row count, or a serializer rewritten to write whole rows at once. |
| An FSST lookup that plans every key's decode from the last key back, so a single-term lookup decodes only what it needs. | [160](ROADMAP.md) | Not kept | Single-term lookups on forge. | 7% slower, because the lookups wait on memory more than on decoding. | Long literals on larger dictionaries, where decoding grows and the lookup stops being bound by memory latency. |
