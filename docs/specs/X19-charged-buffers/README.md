# Charged buffers

This spec covers buffers whose memory charge is their allocation and one heap audit
shared by every cursor operator. It is implemented in part, and
[roadmap item 1183](../../ROADMAP.md) holds the remaining work.

The `Charged` buffer type and the audit harness are in place, and the audit runs a
fixture for each cursor operator. The operators whose readings are above the exact
allowance are listed in the [findings](SPEC.md#findings), each with its roadmap item.
Moving operators onto `Charged` buffers is the remaining work of items 1183 and 1192.

Read the [design](SPEC.md) and its [acceptance criteria](SPEC.md#acceptance-criteria).
The streaming executor that the operators belong to is described in
[X05](../X05-streaming-query-execution.md), and the query memory pool in
[X08](../X08-disk-spill-and-server-memory.md).
