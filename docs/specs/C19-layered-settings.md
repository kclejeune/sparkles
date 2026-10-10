# C19: Layered dataset settings

> **Status:** specified
>
> **Phases:** Phase 1 is the server: settings kinds, the declared layer, layered
> resolution, locks, the `/$/settings` routes and reloading on SIGHUP. Phase 2 is the
> `sparkles settings` command, the NixOS module and the change to `sparkles memory init`.
> Phase 3 is the UI's settings tab. Phase 4 makes the server's model configuration a
> layered settings kind that server administrators can change, with write-only API keys.
>
> **User docs:** none yet.
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

Not implemented yet.
