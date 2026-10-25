# C28: ChatGPT-plan model access

> **Status:** specified.
>
> **Phases:** None shipped. Phase 1 adds personal connections, local sign-in,
> self-hosted credential transfer, the Responses transport and interactive model use.
> Phase 2 adds explicitly delegated background use through durable jobs.
>
> **User docs:** none for ChatGPT-plan access, because it is not implemented.
>
> The [Outcome](#outcome) records implementation when it ships.

This design covers roadmap item 344. It extends the model configuration in
[C19](C19-layered-settings.md), the model roles and disclosure controls in
[C18](C18-natural-language-questions-and-ingest.md), and the per-attempt admission
contract in [C22](C22-graph-rag-community-summaries.md). It uses
[C26](C26-durable-jobs-and-delivery.md) for durable background work.

## 1. Goals and supported deployments

A caller can choose **Sign in with ChatGPT** as a way to connect model access.
Successful consent uses eligible ChatGPT-plan access for supported requests. API-key
providers remain separately configured choices. The interface identifies the active
account, model and access source before sending dataset content.

OpenAI documents plan access for open-source and locally hosted applications, including
self-hosted VMs. Its documentation directs paid and remotely hosted applications to a
separate integration process. An OSS deployment being reachable over a network does
not establish approval for a hosted service. The first implementation supports a
user-controlled local installation or self-hosted runtime. A hosted commercial service
must satisfy the provider's integration requirements before enabling this option.
See the [official overview](https://developers.openai.com/siwc/token-sharing-open-source).

This is an additional model-access connection. Sparkles authentication and dataset
grants still determine who may query or administer a dataset. Connecting a ChatGPT
account neither creates an administrator nor links identities by email. It grants no
access to ChatGPT conversation history. A plan is not a promise of unlimited requests,
all models, embedding access or support for every Sparkles model role.

For example, an analyst can ask questions over permitted software inventory and
advisory graphs using a personal connection. Another analyst cannot spend that
connection's allowance or see its account details.

## 2. Provider configuration and connection ownership

The planned `chatgpt` provider kind describes an operator-approved policy and a
Responses transport. Its endpoint is fixed to the official public API. It accepts
neither an API key nor caller-supplied credential headers or endpoint overrides.
Dataset administrators can select an allowed provider and model, while credentials
remain separate from dataset settings, RDF and provider presets.

Each connection has a random UUID, an owning Sparkles principal, an issued OAuth client
ID, a validated account identity, a selected workspace identity when supplied by the
provider, a stable display label and a credential revision. A connection is never
identified solely by email. An existing registration cannot be replaced with a different
subject or workspace during reauthorization. Adding a different account creates a
separate connection. Owners can retain several registrations and explicitly choose one.

The caller's authenticated execution context carries the selected connection. It is
resolved server-side under the current principal, not trusted because a request names
its UUID. An absent or foreign connection returns the same unavailable response.
Changing the selection does not affect another caller or a running request. Each
request captures the connection UUID and revision at admission and rechecks that it
remains usable before transmission.

Connection state, model catalogs, capability probes, rate limits and token reservations
are scoped to the registration and workspace. Response caches also include the current
disclosure fingerprint and visible snapshot. Cache keys never contain raw tokens.
An account change cannot reuse another account's authenticated HTTP client or cached
entitlement decision. The operator's server and dataset concurrency limits still apply.

Phase 1 does not offer an instance-wide shared personal plan. Existing API-key providers
can still serve operator-configured roles. Without an authorized personal connection,
a background task cannot borrow a recent caller's credentials.

## 3. Sign-in and deployment flow

`sparkles models connect chatgpt` starts a local browser sign-in. The command starts a
loopback listener before opening the authorization URL. The UI's **Sign in with ChatGPT**
action initiates the same helper and waits for an authenticated connection result.
On a remote self-hosted installation the UI explains the local helper and secure
transfer step rather than pretending the server can receive a browser's loopback
callback. The first implementation does not ask users to paste an authorization code
or an error-page URL into an arbitrary web form.

A host persists its own opaque `ext_agent_host_id` before sign-in and reuses it across
restarts. New registration uses `dynamic_agent_client` and Sparkles' own application
name. Returning sign-in uses that connection's issued client ID. Each attempt has fresh
state, nonce and an S256 PKCE verifier. The helper accepts only its selected
`127.0.0.1` callback, validates the state and consumes the attempt once. Duplicate,
expired, unsolicited or changed-client callbacks cannot replace credentials. The
exchange uses the exact redirect URI selected for that attempt.

The requested scope includes identity and the separate
`chatgpt.tokens.use.direct` permission. The token response's granted scopes decide
whether model use is enabled. The implementation verifies the ID token's signature,
issuer, audience, expiry and nonce with provider discovery and signing keys. Identity
without plan permission produces a connected account with plan access disabled.
See [registration and sign-in](https://developers.openai.com/siwc/token-sharing-open-source/sign-in).

The local helper transfers only its selected Sparkles registration to an authenticated,
user-controlled self-hosted destination over a secure channel such as SSH. The receiving
runtime retains its own host ID and becomes the sole refresh owner of the transferred
session. The helper marks its transferred session inactive so it cannot race rotating
refresh tokens. Transferring a second account requires a separate explicit selection.
This does not import unrelated Codex, ChatGPT or editor credential stores. Current
provider limitations on host attribution and revocation for transferred sessions are
shown before transfer. See [self-hosted VMs](https://developers.openai.com/siwc/token-sharing-open-source/self-hosted-vms).

## 4. Credential lifecycle and isolation

Secret records have an explicit format version and minimum-reader capability. They
contain the token bundle, expiry and refresh generation together with the exact client
and account mapping. They are atomically replaced and sealed using the runtime's
protected secret storage. Local helper files are owner-readable only and use a keyring
when available. Credentials never enter browser storage, dataset snapshots, RDF exports,
diagnostic bundles or ordinary configuration downloads. An authorization URL containing
an ID-token hint is redacted from logs. The browser receives connection status and a
safe display label rather than bearer tokens.

Refresh has one owner per session across all server processes. A process-local mutex
alone is insufficient. The runtime serializes the entire refresh exchange with a
cross-process lock and uses a revision check for the durable replacement. Disconnect
invalidates that generation before a delayed refresh can republish credentials. The
latest rotating refresh token must be durable before new requests use it. A crash after
provider rotation but before durable replacement can require sign-in again. It cannot
be repaired by reusing a possibly consumed refresh token indefinitely.

An omitted refresh scope retains the prior grant. An explicitly reduced grant disables
plan requests. Terminal credential errors require reauthorization with the saved client
ID. Temporary routing or service errors preserve the credentials. Refresh responses
cannot change the account or workspace mapping. An expiry read from an unverified
access-token payload is a scheduling hint, not proof of identity or permission.

Sign-out first prevents new admissions and cancels local in-flight work, then attempts
provider session revocation and clears the tokens. A revocation failure is shown as
unconfirmed remote revocation, with a link to the provider's app controls. Retaining the
registration permits later sign-in without registering a new client. Removing the
connection also removes its local account mapping. Neither operation deletes ordinary
dataset query history. These operations cannot promise to retract content already sent.
See [accounts and sessions](https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions).

Credentials are excluded from dataset backups. Whole-server recovery must not silently
reactivate restored sessions or repeat refreshes from an old token generation. Restored
connections begin paused and require reauthorization or an explicit exclusive handoff
with the previous runtime stopped. An unknown secret-record version fails closed.

## 5. Responses transport and completion

The transport uses the selected account's bearer token for the public Responses API.
The account-specific model picker preserves provider ordering and the returned display
labels while sending the chosen model slug. A catalog entry is not proof of successful
inference. Connection testing succeeds only after a complete supported model response.
See [models and inference](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference).

The adapter builds Responses input from the existing provider-neutral prompt. It sends
the necessary history and instructions explicitly. It does not send a Chat Completions
body with an OAuth token. HTTP inference sets `store` to false and `stream` to true.
The adapter omits unsupported controls such as `max_output_tokens`, `temperature`,
`conversation` and `previous_response_id`. It uses instructions or developer messages
in place of explicit system-message input items.

Namespaced local function tools are a separately tested capability. Local execution
still obeys Sparkles permissions and confirmation rules. Hosted file search, hosted MCP,
Code Interpreter and Responses tool search are unavailable on this route. Embedding and
transcription providers remain independent. Structured output is enabled only after a
transport-specific capability test and local schema validation. Unsupported role or
schema combinations fail clearly instead of bypassing validation or changing billing.
See [preview limitations](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations).

The SSE reader accepts arbitrarily split UTF-8 and event boundaries, enforces bounded
event and total response sizes, and observes cancellation and timeouts throughout.
Only `response.completed` finishes a successful call. A delta, tool argument or partial
JSON object is provisional. Incomplete, failed or truncated streams do not become
accepted extraction results, reports, runnable SPARQL or successful cached responses.
Completed responses still pass the existing schema, query and ingestion checks.
Failure after partial text is visible as an interrupted result.

Requests are never automatically replayed after a stream has begun. Before a stream
opens, a rejected credential can cause one serialized refresh and one separately admitted
retry. Other transient retries are bounded and individually accounted for. A retry does
not cross accounts, workspaces, providers or access sources.

## 6. Admission, usage and limits

Every attempt passes the same current dataset authorization, provider send-level,
disclosure, concurrency and budget checks as API-key access. Choosing plan access does
not authorize sending additional facts. Local byte limits and stream cancellation bound
what Sparkles retains, but cannot guarantee a provider's generation or usage stops at
the same point.

The preview does not accept a client-supplied output-token cap. Admission must therefore
not reuse the existing API transport's requested output cap as a hard reservation bound.
A strict token budget is supported only with a verified conservative input and output
upper bound for that exact model and route. Without one, strict-budget operations refuse
before transmission with `budget-bound-unavailable`. An arbitrary configured estimate,
a local cancellation threshold or the account's remaining allowance is not such a proof.
The UI explains this restriction for roles that require strict admission, including
GraphRAG builds. It does not silently remove their limits to make a connection work.

Each admitted attempt records its account connection, access source and reported token
usage. Missing usage retains its conservative reservation. A subscription response is
labelled plan usage rather than converted into an estimated API invoice or described as
free. Sparkles does not calculate remaining plan allowance from its own token counters.
It links to the provider's usage controls for authoritative allowance and credits.

Limit and eligibility failures stop this access path. The interface distinguishes
unavailable plan access, exhausted or app-limited usage, temporary usage-check failure,
unsupported capabilities and a revoked connection. Errors preserve a sanitized provider
code and request ID. They do not expose tokens or another account's details. A limit
may arrive after streaming starts. No guessed reset time is presented. The user can
explicitly choose another configured provider or API-key access for a subsequent request.
See [errors and recovery](https://developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery).

## 7. API and interface surfaces

The planned `/$/model-connections` routes list and manage only the caller's connections.
They expose connection UUID, safe label, selected model, grant state and refresh status.
They never return secret material. Connection create, transfer, selection, test, sign-out
and removal require authenticated ownership and the existing CSRF protection for browser
mutations. Operators may disable personal model connections or particular model roles.
The operator cannot turn a person's connection into a shared secret by naming its UUID
in the server-wide role list.

The model settings UI offers **Sign in with ChatGPT** beside the existing provider
configuration. Its status distinguishes signed out, identity connected without plan
permission, ready, usage limited, temporary failure and sign-in required. Multiple
accounts have distinct labels and a visible active selection. Completing sign-in does
not send dataset content automatically. A test request discloses that it consumes plan
usage. Provider and model changes remain explicit in Ask and extraction controls.

CLI connection operations mirror these states. Batch commands require an explicit
connection selection and appropriate execution ownership. Connection references are
not transferable bearer credentials. UI text and button presentation follow the
[provider's interface guidance](https://developers.openai.com/siwc/ui-ux-guidelines).

## 8. Background delegation

Phase 2 allows the owner to authorize a named job or bounded workflow to use a selected
connection. The delegation identifies its owner, allowed dataset and role, connection,
expiry, local request and token limits, and revocation generation. Existing dataset and
provider policies remain additional constraints. It grants no wider dataset access.

C26 stores only the delegation reference. Each attempt resolves current credentials and
rechecks ownership, grant, expiry and budget. Retries and restarts cannot extend consent,
reset limits or borrow another connection. A revoked owner or paused restored session
stops the job. Strict-budget operations remain unavailable without the proof required in
section 6. Background use is not enabled merely because interactive Ask succeeded.

## 9. Implementation order and acceptance

Phase 1 ships as one supported vertical path through protected connection storage,
local OAuth and self-hosted transfer, refresh and sign-out, Responses streaming, and
interactive UI and CLI selection. Existing API-key, Ollama and Anthropic providers keep
their current contracts. Phase 2 follows the durable execution and delegation work.

The acceptance matrix includes the following checks.

| Area | Required behavior |
|---|---|
| OAuth | Fresh state, nonce and PKCE are checked. Replays, duplicated parameters, changed redirects, invalid signatures, wrong audiences and mismatched client or account identities cannot publish credentials. |
| Permission | Identity-only consent remains disabled for inference. Removing plan scope on refresh stops requests. Two equal email addresses never merge registrations. |
| Ownership | Two principals and two workspaces remain isolated through lists, catalogs, caches, tests, usage and request routing. An anonymous or foreign caller cannot use a connection UUID. |
| Lifecycle | Concurrent requests and processes perform one rotating refresh. Disconnect during refresh, refresh-write failure, crash, expired credentials, revocation failure and restored backups fail safely. |
| Transfer | A selected registration reaches only its authenticated destination. The VM retains its host ID and owns refresh exclusively. No unrelated application credentials are read. |
| Transport | Captured requests use the official Responses route and required fields. Unsupported preview fields and hosted tools are absent. Split events, malformed JSON, oversized events and interrupted streams never publish success. |
| Usage | Completion settles reported usage. Missing usage retains a reservation. Strict budgets refuse without a verified bound. A mid-stream usage limit neither publishes results nor invokes API-key fallback. |
| Existing behavior | API-key providers, per-role model selection, disclosure guards, schema validation and query authorization keep passing their tests. |
| Interfaces | Browser and CLI show the active account and access source, disabled consent, plan limits, transfer instructions, sign-out and reauthorization without exposing tokens. |

Local fake OAuth, JWKS and SSE servers cover deterministic acceptance without real
accounts or paid calls. A live owner-authorized smoke test must verify sign-in, one
completed inference, refresh and sign-out on each supported deployment type before
the feature is documented as available. Its account eligibility and limits are recorded
as a compatibility observation, not a guarantee for every subscriber.

## Outcome

The design is specified. No ChatGPT connection, OAuth helper, Responses model transport
or plan-access interface is implemented. Roadmap item 344 tracks both phases. Existing
model providers continue to use their configured API keys or local model access.
