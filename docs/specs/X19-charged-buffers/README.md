# Charged buffers

This spec covers buffers whose memory charge is their allocation and one heap audit
shared by every cursor operator. It is implemented in part, and
[roadmap items 1192 and 1193](../../ROADMAP.md) hold the remaining work.

The `Charged` buffer type and the audit harness are in place, and the audit runs a
fixture for each cursor operator. The operators whose readings are above the exact
allowance are listed in the [findings](SPEC.md#findings), each with its roadmap item.
DISTINCT is on `Charged` buffers, and moving the other operators onto them is the
remaining work of items 1192 and 1193. Item 1240 holds the small costs that the move of
DISTINCT left on two streamed queries.

Read the [design](SPEC.md) and its [acceptance criteria](SPEC.md#acceptance-criteria).
The streaming executor that the operators belong to is described in
[X05](../X05-streaming-query-execution.md), and the query memory pool in
[X08](../X08-disk-spill-and-server-memory.md).
