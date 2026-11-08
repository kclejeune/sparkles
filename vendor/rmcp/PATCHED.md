# rmcp 3.5.1

This is rmcp 3.5.1 from the Model Context Protocol Rust SDK at
<https://github.com/modelcontextprotocol/rust-sdk>, revision
`79437f291b2c44053d00dcd5db969fd0cca7c887`, published under Apache-2.0.
The source archive has SHA256
`ab5023804cbe0304cbe06197ac16e645a0a0d0ffa2953bf735c15c2fa0213350`.
Its Cargo VCS record and published source are retained. The published archive has
no separate license or notice file. LICENSE-APACHE contains the standard declared
license text distributed with this dependency in THIRD_PARTY_LICENSES.md.

The server uses this exact local package. The upstream build script installs
upstream repository hooks and produces no generated code. Its original source is
retained, and package.build is disabled so it cannot change Sparkles repository
configuration when built from vendor/rmcp.

The package has its own empty workspace boundary so Cargo cannot adopt the
vendored dependency into an ancestor checkout's workspace.

The retained-output feature adds an outgoing retained result to the SDK's existing
CallToolResponse and ServerResult unions. Its data remains opaque and shared when
cloned, while serialization borrows the current text and structured result. Legacy
CallToolResult behavior is unchanged. The stdio writer retains its pending frame and accepted-byte cursor across a
cancelled send future.


Direct HTTP JSON and SSE retained-result framing use the same application-owned
Bytes without intermediate payload Vec/String copies. Retained SSE refusal closes
and cancels its stream without an unpaid fallback. The retained SSE body preserves
legacy priming/retry/keepalive framing for unrelated ordinary messages. These source
changes are not yet runtime-qualified. Session/replay event metadata and task/product
propagation remain explicit unfinished integration work.
