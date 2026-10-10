# C19: Layered dataset settings

> **Status:** implemented
>
> **Phases:** Phase 1 is the server: settings kinds, the declared layer, layered
> resolution, locks, the `/$/settings` routes and reloading on SIGHUP. Phase 2 is the
> `sparkles settings` command, the NixOS module and the change to `sparkles memory init`.
> Phase 3 is the UI's settings tab. Phase 4 makes the server's model configuration a
> layered settings kind that server administrators can change, with write-only API keys.
> An addendum (§11.5, §11.6) adds provider presets and per-provider TLS options.
>
> **User docs:** [API: Settings](../API.md#settings) ·
> [API: Server settings](../API.md#server-settings) ·
> [API: Model secrets](../API.md#model-secrets) ·
> [Usage: Changing models and keys at runtime](../USAGE.md#changing-models-and-keys-at-runtime) ·
> [Usage: Turning on the assistant in the server](../USAGE.md#turning-on-the-assistant-in-the-server) ·
> [Usage: Changing dataset settings from the command line](../USAGE.md#changing-dataset-settings-from-the-command-line) ·
> [Features](../FEATURES.md)
>
> This is the design as written before implementation. The [Outcome](#outcome) section at
> the end records how it landed.

This design was written from RFC 7396 (JSON Merge Patch), RFC 9110 on conditional
requests, the public documentation of Grafana's provisioning (`allowUiUpdates`),
PostgreSQL's `ALTER SYSTEM` and `postgresql.auto.conf`, systemd's drop-in directories,
Firefox enterprise policies with locked preferences, and the NixOS module conventions
for settings and secrets, together with the Sparkles code. It builds on access control
([C09](C09-dataset-access-control.md)), agent memory ([C17](C17-agent-memory.md)) and
questions and ingestion ([C18](C18-natural-language-questions-and-ingest.md)).

## 1. Summary

A dataset's assistant, memory and ingest settings each live in a JSON file in the
dataset's directory, and each has its own route. An operator who manages the server
with Nix has no good way to set them. The first NixOS integration copied generated files
over the runtime ones before every start. That has four problems.

- **Lost edits.** A change made through the API or the UI disappears at the next
  restart, without any warning.
- **Late datasets.** A dataset created through the UI gets its declared settings only
  after the next restart, and an in-memory dataset cannot get them at all.
- **Late errors.** A misspelled value or a role that names an unknown provider passes
  evaluation and fails when the server reads the file.
- **No editors.** The routes disagree on semantics. The assistant and memory routes
  replace the whole object, and the ingest route merges members and restores a default
  for `null`. Each kind would need its own CLI command and UI form.

This spec separates what the operator declares from what is changed at runtime. The
server reads a declared settings file, keeps runtime changes in the dataset's files as
before, and computes each setting from built-in defaults, the declared values and the
runtime changes, in that order. The operator can lock a field so that it cannot be
changed at runtime. One set of routes serves every kind of setting, and the CLI, the UI
and the NixOS module are built on it.

## 2. Goals and non-goals

Goals:

- Operators can declare settings in a file that the server reads at start and on
  SIGHUP, without writing into dataset directories.
- Users with `admin` on a dataset can change any setting that is not locked, through
  the API, the CLI or the UI, and the change persists across restarts and reloads.
- A declared value acts as a default unless the operator locks it.
- Every answer says where each value comes from.
- The NixOS module validates declared settings at build time.
- Model provider configuration reloads on SIGHUP instead of needing a restart.
- Server administrators can change the model configuration at runtime, including
  providers, endpoints and API keys, unless the operator locks it (Phase 4).

Non-goals:

- Creating datasets from the settings file. The module's `services.sparkles.datasets`
  and the catalog API already create datasets.
- Ingest profiles, stored queries, backup repositories and other collections. The
  registry of §3 is built so that more kinds can be added later.

## 3. Settings kinds

A settings kind is a named JSON object kept per dataset. Phase 1 registers three kinds.

| Kind | Runtime file | Existing routes |
|---|---|---|
| `assistant` | `assistant.json` | `GET`, `PUT /$/assistant/{ds}` |
| `memory` | `memory.json` | `GET`, `PUT /$/memory/{ds}` |
| `ingest` | `ingest.json`, its settings members only | `PUT /$/ingest/{ds}/settings` |

Each kind registers its name, its file, its built-in defaults, a validation function
that receives the effective object and the current model configuration, and the
members that are secret-free by construction. The server's code for each feature reads
the effective object through the registry and does not open the file itself.

An in-memory dataset keeps its runtime layer in the process, as it does today.

## 4. Layers

The effective value of a kind for a dataset is computed in four layers. A later layer
overrides an earlier one.

1. The built-in defaults of the kind.
2. The declared `defaults` of the settings file (§5), which apply to every dataset.
3. The declared entry for the dataset, by name.
4. The runtime layer, kept in the dataset's file.

Layers merge as in RFC 7396. Objects merge member by member, and arrays and scalars
replace the earlier value. A field is a path of object members, written with dots, such
as `send` or `budget.perRequest`. An array is one field.

The runtime file holds only the fields that were changed at runtime. A runtime file
written before this change holds a complete object. It is read as a runtime layer that
sets every field, which gives the same effective value as before.

### 4.1 Locks

The settings file may lock fields, either for every dataset in `defaults` or for one
dataset. A locked field takes its value from the declared layers, and the runtime
layer cannot change it. A write that would change the effective value of a locked
field is refused with `409` and the code `locked-by-config`, and the answer lists the
fields. A write that sets a locked field to its current value is accepted and stores
nothing for that field. If a lock is added while a runtime value exists for the field,
the runtime value is kept in the file but ignored, and the answer marks it as
`overridden`.

The obvious candidate for a lock is `assistant.send`, which decides what may leave the
server.

### 4.2 Sources

A `GET` of a kind returns the effective object and a source for every field:
`default`, `declared` or `runtime`, and `locked` for a field that a lock fixes. The UI
and the CLI use the sources to show where a value comes from and to offer a reset.

### 4.3 Validation

Validation runs on the effective object, never on a single layer. A write is accepted
only when the effective object after the write is valid. When a reload of the settings
file or of the model configuration makes an effective object invalid, for example
because a role now names a provider that is no longer configured, the server keeps
serving. The kind's status reports the problem, and the feature that reads the kind
treats it as it treats a missing provider today.

## 5. The settings file

`sparkles serve --settings FILE` names a JSON file.

```json
{
  "defaults": {
    "assistant": { "send": "schema" },
    "locked": ["assistant.send"]
  },
  "datasets": {
    "slurp": {
      "assistant": { "enabled": true, "ingest": true, "send": "documents" },
      "memory": {
        "agentGraphs": ["urn:x-sparkles:import/*"],
        "imports": { "base": "urn:x-sparkles:import/", "extract": "server" }
      },
      "locked": ["assistant.send"]
    }
  }
}
```

A dataset's `locked` list adds to the list of `defaults`. A dataset entry overrides a
lock of `defaults` for the same field only by setting the field, which keeps it locked
at the new value. The file is read at start and again on SIGHUP. A file that does not
parse or does not validate fails the start. On a reload it is logged as an error, and
the server keeps the previous file. A name in `datasets` that matches no dataset is
kept, so the entry applies as soon as a dataset with that name is created. Settings
follow the name, so a renamed dataset loses its declared entry and gains the entry of
its new name, if any.

The assistant stays off by default, as in C18. Configuring model providers makes the
assistant available, and each dataset still opts in. To turn it on for every dataset,
including datasets created later, an operator declares `defaults.assistant.enabled` in
the settings file and may lock it. The user docs and the NixOS module's documentation
show this as the standard way to turn the assistant on for a server.

The settings file must not contain `endpoint` or `apiKey` members anywhere, for the
same reason as the assistant route: only the model configuration names endpoints and
keys.

### 5.1 Declared datasets

Datasets that the server is started with, through `--loc` or the module's
`services.sparkles.datasets`, are declared by the operator. Today the catalog refuses to
rename them but lets `DELETE /$/datasets/{ds}` remove them. The files stay, and the next
start attaches the dataset again, so the deletion appears to succeed and is then undone.
The server now refuses to delete a declared dataset with `409` and the code
`declared-dataset`. The catalog listing marks these datasets as `declared`, and the UI
disables their delete action and explains that the operator removes them from the
server's configuration. Clearing a declared dataset's data still works through SPARQL
Update and the Graph Store Protocol.

`GET /$/settings` needs server `admin`. It answers the path of the settings file, when
it was read, the last reload error, and the declared names that match no dataset.

## 6. HTTP

| Method and path | Needs | Effect |
|---|---|---|
| `GET /$/settings/{ds}` | `read` | Every kind's effective object, with sources and locks. |
| `GET /$/settings/{ds}/{kind}` | `read` | `{kind, effective, declared, runtime, sources, locked, status}` and an `ETag` of the runtime layer. |
| `PATCH /$/settings/{ds}/{kind}` | `admin` | An RFC 7396 merge patch on the runtime layer. `null` removes the runtime value, so the field falls back to the declared value or the default. |
| `PUT /$/settings/{ds}/{kind}` | `admin` | Makes the effective object equal to the body. The runtime layer stores the fields where the body differs from the declared and default layers. |
| `DELETE /$/settings/{ds}/{kind}` | `admin` | Clears the runtime layer. `?field=PATH` clears one field. |

Writes accept `If-Match` with the `ETag`, and a mismatch is a `412`. Writes to one kind
of one dataset are serialized. A body with `endpoint` or `apiKey` anywhere is a `400`,
as today.

The existing routes stay. `GET /$/assistant/{ds}` and `GET /$/memory/{ds}` answer the
effective object in their current shape. `PUT /$/assistant/{ds}` and
`PUT /$/memory/{ds}` behave as the new `PUT`, and `PUT /$/ingest/{ds}/settings`
behaves as the new `PATCH`, which matches its current semantics.

## 7. Model configuration reload

The model configuration of `--model-config` is read again on SIGHUP. A configuration
that does not load is logged, and the server keeps the previous one. Requests in flight
keep the configuration they started with. Credentials are already read at each request
and need no change.

## 8. CLI

`sparkles settings` talks to a server with the same target options as `sparkles dataset`.

```
sparkles settings get   DS [KIND] [--layer effective|declared|runtime] [--json]
sparkles settings set   DS KIND.FIELD=VALUE...
sparkles settings edit  DS KIND
sparkles settings reset DS KIND[.FIELD]
sparkles settings diff  [DS]
sparkles settings apply FILE
sparkles settings check FILE [--model-config FILE]
```

`set` parses each value as JSON and falls back to a string, so `assistant.send=documents`
and `assistant.budget.perRequest=80000` both work, and it sends one `PATCH` per kind.
`edit` opens the runtime layer in `$EDITOR` and sends it with `If-Match`. `diff` lists
the runtime values that differ from the declared ones. `apply` takes a file in the
format of §5 and patches each dataset's runtime layer with it, for deployments that do
not use a settings file. Locks in the file are reported and not applied, since only the
server's settings file can lock. `check` validates a settings file offline, and with
`--model-config` it also checks that roles name configured providers.

### 8.1 `sparkles memory init`

`sparkles memory init` also turns on server-side ingestion. It sets `assistant.enabled`,
`assistant.ingest` and `assistant.send = "documents"`, but only for fields whose source
is `default`. A declared or runtime value is left as it is, and a lock is never
touched. The output lists each field it changed, each field it left alone with its
source, and the providers of the `extract` role that will receive document text.
`--no-ingest` skips this step.

Setting `send` to `documents` also allows rows to be sent for summaries, so the output
says so. An operator who does not want document text to leave the server locks
`assistant.send` in the settings file, and `memory init` then reports the lock.

## 9. NixOS module

`services.sparkles.settings` holds the settings file of §5 as a Nix attribute set. The
module generates `/etc/sparkles/settings.json`, passes `--settings`, and adds the file
to `reloadTriggers` so a change reloads the server instead of restarting it. The build
runs `sparkles settings check` on the generated file, with the generated model
configuration when there is one, so a wrong value fails the build. The module no
longer writes into dataset directories.

`services.sparkles.models` keeps its options. Its generated file moves from
`restartTriggers` to `reloadTriggers`.

## 10. UI

The dataset page gets a Settings tab with a section for each kind. Each section shows a
form for the common fields and a JSON editor for the whole runtime layer. Each field
shows its source. A field from the settings file is labeled as such, a locked field is
read-only with a lock icon, and a runtime value has a button that resets it. A user
without `admin` sees the tab read-only. Writes send `If-Match`, and a `412` reloads the
section and tells the user that someone else changed it.

## 11. Server-wide model settings

Phase 4 adds the first server-wide settings kind, `models`. It holds the whole model
configuration of C18: providers with their kind, endpoint, headers, limits, budgets and
allowed models, the per-model options, the server's role lists and the routing
settings. This changes a decision of C18, which allowed no route to create a provider or
change its endpoint. Server administrators are trusted with the server's outbound
connections, so they may now change providers through the API. Users with `admin` on a
dataset still change only that dataset's `assistant` settings, as before.

### 11.1 Layers

The `models` kind uses the layers of §4 at server scope.

1. The built-in defaults of C18.
2. The declared model configuration of `--model-config`.
3. The runtime layer in `<dataDir>/models.json`.

Locks for server-wide kinds are declared in the settings file under `server`:

```json
{
  "server": { "locked": ["models.providers.claude.endpoint", "models.routing"] }
}
```

A lock may name a whole provider, such as `models.providers.claude`, which fixes every
field of it and prevents its removal at runtime. Providers are objects keyed by name, so
the merge of §4 adds and changes providers member by member. Removing a declared
provider at runtime stores `null` for it in the runtime layer. Role lists are arrays, so
each list is one field.

Requests to providers still go through the server's outbound policy, so an endpoint on
a private address still needs `--outbound-allow-private`. Headers that carry
credentials and endpoints with credentials, a query or a fragment are refused as
before. A change takes effect for requests that start after it, and requests in flight
keep the configuration they started with, as in §7. A dataset's role list that names a
provider removed at runtime is reported in that dataset's status, as in §4.3.

### 11.2 API keys

A provider names its key with `apiKey.secret`, as before. A secret has two possible
sources. The declared source is `--model-secret NAME=file:PATH` or `NAME=env:VARIABLE`.
The runtime source is a value that a server administrator stores through the API. A
runtime value overrides the declared source unless the settings file locks the secret
with `secrets.NAME` in `server.locked`.

The server stores a runtime value in `<dataDir>/secrets/NAME` with mode 0600, in a
directory with mode 0700. The files are not encrypted until the database-directory
encryption of [F11](F11-encryption-at-rest.md) is built, and they are then wrapped by
its master key like the rest of the data directory. An operator who does not want keys
on disk locks the secrets in the settings file. No route and no command ever returns a
key's value.

| Method and path | Needs | Effect |
|---|---|---|
| `GET /$/server/secrets` | server `admin` | Each secret's name, source (`declared`, `runtime` or `missing`), whether it is locked, when a runtime value was last set, and the providers that use it. |
| `PUT /$/server/secrets/{name}` | server `admin` | Stores a runtime value from `{"value": "..."}`, `204`. |
| `DELETE /$/server/secrets/{name}` | server `admin` | Removes the runtime value, so the declared source applies again, `204`. |

### 11.3 HTTP

| Method and path | Needs | Effect |
|---|---|---|
| `GET /$/server/settings/{kind}` | server `admin` | The kind's effective object, declared and runtime layers, sources and locks, and an `ETag`, as in §6. |
| `PATCH`, `PUT`, `DELETE /$/server/settings/{kind}` | server `admin` | As in §6, on the server's runtime layer. |

`GET /$/models` keeps its current shape and access. It reports the effective
configuration and each provider's status, and a provider's status now says whether its
key is `missing`.

Every change to `models` and to a secret is logged with the principal, the fields
changed and, for a secret, its name only.

### 11.4 CLI, NixOS module and UI

`sparkles settings` takes `--server` in place of a dataset for server-wide kinds, as in
`sparkles settings set --server models.roles.draft='[{"provider":"claude","model":"claude-haiku-5-5"}]'`.
`sparkles secrets list`, `sparkles secrets set NAME` and `sparkles secrets unset NAME`
manage runtime keys. `set` reads the value from standard input or a prompt with echo
off, never from an argument, so the key does not end up in shell history or the
process list.

The NixOS module keeps `services.sparkles.models.settings` and
`services.sparkles.models.secrets` as the declared layer. It adds
`services.sparkles.settings.server.locked`, and `sparkles settings check` validates the
generated model configuration together with the settings file.

The UI's server page gets a Models section for server administrators. It lists the
providers with their status and edits providers, per-model options, role lists and
routing with the same source labels, locks and resets as §10. A key field is
write-only. It shows whether a key is set, its source and when it was set, and offers
to replace or remove a runtime value.

### 11.5 Provider presets

This section and §11.6 were added after Phase 4 landed.

A preset is a template for a provider whose endpoint is well known. It is not a new
kind. It fills the members of a new provider of an existing kind, and every member it
fills can be changed before or after the provider is added.

| Preset | `kind` | `endpoint` | `apiKey.secret` | Starting model |
|---|---|---|---|---|
| `anthropic` | `anthropic` | `https://api.anthropic.com` | `anthropic` | `claude-sonnet-5-5` |
| `openai` | `openai` | `https://api.openai.com/v1` | `openai` | `gpt-5-mini` |
| `ollama` | `ollama` | `http://127.0.0.1:11434` | none | `qwen3:8b` |

The starting model becomes an empty entry of the provider's `models`, so that
`GET /$/models` lists it and `POST /$/models/{name}/test` has a model to call. No preset
sets pricing. Generic OpenAI-compatible gateways have no preset, because their endpoints
differ for every deployment. They use the `openai` kind with their own endpoint.

The UI's Add provider form has a preset picker whose Custom choice keeps the empty form.
`sparkles settings set --global --preset NAME PROVIDER [FIELD=VALUE…]` adds a provider
from a preset. Each assignment names a member of the provider, such as
`endpoint=https://proxy.example/v1` or `apiKey.secret=team-key`, and replaces the
preset's value. The command refuses a provider name that is already in use, so a preset
never overwrites a configured provider.

The table is kept twice, as a Rust constant in `models/presets.rs` for the CLI and as
`ui/src/lib/provider-presets.json` for the UI, and a Rust test checks that the two
agree. Serving the table from the server was the alternative. It would have needed a
route, its OpenAPI description and a request before the form can open, for data that
changes only with a release.

### 11.6 TLS options per provider

A provider with an `https` endpoint may have a `tls` member.

```json
{
  "providers": {
    "internal": {
      "kind": "openai", "endpoint": "https://llm.internal.example/v1",
      "apiKey": { "secret": "internal" },
      "tls": { "caCert": { "file": "/etc/ssl/internal-ca.pem" } }
    }
  }
}
```

`tls.caCert` names a PEM CA certificate or bundle that is trusted for that provider in
addition to the system roots. It is a reference, either `{"file": PATH}` with an
absolute path on the server or `{"secret": NAME}` naming a secret of §11.2, so a
declared `--model-secret` source or a runtime value stored through the API. The
certificate itself is never written into the configuration, and a PEM text where the
reference belongs is refused with a message that says so. The certificate is read for
each request, as keys are, so a renewed file needs no reload. This is the answer for an
endpoint with a self-signed certificate or a certificate from an internal CA.

`tls.insecureSkipVerify: true` turns certificate verification off for that provider.
Anyone on the network path can then read the API key and the prompts sent to the
provider, and change the answers. The option is built to be hard to turn on by accident.

* It exists per provider only. No server-wide switch turns verification off.
* It is part of the `models` kind, so only server administrators can change it.
* The settings file can lock it, as in `models.providers.NAME.tls.insecureSkipVerify`
  under `server.locked`, which pins the declared value. A NixOS deployment that
  declares nothing pins verification on.
* The server logs a warning naming the provider each time it loads a configuration
  that turns verification off, at start, on SIGHUP and after each change through the
  API.
* `GET /$/models` reports `unverified: true` for the provider, and the UI marks it.
* The UI shows the option as an unchecked checkbox, and a change that turns it on is
  sent only after a dialog whose acknowledgement explains what can be read. The same
  dialog guards the runtime layer's JSON editor.

Both options are refused on an `http://` endpoint, where there is no certificate to
check. `tls` has no other members.

The options change only how the provider's certificate is checked. The request still
goes through the outbound policy: the destination is checked before any connection, a
private address still needs `--outbound-allow-private`, and the connection goes to the
addresses that were checked. While either option is set, a redirect to another origin
is refused, so the options never apply beyond the server they were given for.

## 12. Rejected alternatives

- **Writing declared settings into the dataset files before each start.** This is the
  first NixOS integration. It loses runtime edits and delays settings for datasets
  created at runtime, as §1 describes.
- **Declared values always win.** This is simpler, but it blocks runtime changes for
  every declared field, and the operator asked for defaults that users can change.
- **Runtime values always win, with no locks.** This leaves the operator no way to
  enforce a policy such as what may be sent to a model.
- **Locks per kind instead of per field.** These force the operator to lock all of
  `assistant` in order to lock `send`.
- **Model configuration only in the operator's file.** C18 chose this. It keeps
  outbound connections with the operator, but it forces a restart or a redeploy for
  every change of a role list or a budget. Server administrators are trusted with the
  server, and the operator can still lock what must not change.
- **Turning the assistant on whenever models are configured.** Even the `schema` level
  of `send` sends entity labels from the data, and anyone with `read` can spend tokens.
  A provider added at runtime would then turn assistants on across the server. A
  variant that turned it on only when every `draft` provider is on loopback was also
  declined in favor of one explicit setting.
- **Returning stored keys to administrators.** A key that can be read back can leak
  through the UI, logs and backups of API answers. Keys are write-only, as in most
  services that hold credentials.
- **Creating datasets from the settings file.** The module and the catalog already
  create datasets, and two ways of declaring a dataset would have to agree on paths,
  types and deletion.
- **Recreating a deleted declared dataset at the next start.** This is today's
  behavior, and it makes a deletion look successful until the restart undoes it.

## 13. Acceptance examples

- **A1.** The settings file declares `slurp.assistant.enabled = true` and the dataset
  does not exist. After `POST /$/datasets` creates `slurp`, `GET /$/settings/slurp/assistant`
  answers `enabled: true` with the source `declared`, without a restart.
- **A2.** A `PATCH` sets `assistant.historyDays = 7` on a dataset whose settings file
  declares 30. After a restart and after a SIGHUP, the effective value is still 7 with
  the source `runtime`. `DELETE ?field=historyDays` brings back 30 with the source
  `declared`.
- **A3.** `assistant.send` is locked at `schema`. A `PATCH` with `send: "documents"` is a
  `409` with `locked-by-config`. A `PUT` that restates `send: "schema"` succeeds.
- **A4.** A settings file with `send: "everything"` fails `sparkles settings check` and
  fails the start. On a reload, the server logs the error and keeps the previous file.
- **A5.** An in-memory dataset named in the settings file gets its declared values.
- **A6.** A runtime file written before this change gives the same effective values as
  before.
- **A7.** Two `PATCH` requests with the same `If-Match` value: the second is a `412`.
- **A8.** `sparkles memory init` on a new dataset sets the three assistant fields and
  lists the `extract` providers. On a dataset with `send` locked at `schema`, it leaves
  `send` alone and reports the lock.
- **A9.** `DELETE /$/datasets/demo` on a dataset named by `--loc` is a `409` with
  `declared-dataset`, and the dataset and its files are unchanged.
- **A10.** A change to `services.sparkles.models.settings` reloads the server without a
  restart, and `GET /$/models` shows the new roles.
- **A11.** A server administrator adds a provider and a role list with `PATCH
  /$/server/settings/models` and stores its key with `PUT /$/server/secrets/{name}`. The
  next ask uses the provider without a restart. A user with only dataset `admin` gets
  `403` on both routes.
- **A12.** `GET /$/server/secrets` and `GET /$/server/settings/models` never contain a
  key's value, and neither does the server's log.
- **A13.** With `models.providers.claude.endpoint` locked, a `PATCH` that changes the
  endpoint is a `409` with `locked-by-config`, and a `PATCH` that changes the provider's
  budget succeeds.
- **A14.** A runtime key overrides a declared `file:` source. `DELETE` of the runtime
  value brings back the declared file. With `secrets.anthropic` locked, the `PUT` is a
  `409`.
- **A15.** A runtime provider on a private address is refused at request time unless
  `--outbound-allow-private` is set.

## 14. Sources

- RFC 7396, JSON Merge Patch, for the merge rule and `null` as removal.
- RFC 9110, sections 8.8.3 and 13.1.1, for `ETag` and `If-Match`.
- Grafana's provisioning documentation, for provisioned resources that the UI may or may
  not update.
- PostgreSQL's documentation of `ALTER SYSTEM`, for runtime settings kept apart from the
  operator's file.
- systemd's documentation of drop-in directories, for layered configuration.
- Firefox's enterprise policy documentation, for locked preferences.
- The NixOS manual's guidance on settings options and secrets.
- The Sparkles code and specs C09, C17, C18 and F11.

## Outcome

Phases 1, 2 and 3 and the server side of Phase 4 landed on 2026-10-10. The CLI, UI and
NixOS parts of Phase 4 are not built.

The server has a registry of three dataset-scoped kinds, `assistant`, `memory` and
`ingest`. The `ingest` kind holds the settings members of `ingest.json`, and the
extraction profiles in the same file stay outside the layers. Each kind resolves its
four layers as §4 describes, and every feature reads its settings through the registry.
The answer of `GET /$/settings/{ds}/{kind}` carries `effective`, `declared`, `runtime`,
`sources`, `locked`, `overridden`, `status` and `etag`. The routes of §6, the settings
file of §5 with `serve --settings`, the reload on SIGHUP of the settings file and the
model configuration, and `sparkles settings check` work as written. The legacy routes
`/$/assistant/{ds}`, `/$/memory/{ds}` and `/$/ingest/{ds}/settings` write through the
same code and keep their answers. Datasets of `--loc` and `--mem` answer `DELETE` with
`409` and the code `declared-dataset`, and `DatasetInfo` marks them with `declared:
true`.

The registry was written so that Phase 4 can add server-scoped kinds. A kind has a
scope, the write locks are keyed by an optional dataset and the kind, and the layer
resolution does not depend on where the declared layers come from. The parser accepts a
top-level `server` member with a `locked` list whose fields start with `models` or
`secrets`, checks it and stores it, and Phase 4 applies it. The model configuration
is read from one handle that a reload swaps, so a request in flight keeps the
configuration it started with.

The tests cover A1 to A7 and A9 in the process, and A2, A4 and A9 again with a server
process, a restart and SIGHUP. A8 and A10 belong to later phases, and A11 to A15 to
Phase 4 below.

These points differ from the design or settle what it left open.

- The refusal to delete a declared dataset is in the server's HTTP handler and not in
  the catalog of the library. The Python and Node bindings attach and delete datasets
  through the catalog, and the refusal would have changed their behaviour. A dataset of
  `--mem` is attached the same way as one of `--loc`, so it counts as declared too.
- A `PATCH` with `null` removes a runtime value and so cannot remove a member of a map
  that the declared layer sets. A `PUT` without that member stores `null` for it, and
  that does remove it.
- A `PUT` body that leaves out a member gets the built-in default of the member, and a
  locked field that the body leaves out keeps its value.
- `status` is `{valid, error}`, where `error` is present only when `valid` is false.
- `GET /$/settings` also reports when the model configuration was read and the last
  error of reading it, under `models`.
- A member name that holds a dot is written with a backslash before the dot in a field
  path, such as `agents.claude\.ai`.
- The ETag is computed per kind from its runtime layer, and the answer repeats it in an
  `etag` member.
- The settings of a dataset are read from the files of its main branch, never from a
  branch.
- A runtime file written before the layers existed holds every field, so its values
  override the declared values that are not locked. A `PUT` rewrites the file with
  only the fields that differ from the declared values and the defaults.
- The runtime layer of an in-memory dataset moves with a rename and is dropped when the
  dataset is deleted.
- Every settings write is refused on a read-only server (`serve --read-only`) with
  `403`, as other admin writes are. This covers the routes of §6, the legacy
  `PUT /$/assistant/{ds}`, `PUT /$/memory/{ds}` and `PUT /$/ingest/{ds}/settings`, and
  the server-wide routes of §11. Phase 1 had left the new routes without the check.
- A `PATCH` that restates the value of a locked field stores nothing for it, also when
  the runtime layer already holds other members of the same object. Phase 1 kept the
  restated value in that case.

### Phase 2: the NixOS module

`services.sparkles.settings` holds the settings file of §5 as a free-form attribute set
of JSON values, so `defaults`, `datasets.<name>`, the `locked` lists and
`server.locked` all pass through unchanged, and lists from several modules are
concatenated. The module generates `/etc/sparkles/settings.json`, passes `--settings`,
and puts the file in `reloadTriggers`. The generated `models.json` moved from
`restartTriggers` to `reloadTriggers`, and the module has no restart triggers left.
`datasetSettings`, which copied files into dataset directories before each start, is
gone, and with it the module's writes into dataset directories and the assertion that
refused in-memory datasets. Names under `settings.datasets` must still match the
module's rule for dataset names.

The settings file is the output of a derivation that runs `sparkles settings check` on
the generated JSON, with `--model-config` naming the generated model configuration when
`services.sparkles.models.settings` is set, so a wrong value fails the system build.
The check reads only the two files. A model configuration given with
`models.configFile` is not available at build time, so the build then checks the
settings without providers. When the build platform cannot run the host's binary, the
module skips the check.

The NixOS VM test `nixos-module` covers A1, A2, A5 and A9 through the module, the
`409` of A3, and A10. It switches to a specialisation with another dataset entry and
another role list, and checks that `switch-to-configuration` reloads the unit, that the
main PID stays the same, and that both changes apply while the runtime values stay. It
also reloads the service right after a restart and checks that the server keeps
running. The check `nixos-settings` builds the module without a VM and uses
`testers.testBuildFailure` to show that a bad `send`, an unknown kind, an `endpoint`
member, an empty lock and a provider that the generated model configuration lacks each
fail the build. `nixos-models` checks that the model configuration is a reload trigger
and no longer a restart trigger.

These points differ from the design or settle what it left open.

- The module always passes `--settings`, with `{}` when nothing is declared. Adding
  settings to a server that had none then changes only the file and reloads the server,
  where a new flag would have restarted it.
- `ExecReload` is set in every configuration, since the server now always catches
  SIGHUP for the settings. The server installs its handlers only after it has opened
  its datasets, and SIGHUP before that would stop it. The reload command therefore
  waits until the main process's `SigCgt` mask in `/proc` shows SIGHUP as caught, for
  up to 80 seconds, before it sends the signal.

### Phase 2: the command line

`sparkles settings` has the subcommands of §8. `get`, `set`, `edit`, `reset`, `diff`
and `apply` talk to a server through the routes of §6. They find the server and the
token as the other remote commands do, from `--server`, `SPARKLES_SERVER` or the saved
login, and from `SPARKLES_TOKEN` or the credentials file, and each takes `--json`.
`check` works offline as before. `sparkles memory init` turns on server-side ingestion
as §8.1 describes and takes `--no-ingest`. The tests in
`crates/sparkles-server/tests/cli_settings.rs` cover every subcommand against a server
process, a locked field, an `If-Match` conflict in `edit` with a scripted editor, and
A8.

These points differ from the design or settle what it left open.

- The target options are `--server`, `--insecure-http` and `--json`, as in
  `sparkles memory`. `sparkles dataset` also takes `--data-dir` for a stopped server's
  catalog, but the settings commands need a running server, since the declared layers
  live in the server's settings file. `--branch` is refused, because settings belong to
  a dataset's main branch.
- `--server` names the server's URL here, as in every other remote command. §11.4 uses
  `--server` in place of a dataset for server-wide kinds, which would clash with it.
  The command resolves its target through one type with a dataset variant, so Phase 4
  can add a server-wide target, but it has to pick another spelling for it, such as a
  `--server-wide` flag or a reserved name in place of the dataset.
  The maintainer chose `--global`, as in `git config --global`, so the Phase 4 command
  is `sparkles settings set --global models.roles.draft=...`.
- `get --layer declared` or `--layer runtime` prints that layer, and with `--json` only
  the layer's object, so that `get DS KIND --layer runtime --json` gives what `edit`
  edits. Without `--layer`, `--json` prints the server's answer.
- `set` with `null` as the value removes the runtime value, as a merge patch does.
- `edit` opens `$VISUAL`, else `$EDITOR`, else `vi`, and sends the difference between
  the runtime layer it read and the edited object as a merge patch with `If-Match`. A
  member removed in the editor becomes `null` in the patch, which removes its runtime
  value. On a `412` the command keeps the edited text in a file, says where, and offers
  to open the editor again on the current runtime layer. On a `400` or a `409` it offers
  to edit the same text again.
- `diff` compares each runtime value with the declared value and, where the settings
  file declares nothing, with the built-in default, so a runtime file written before the
  layers existed lists only the fields that differ from the defaults. The built-in
  defaults are those of the CLI's own build. A runtime value that a lock overrides is
  marked as ignored.
- `apply` patches every dataset on the server with `defaults`, merged with the
  dataset's own entry, and skips names that match no dataset. It validates the file
  offline first, reports the locks of `defaults`, the dataset entries and `server`
  without applying them, and goes on after a refused patch, ending with exit status 1.
- `memory init` reads `GET /$/settings/{ds}/assistant`, patches the fields whose source
  is `default` with `If-Match`, and reads again on a `412`, up to three times. The
  providers of the `extract` role come from the dataset's `roles.extract` when it is
  set and from `GET /$/models` otherwise. That route needs server `admin`, so for a
  caller with only dataset `admin` the output says the providers are unknown. Each
  provider is listed with the `send` level that applies to it after `sendByProvider`,
  and the text marks a provider that receives no document text. A failure of this step
  is reported in the output and does not fail `init`.
- The settings module gained `Kind::default_value`, which `diff` uses. Nothing else in
  the server changed.
- The same work added `sparkles memory consolidate`, `sparkles memory retention` and
  `sparkles memory maintenance` for the maintenance routes of C18 Phase 5, which had no
  command.

### Phase 3

The dataset page has two tabs, **Overview** with the panels it had before and
**Settings**, which `?tab=settings` opens. The Settings tab has a section for each kind.
A section has a form for the common fields of §10, grouped by topic, and under
**Advanced** an editor of the runtime layer as JSON beside the declared layer and the
locked fields. Saving the editor sends the merge patch from the old runtime layer to the
new one, so a member removed from the text is reset.

Each field shows its source. A declared value is labeled "server config", a locked
field has a lock and a disabled control, a runtime value is marked "changed" with a
reset that sends `DELETE ?field=`, and a field in `overridden` says that its change is
ignored and offers the reset as well. A field whose own source is `default` but that a
layer sets below it, such as a map, takes the strongest source of its leaves. A runtime
value over a declared one names the declared value in its tooltip. An invalid `status`
shows its error above the form.

A save sends a `PATCH` of the fields whose form value changed, with `If-Match`. An
emptied optional field sends `null`, and a map field sends only its changed members. A
`412` reloads the section and shows a toast, a `409` with `locked-by-config`
highlights the named fields, a `400` shows the server's message, and a `403` turns the
section read-only. The tab is read-only for callers without dataset `admin`, which the
UI reads from `/$/whoami` as its other panels do. The source labels, locks and resets
are the components `FieldSource`, `SettingRow` and `SettingsKindPanel`, which take a
kind's URL and its field list, so Phase 4 can use them for `models`.

The datasets list marks declared datasets and disables their delete button with an
explanation, as does the dataset page. A `409` with `declared-dataset` shows the same
explanation in the delete dialog.

These points differ from the design or settle what it left open.

- The tab also covers memory maintenance from C18 Phase 5, which had routes and no UI.
  The memory section shows each job's last run, outcome and next scheduled run, and
  offers **Run now** to dataset admins. Retention runs a dry run first and asks for a
  confirmation that lists the graphs it would delete. Maintenance passes are tasks of
  the ingestion registry, not of `/$/tasks`, so the section follows them through
  `/$/ingest/{ds}/{task}` with its own progress bar instead of `TaskProgress`. The
  last run is the newest pass the server still knows, dry runs left out, or else the
  scheduled pass of `/$/memory/{ds}/maintenance`.
- The JSON editor cannot store a `null` in the runtime layer, since a `null` in a merge
  patch removes a value. Removing a declared member of a map still needs a `PUT`.
- When Phase 3 was built, the settings routes did not check the server's read-only
  flag. The server side of Phase 4 added the check, so on a read-only server the tab's
  saves are refused with `403` and the section turns read-only. Run now is disabled
  there, since maintenance is refused.
- No Rust code changed.
- The documentation and status lines were merged with the Phase 2 work, which
  landed in parallel.

`ui/src/lib/settings.test.ts` and `ui/src/lib/maintenance.test.ts` test the path,
source, patch and error logic and the routes' requests. `ui/tests/e2e/settings.spec.ts`
runs against a server started with a settings file and a declared dataset. It checks
the locked and declared fields, a save and its reset, a stale save after another
client's change, a refused change of a locked field, a consolidation run, the read-only
tab of a user without admin, and the disabled delete of the declared dataset.

### Phase 4, the server

The server side of Phase 4 landed on 2026-10-10. The CLI, the UI and the NixOS module
followed on the same day, as the next section describes.

The registry has a server-wide kind, `models`, whose declared layer is the file of
`--model-config` and whose runtime layer is `<dataDir>/models.json`. It resolves
through the same code as the dataset kinds, with the locks of `server.locked`. The
effective object is checked as `--model-config` is, and the configuration built from it
replaces the one that `GET /$/models` and every model call read. Requests in flight keep
the configuration they started with. The routes of §11.3 serve the kind, and
`GET /$/settings` lists it under `serverKinds`. The secrets of §11.2 are stored in
`<dataDir>/secrets` and listed, stored and removed through `/$/server/secrets`. All of
these routes need `server-admin`, and `GET /$/models` keeps its shape with a `source` in
each provider's `apiKey`.

The routes never return a key, and the server takes these steps so that no log line
carries one. A `PUT` body is read into a type whose `Debug` prints nothing of the value
and which has no `Display` or `Serialize`. The errors about a body, an `apiKey` that
holds a value instead of `{"secret": NAME}`, and a header that carries credentials
never quote the value. The server never logs request bodies, and the access log names
only the route and the principal. A runtime value is read only as a `file:` source at
each request, as declared files are, and `GET /$/server/secrets` reads only the names
and times of the files. The process test runs the server at TRACE and checks that its
log holds neither the runtime nor the declared key.

Each change to `models` is logged at INFO under the `sparkles::audit` target as the
event `server_settings_changed`, with the kind, the operation, the changed fields as
dotted paths and the principal. Each change to a secret is logged as `secret_set` or
`secret_removed` with the secret's name and the principal. This is the target that the
server's other admin actions, such as logins and backup repositories, already use.

The tests cover A11 and A12 with access control in
`http/router_tests/auth/server_settings.rs`, and A13 to A15 with the layering, the
locks, the refusals, a restart and a reload in `settings/server_tests.rs`.
`tests/cli_server_settings.rs` runs a server process through a restart, a SIGHUP that
reads a changed `--model-config`, `sparkles settings check` with `server.locked`, and
the log check of A12.

These points differ from the design or settle what it left open.

- A `PATCH` with `null` for a provider that `--model-config` defines stores `null` and
  removes it, as §11.1 says, while `null` elsewhere still removes only a runtime value.
  `DELETE ?field=providers.NAME` brings the declared provider back. The registry marks
  `providers` as the one map whose members a `null` removes.
- A lock on a provider that the declared configuration does not define keeps it from
  being added at runtime. The settings file fails on a lock that names an unknown member
  of `models`, of a provider or of `routing`, or an unknown role, and
  `sparkles settings check --model-config` warns about a lock on a provider the
  configuration lacks or on a secret that no provider uses. A role list is one field,
  so a lock cannot name an entry of it.
- Without `--model-config`, a runtime layer alone configures the models. With neither,
  the server has no models, as before.
- When a change of `--model-config` or of the locks makes the effective configuration
  invalid, the server keeps the configuration in force on a reload, or uses the
  declared configuration alone at a start, and reports the error under `models` in
  `GET /$/settings` and in the kind's `status`. The start does not fail, so a server
  administrator can repair the runtime layer through the API.
- The tokens a provider has counted today carry over to the new configuration, and so
  do the detected structured-output levels and the last outcome of a provider whose
  kind and endpoint did not change. Concurrency slots start empty in the new
  configuration.
- Secret names follow one rule everywhere: 1 to 128 letters, digits, `_`, `-` and `.`,
  not starting with `.` or `-`. `--model-secret` and `apiKey.secret` now refuse other
  names, which earlier were only required to be non-empty.
- `DELETE /$/server/secrets/{name}` answers `204` whether or not a runtime value
  existed, so that it can be repeated. A locked secret's stored value can still be
  removed, and while the lock holds it is listed as `overridden` and ignored.
- `GET /$/server/secrets` also reports whether a declared source exists, and
  `overridden`, and it lists the secrets that a provider or a lock names without any
  source as `missing`.
- A server without a data directory, as in embedded use, keeps the runtime layer of
  `models` in the process and refuses to store a secret with `409` and
  `no-data-directory`.
- The runtime layer drops empty objects, as for the dataset kinds, so a model entry
  without options, such as `"models": {"m": {}}`, cannot be added at runtime. An entry
  with at least one option can.
- The SIGHUP listeners of the settings and models, the auth configuration, the rate
  limits and the TLS certificate now register with no await between them, the settings
  first, and the auth and TLS listeners register before their tasks start. A reload
  sent as soon as the process catches SIGHUP therefore reaches each of them. The
  backup configuration registers its listener earlier in the start and is unchanged.

### Phase 4, the CLI, UI and NixOS module

The rest of Phase 4 landed on 2026-10-10, together with a change to every settings
answer.

Each answer of a dataset kind and of a server kind now has an `overrides` member, a list
of `{path, declared, runtime}`. It names each runtime value that replaces a declared one,
with both values, so that a client can show what a reset brings back without comparing
the layers itself. The path is the runtime leaf, or the shorter path where the declared
value is not an object, so a runtime `null` that removes a declared provider appears
once as `providers.NAME` with `runtime: null`. Runtime values where nothing is declared
are not listed, and neither are locked fields, which `overridden` already reports.
docs/API.md, the OpenAPI document and the generated TypeScript client describe it.

`sparkles settings get`, `set`, `edit`, `reset` and `diff` take `--global` in place of a
dataset and work on the `models` kind at `/$/server/settings/models`. `get` prints a
runtime value over a declared one as `runtime, overrides declared VALUE`, and a removed
member as `(removed)`. `reset` prints the value that applies afterwards and whether it
is the declared value, the default, the locked value or a runtime value further down,
and a reset of a whole kind lists each field it reset. `diff` reads `overrides` and,
without a dataset, covers the server kinds too. `apply` lists the file's
`server.locked` and changes nothing at server scope. `sparkles secrets list`, `set NAME`
and `unset NAME` manage runtime keys. `set` reads standard input when it is not a
terminal and otherwise prompts with echo off, refuses an empty value, and never prints
the value. `list` prints the name, source, lock, time of the runtime value, the
providers that use the secret and whether the runtime value overrides the declared
source, and `--json` prints the server's answer. `tests/cli_server_settings.rs` runs
these commands against a server process and checks that the key appears in neither the
CLI's output nor the server's log.

The server page of the UI has a Models section for users with `server-admin`. It lists
the providers from `GET /$/models` with their key status, adds a provider through a
short form, and shows the `models` kind with the components of the Settings tab, with a
group of fields for each provider, the role lists and routing, and the runtime layer as
JSON under Advanced. A provider has a Remove button unless it or one of its fields is
locked, and a declared provider removed at runtime is listed with Use server config.
The API keys panel shows whether each secret is set and where its value comes from.
Replace opens a password field whose value is read from the form when it is sent and
never kept in the component's state. A runtime key over a declared one offers Use
server config, and a key that exists only at runtime offers Remove runtime value.

Every settings section, in the dataset tab and in the Models section, shows a runtime
value over a declared one as "overrides server config" with `server config: VALUE`
below the field and a reset labeled Use server config. A runtime value with no declared
value has Reset to default. A section with overrides says how many there are and offers
Use server config for all, which lists them in a dialog and removes them one `DELETE`
at a time with the `ETag` of the previous answer. `ui/tests/e2e/models.spec.ts` and
`settings.spec.ts` cover these against a server with a model configuration, a declared
key and a locked endpoint.

The NixOS module already passed `services.sparkles.settings.server.locked` through to
the settings file, and the build's `sparkles settings check` already validated it with
the generated model configuration. The VM test now locks a provider's endpoint, checks
that a `PATCH` of it is refused with `409`, changes the provider's budget with
`sparkles settings set --global`, and checks that the change survives a reload and a
switch to another configuration. `nix/settings-test.nix` checks that a lock path naming
no field fails the build.

These points differ from the design or settle what it left open.

- The flag is `--global` rather than `--server`, since `--server` already names the
  server to connect to in every remote command. With `--global`, the first positional
  argument is the kind for `get` and `edit`, an assignment for `set` and the field for
  `reset`. When a dataset argument equals a server kind's name, the error suggests
  `--global`.
- `edit --global` and `get --global` default to the `models` kind, the only server kind.
- Use server config for all removes the overrides one field at a time in the order of
  `overrides`, which lists providers before role lists. A role list that names a
  provider brought back by an earlier reset is then valid when its own reset runs. If a
  reset in the middle is refused, the section shows the error and reloads, so it shows
  the overrides that are left.
- The UI keeps the Models section's form in step with the providers by building it again
  when a provider is added, removed or changes protocol, so unsaved edits in other
  fields are lost at that moment. Adding and removing a provider are separate writes and
  do not wait for Save.
- Runtime keys stay unencrypted in `<dataDir>/secrets` until F11 adds encryption at
  rest. The UI's API keys panel says so.

### Provider presets and TLS options

The presets of §11.5 and the TLS options of §11.6 landed on 2026-10-10 as written.

The outbound policy of the engine gained a `tls` member with extra root certificates
and a switch that accepts any certificate. The model client sets it for each request
from the provider's `tls`, next to the provider's connect timeout, and every other
member of the policy stays the server's. A certificate that the client does not trust
fails with the message "the server's TLS certificate is not trusted". A `caCert` that
cannot be read or holds no PEM certificate fails the call before any request, with the
code `tls-config`. `GET /$/models` gives each provider `unverified` and, for a provider
with `tls`, a `tls` object with `verification` (`system`, `custom-ca` or `off`) and the
`caCert` reference with its `status` (`ok` or `unreadable`). A secret that a `caCert`
names is listed by `GET /$/server/secrets` with the providers that use it, and the API
keys panel can store its runtime value like a key's.

`models/tests.rs` runs an HTTPS mock with a certificate from a test CA. The call fails
with the system roots, succeeds with the CA from a file and from a secret, fails without
a request when the CA cannot be read or is not PEM, and succeeds with
`insecureSkipVerify`, which `GET /$/models` reports as unverified. With a policy that
refuses loopback addresses the same provider is still refused. `server_tests.rs` locks
`tls.insecureSkipVerify` and checks that a `PATCH` turning it on is a `409` while the
CA certificate can still change. `tests/cli_server_settings.rs` adds providers from each
preset, with and without assignments, checks the refusals, and checks the warning in
the server's log. The UI's unit tests cover the presets, the TLS fields and which
patches need the acknowledgement, and `ui/tests/e2e/models.spec.ts` adds a provider
from a preset and turns verification off through the dialog.

These points settle what the addendum left open.

- An empty `tls`, or `insecureSkipVerify: false`, is accepted on an `http` endpoint,
  since a reset of its members through a merge patch can leave one behind. Only a
  `caCert` or `insecureSkipVerify: true` is refused there.
- A `caCert` with both a file and a secret is refused. `insecureSkipVerify` together
  with a `caCert` is accepted, and verification is off.
- A lock can name `tls`, `tls.caCert` or `tls.insecureSkipVerify`, but not a member
  of `caCert`.
- The UI asks for the acknowledgement when Save or the runtime JSON would send a patch
  that turns verification on, not when the checkbox is ticked, so the dialog also
  covers the JSON editor. The TLS fields appear only for providers with an `https`
  endpoint.
