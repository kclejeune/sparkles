# C21: Outbound notifications

> **Status:** specified
>
> **Phases:** Phase 1 is the `notifications` settings kind, the webhook and ntfy
> channels, delivery with retries, deduplication and repeats, a test send, delivery
> status and metrics, the memory review and backup failure events, the model budget
> event, `sparkles notify` and the NixOS documentation. Phase 2 adds an email channel.
> Phase 3 adds a Notifications section to the UI's server page.
>
> **User docs:** [API: Notifications](../API.md#notifications) ·
> [Usage: Sending notifications](../USAGE.md#sending-notifications) ·
> [Features](../FEATURES.md)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This design was written from the public documentation of ntfy's publish API, the
Standard Webhooks specification, the CloudEvents specification's JSON format,
Prometheus Alertmanager's webhook receiver and its `repeat_interval`, RFC 2104 (HMAC),
RFC 9110 on `Retry-After`, and, for Phase 2, RFC 5321, RFC 5322, RFC 3207, RFC 4954,
RFC 6409 and RFC 8314. It builds on layered settings ([C19](C19-layered-settings.md)),
agent memory maintenance ([C18](C18-natural-language-questions-and-ingest.md) §8),
backup policies ([F05](F05-snapshot-repositories.md)) and the server's outbound policy.

## 1. Summary

Some of what happens in a server needs a person, and nobody is told. A scheduled
consolidation of agent memory proposes facts on a review branch, and the next pass
writes nothing until someone reviews it, so memory stops growing in silence. A backup
policy fails at night, and the only trace is a log line and a metric that nobody
watches. A model provider's daily token budget runs out, and every question fails with
`budget-exceeded` until midnight UTC.

This spec adds outbound notifications. The operator or a server administrator
configures channels, which are a webhook or an ntfy topic, and routes from event types
to channels. The server sends a small JSON envelope for each event, retries a failed
delivery a bounded number of times, and repeats a notification about a condition that
persists at most once per interval, so a stuck review does not flood a phone.

## 2. Goals and non-goals

Goals:

- An operator declares channels and routes in the settings file, and a server
  administrator changes them at runtime unless a field is locked, as for the `models`
  kind of C19 §11.
- Channel credentials are named by secret, never written in the settings.
- Every request goes through the server's outbound policy.
- A delivery that fails for a transient reason is retried with backoff, and one that
  cannot succeed is not.
- A condition that lasts is notified once and then at most once per repeat interval
  until it clears.
- A server administrator can send a test notification and can see, per channel, the
  last success and the last failure.
- Notifications are off until the configuration turns them on.

Non-goals:

- Notifications to dataset users. The recipients are the people who run the server.
  Dataset admins choose the thresholds of their dataset's events but not the channels.
- A general event bus or a subscription API. The change feed of
  [CI](CI-commit-identity.md) already serves programs that follow commits.
- Chat formats such as Slack's or Discord's message payloads. A webhook relay or ntfy
  covers them.
- Delivery that survives a restart. The queue lives in the process (§6.5).

## 3. Events

### 3.1 Event types

| Type | Scope | Fires when | Repeats |
|---|---|---|---|
| `memory.review.pending` | dataset | The oldest open review branch of the dataset is older than its `memory.review.notifyAfter` | At most every `memory.review.repeatEvery` while the dataset has open review branches |
| `backup.failed` | server | A run of a backup policy ends `failed` or `partial` | At most every `repeatEvery` of the kind for the same policy, until a run of the policy succeeds |
| `models.budget.exceeded` | server | A provider's tokens counted today reach its `budget.tokensPerDay` | Once per provider and UTC day |
| `notification.test` | server | A server administrator asks for a test | Never deduplicated |

An open review branch is a branch whose name the memory inbox treats as review work
(`ingest.`, `review.`, `consolidation.` and `proposals.` branches) and that has commits
of its own that `main` does not have. Its age is the time since the branch was created.
This is what a scheduled consolidation waits for when it reports `pending-review`.

Failed maintenance tasks, failed embeddings and replication lag are candidates for
later event types. Each new type needs a name, a scope, a trigger and a repeat rule in
this table.

### 3.2 The envelope

Every channel carries the same envelope. The webhook sends it as the body, and ntfy
gets its text fields (§4.2).

```json
{
  "version": 1,
  "id": "0b4f8a7e-6f53-4a8e-9a43-1f1f5c1f2d7e",
  "type": "memory.review.pending",
  "time": "2026-10-10T09:00:00Z",
  "server": "sparkles-prod",
  "dataset": "org",
  "severity": "warning",
  "title": "Memory review waiting in org",
  "summary": "3 review branches in org wait for review. The oldest, consolidation.20261007-1, was created 3 days ago.",
  "link": "https://sparkles.example.org/ui/memory?ds=org&tab=inbox",
  "data": {
    "open": 3,
    "oldest": { "branch": "consolidation.20261007-1", "kind": "consolidation", "created": "2026-10-07T02:00:00Z" },
    "branches": ["consolidation.20261007-1", "ingest.standup-4", "review.alice.20261009-1"],
    "notifyAfter": "2d",
    "repeatEvery": "1d"
  }
}
```

`version` changes only when a member is removed or changes meaning. New members may be
added at any time, and receivers ignore members they do not know. `id` is unique per
notification and stays the same across the retries of one delivery, so a receiver can
drop a duplicate. The names `id`, `type` and `time` follow CloudEvents, without its
`specversion` and `source`, which a receiver of one server does not need. `server` is
the kind's `server` setting or else the host name. `dataset` is absent for server-wide
events. `severity` is `info`, `warning` or `critical`. `link` is absolute when the kind
sets `baseUrl` and a path under `/ui` otherwise. `data` holds the event-specific
members, listed per type in docs/API.md.

## 4. Channels

A channel has a name, a `type` and the members of its type. Channels are a map keyed by
name in the kind (§5), so a lock can name one channel and the layers merge channel by
channel.

### 4.1 Webhook

```json
{ "type": "webhook", "url": "https://hooks.example.org/sparkles", "signingSecret": { "secret": "hook-signing" } }
```

The server sends `POST url` with the envelope as an `application/json` body. A `2xx`
answer is a success. A URL that carries a credential, which is a user name, a password,
a query or a fragment, is refused, because the settings must not hold credentials.
Such a URL, such as an incoming webhook whose path holds a token, is named by
`urlSecret: { "secret": NAME }` in place of `url`, and the server reads the whole URL
from the secret at each delivery.

With `signingSecret`, the request carries the three headers of Standard Webhooks:

- `webhook-id`, the envelope's `id`;
- `webhook-timestamp`, the time of the attempt in seconds since the Unix epoch;
- `webhook-signature`, `v1,` followed by the Base64 HMAC-SHA256 of
  `{webhook-id}.{webhook-timestamp}.{body}` under the key.

A secret value that starts with `whsec_` is decoded from Base64 after the prefix, as
Standard Webhooks specifies, and any other value is used as bytes. A receiver checks
the signature and rejects a timestamp far from its clock, which stops replays.

### 4.2 ntfy

```json
{ "type": "ntfy", "server": "https://ntfy.sh", "topic": "sparkles-ops", "token": { "secret": "ntfy-token" }, "priority": 4, "tags": ["warning"] }
```

The server publishes with ntfy's JSON publish API, a `POST` of
`{topic, title, message, priority, tags, click}` to the root of `server`. The default
server is `https://ntfy.sh`. `message` is the envelope's `summary`, `title` its
`title`, and `click` its link when the link is absolute. `token` names a secret that
holds an ntfy access token and is sent as `Authorization: Bearer`. The priority is 1
to 5. Without one, the server uses 4 for `warning`, 5 for `critical` and 3 otherwise.
`tags` are added to the event type's own tag. A topic on a public ntfy server can be
read by anyone who guesses its name, so the user docs recommend a token with a reserved
topic or a server of one's own.

### 4.3 Email (Phase 2)

```json
{ "type": "email", "host": "smtp.example.org", "port": 587, "tls": "starttls", "username": "sparkles", "password": { "secret": "smtp" }, "from": "sparkles@example.org", "to": ["ops@example.org"] }
```

The server submits a message on the submission port of RFC 6409 with STARTTLS (RFC
3207), or on port 465 with implicit TLS (RFC 8314), and authenticates with SASL PLAIN
(RFC 4954) when `username` is set. A plain connection without TLS is refused. The
message follows RFC 5322 with the title as the subject and the summary, the link and
the data as a plain-text body. The SMTP host passes the outbound policy's host check
before the server connects. The transaction follows RFC 5321, and a `4xx` reply is
retried as a transient failure while a `5xx` reply is not. Phase 2 picks a mail crate
and records it in PROVENANCE.

## 5. Configuration

### 5.1 The `notifications` kind

Notifications are a server-wide settings kind of C19, next to `models`.

```json
{
  "enabled": true,
  "server": "sparkles-prod",
  "baseUrl": "https://sparkles.example.org",
  "repeatEvery": "1d",
  "channels": {
    "phone": { "type": "ntfy", "topic": "sparkles-ops", "token": { "secret": "ntfy-token" } },
    "ops": { "type": "webhook", "url": "https://hooks.example.org/sparkles", "signingSecret": { "secret": "hook-signing" } }
  },
  "routes": {
    "memory.review.pending": ["phone"],
    "backup.*": ["phone", "ops"],
    "*": ["ops"]
  },
  "delivery": { "attempts": 5, "backoffSecs": 30, "maxBackoffSecs": 1800, "timeoutSecs": 10 }
}
```

The built-in defaults are `enabled: false`, no channels, no routes, a `repeatEvery` of
one day and the delivery values above. With `enabled` false, nothing is sent except a
test.

A route's key is an event type, a prefix ending in `.*`, or `*`. An event goes to the
union of the channels of every key that matches it, so the example sends a backup
failure to `phone` and `ops` once each. A route that names an unknown channel is
refused when the kind is written, as a role that names an unknown provider is refused
in `models`. Routes are a map so that the layers merge them per key and a lock can fix
one route.

`delivery.attempts` counts the first attempt, from 1 to 10. The wait before attempt
`n + 1` is `backoffSecs × 2^(n−1)`, at most `maxBackoffSecs`, or the delivery's
`Retry-After` when that is longer and still within `maxBackoffSecs`.

### 5.2 Layers and locks

The kind uses the layers of C19 §11.1.

1. The built-in defaults.
2. The declared `server.notifications` object of the settings file.
3. The runtime layer in `<dataDir>/notifications.json`.

The settings file's `server` member gains `notifications` next to `locked`, and
`server.locked` accepts fields that start with `notifications`, such as
`notifications.channels.phone` or `notifications.enabled`. A lock on a channel fixes all
of its members and keeps it from being removed. As for providers, `null` for a channel
in a `PATCH` removes a declared channel by storing `null` in the runtime layer.

The settings file is the declared layer because it already reloads on SIGHUP, already
holds `server.locked`, and is already a free-form option of the NixOS module. A
separate `--notify-config` flag would have needed its own reload, its own module option
and its own check.

### 5.3 Secrets

A secret reference is `{ "secret": NAME }`, as in a provider's `apiKey`. The names share
the namespace of C19 §11.2, so a notification secret has the same two sources. The
declared source is `--model-secret NAME=file:PATH` or `NAME=env:VARIABLE`, and the
runtime source is a value stored with `PUT /$/server/secrets/{name}`, which a
`secrets.NAME` lock in `server.locked` turns off. `GET /$/server/secrets` lists the
channels that use each secret next to the providers. A channel member that holds a
credential inline, such as a `token` string, is refused. The server reads a secret at
each delivery, so a rotated file applies without a reload.

### 5.4 Dataset settings

Events that belong to a dataset take their thresholds from that dataset's settings, so a
dataset admin decides how patient the dataset's reviewers are. The `memory` kind gains
`review`.

```json
{ "review": { "notifyAfter": "2d", "repeatEvery": "1d" } }
```

`notifyAfter` is the age of the oldest open review branch at which the event fires.
Without it the dataset sends no memory review event, which is the default. `repeatEvery`
is the shortest time between two notifications about the same dataset and defaults to
the `repeatEvery` of the `notifications` kind. Both are durations such as `12h` or `2d`,
at least one hour. Like every dataset setting, both can be declared in the settings file
for every dataset or one, and locked there.

## 6. Delivery

### 6.1 Outbound policy

Every request goes through the server's outbound policy, as SERVICE, LOAD and the model
providers do. A channel on a loopback or private address needs
`--outbound-allow-private` or an `--outbound-allow` entry, and a link-local address
needs an `--outbound-allow` entry either way. A refusal by the policy is final, is
never retried, and is reported with the result `refused`.

### 6.2 Retries

A delivery is retried when the connection fails, when it times out, and on `408`,
`429` and `5xx` answers. Any other answer, a refusal by the policy, and a missing or
unreadable secret end the delivery at once. The waits follow §5.1. An attempt's timeout
is `delivery.timeoutSecs`, capped by the outbound policy's own timeout.

### 6.3 Deduplication and repeats

Each event that describes a condition has a key, such as
`memory.review.pending/org` or `backup.failed/nightly`. The server keeps, per key, when
the condition was first notified, when it was last notified and how many times. An
event whose key was notified less than its repeat interval ago is suppressed and
counted. When the condition clears, the key is removed, so the next occurrence is
notified at once. A memory review clears when the dataset has no open review branch,
and a backup failure clears when a run of the policy succeeds.

The keys are kept in `<dataDir>/notifications-state.json`, so a restart does not send
the same notification again. A server without a data directory keeps them in the
process.

The rule follows Alertmanager's `repeat_interval`, which resends an alert that is
still firing after the interval and stays quiet in between.

### 6.4 Evaluation

The server checks the memory review and budget conditions once a minute, in one
background task. The backup event is sent when a policy run ends. An event is routed
when it is raised, and a change to the routes or channels applies to the events raised
after it.

### 6.5 The queue

Deliveries wait in a queue in the process, with at most 1,000 entries. A delivery that
does not fit is dropped with the result `dropped` and a warning in the log. The queue is
lost when the server stops, and the deliveries in it are neither sent nor retried after
a restart. This keeps the design free of a delivery log on disk. A condition that
persists is notified again after the restart, since its key was saved only when a
delivery was queued and its repeat interval runs from then.

### 6.6 Test send

`POST /$/notifications/test/{channel}` sends a `notification.test` envelope to the
channel at once, in one attempt, whether or not `enabled` is set and whatever the
routes say. The answer reports the result, the HTTP status the channel gave and the
time it took. A failure answers `502` with the error, so an administrator can fix a
channel before turning notifications on.

## 7. Status, metrics and audit

`GET /$/notifications` answers whether notifications are enabled, each channel with its
type, its target without credentials, its last success and its last failure with the
error, the length of the queue, the active keys of §6.3, and the last 50 deliveries with
their results and attempts. The counters and the recent deliveries live in the process
and start empty after a restart.

The metrics are:

- `sparkles_notifications_sent_total{channel,event,result}`, a counter of deliveries by
  final result, which is `ok`, `failed`, `refused` or `dropped`;
- `sparkles_notifications_retries_total{channel}`, a counter of retried attempts;
- `sparkles_notifications_suppressed_total{event}`, a counter of events that a repeat
  interval held back;
- `sparkles_notifications_queued`, a gauge of the queue's length.

Changes to the `notifications` kind are logged as `server_settings_changed` under
`sparkles::audit`, as for `models`. A test send is logged there as `notification_test`
with the channel and the principal. Each failed delivery is logged as a warning under
`sparkles::notify` with the channel, the event type and the error, and the error never
contains a secret or a URL read from a secret.

## 8. HTTP

| Method and path | Needs | Effect |
|---|---|---|
| `GET`, `PATCH`, `PUT`, `DELETE /$/server/settings/notifications` | server `admin` | The kind, as in C19 §11.3. |
| `GET /$/notifications` | server `admin` | The status of §7. |
| `POST /$/notifications/test/{channel}` | server `admin` | The test send of §6.6. |

The dataset thresholds of §5.4 are written through `/$/settings/{ds}/memory` with
dataset `admin`.

## 9. CLI

```
sparkles notify status [--json]
sparkles notify test CHANNEL [--json]
```

Both take the target options of `sparkles settings`. The configuration is changed with
`sparkles settings set --global notifications.enabled=true` and the other commands of
C19, which take `--global notifications` where they take a server kind. A secret is
stored with `sparkles secrets set NAME`.

## 10. NixOS module

The module's free-form `services.sparkles.settings` already passes `server` through, so
`settings.server.notifications` declares the kind and `settings.server.locked` locks it.
The secrets are declared with `services.sparkles.models.secrets`, whose entries become
`--model-secret` flags, for example from a sops-nix file. The build's
`sparkles settings check` validates the declared kind. No new module option is needed,
and the option descriptions and the user docs show the example.

## 11. UI (Phase 3)

The server page gets a Notifications section for server administrators, next to Models.
It shows each channel's status from `GET /$/notifications`, edits the kind with the
settings components of C19 §10, and has a Send test button per channel. Phase 1 leaves
it out because the API and the CLI cover the work, and because the server page's Models
section is changing in parallel work.

## 12. Phases

- **Phase 1.** The kind with its layers, locks and secrets, the webhook and ntfy
  channels, the queue with retries and backoff, deduplication and repeats, the test
  send, the status route and metrics, the `memory.review.pending`, `backup.failed` and
  `models.budget.exceeded` events, `sparkles notify`, and the user and NixOS docs.
- **Phase 2.** The email channel of §4.3.
- **Phase 3.** The UI section of §11.

## 13. Rejected alternatives

- **Notifications in Alertmanager only.** Sparkles exports Prometheus metrics, and an
  operator with Alertmanager can alert on them. Many single-server installations do
  not run it, and a metric cannot say which branch waits for review or link to it.
- **A separate configuration file.** See §5.2.
- **Inline tokens and signing keys.** They would end up in the Nix store, in the
  answers of `GET /$/server/settings/notifications` and in backups of API answers.
- **Routes as a list of rules.** A list is one field in C19's layers, so a lock or a
  runtime change would replace all routes at once.
- **Retrying forever.** A channel that is down for a day would collect an unbounded
  queue. Bounded attempts and the repeat of a lasting condition cover the gap.
- **Persisting the queue.** It needs a delivery log with its own recovery, for a channel
  that tells people about conditions the server re-evaluates anyway.
- **Chat-specific payloads.** Each chat service has its own format and its own
  versioning. A webhook relay or ntfy's integrations translate the envelope.
- **Dataset admins choosing channels.** Channels are outbound connections of the
  server, which C19 leaves to server administrators.

## 14. Acceptance examples

- **A1.** With a webhook channel with a signing secret and a route for
  `notification.test`, `POST /$/notifications/test/hook` delivers one request whose
  `webhook-signature` verifies against the secret, and the answer is `200` with
  `result: ok`.
- **A2.** A channel whose server answers `503` twice and then `200` is delivered on the
  third attempt, after waits that double. `GET /$/notifications` shows the success, and
  the metrics count one `ok` and two retries.
- **A3.** A channel whose server answers `400` is tried once, and its last failure
  shows the status.
- **A4.** A webhook at `http://127.0.0.1` on a server without
  `--outbound-allow-private` is never contacted, and the delivery ends `refused`.
- **A5.** A dataset with `memory.review.notifyAfter = 2d` and a consolidation branch
  created three days ago sends one `memory.review.pending`. Evaluations in the next 23
  hours send nothing. The evaluation 24 hours later sends a second one. After the
  branch is merged, the next evaluation sends nothing and clears the key, and a new
  branch notifies again once it is two days old.
- **A6.** With `notifications.channels.phone` locked, a `PATCH` that changes the topic
  is `409` with `locked-by-config`, and a `PATCH` that adds another channel succeeds.
- **A7.** A `PATCH` with a `token` string in place of a secret reference is `400`, and a
  `PATCH` with a route to an unknown channel is `400`.
- **A8.** A backup policy run that fails sends one `backup.failed`, a second failure
  within `repeatEvery` sends none, and after a successful run the next failure sends
  one again.
- **A9.** With `enabled` false, no event is delivered, and the test send still works.
- **A10.** No answer, log line or envelope contains the value of a secret.

## 15. Sources

- ntfy's documentation of publishing, its JSON publish format, priorities, tags,
  click actions and access tokens.
- The Standard Webhooks specification, for the `webhook-id`, `webhook-timestamp` and
  `webhook-signature` headers, the signed content and the `whsec_` key format.
- The CloudEvents specification and its JSON event format, for the `id`, `type` and
  `time` attributes.
- Prometheus Alertmanager's documentation of the webhook receiver and of
  `repeat_interval`.
- RFC 2104, HMAC, and RFC 4231 for its SHA-256 test vectors.
- RFC 9110, section 10.2.3, for `Retry-After`.
- For Phase 2, RFC 5321 (SMTP), RFC 5322 (message format), RFC 3207 (STARTTLS), RFC
  4954 (SMTP AUTH), RFC 6409 (message submission) and RFC 8314 (implicit TLS).
- The Sparkles code and specs C18, C19 and F05.
