# C09: Authentication and dataset-level access control

> **Status:** implemented in part
>
> **Phases:** Phase 1 (all of it: Basic users, API tokens, OIDC sign-in for the UI,
> trusted proxy headers, CLI logins, remote `query`/`update`/`load`, the UI pages);
> Phase 2 in part (rate limiting of failed logins, a Content Security Policy for the UI);
> Phase 3 (graph-level ACLs) not built.
>
> **User docs:** [API: Authentication and access control](../API.md#authentication-and-access-control) · [API: Rate limiting](../API.md#rate-limiting) · [Features](../FEATURES.md#server-fuseki-equivalent-reasoning-validation-ui)
>
> This is the design as written before implementation; the [Outcome](#outcome) section at the end
> records how it landed.

Written as a clean-room spec; it was a must-have. Scope:

- `crates/sparkles-server`: HTTP, state, observability, CLI (including a minimal remote
  client);
- one option in `crates/sparkles` (`QueryOptions` for SERVICE and LOAD);
- `ui/`: login, sidebar user, permission-aware actions, API tokens page, CLI approval
  pages;
- `nix/module.nix` and `docs/API.md`.

## 1. Summary

**Before this feature.** The server has no authentication. Any client that reaches the
port can query, update, delete datasets, read metrics, make the server fetch URLs
(`SERVICE`, `LOAD <http…>`) and load local files (`LOAD <file:…>`). The README and the
NixOS module recommend nginx `basicAuthFile`: one shared password, all or nothing. The
CLI works only on local database directories.

**This spec adds**, behind an opt-in `serve --auth-config FILE`:

| Mechanism | For | Credential |
|---|---|---|
| Config users | scripts, small teams | HTTP Basic; argon2id hashes in the config |
| API tokens | CLI, scripts, CI | `Authorization: Bearer spk_…`. Stored hashed. Scoped, expiring and revocable. Minted through the API, the UI or the CLI; static tokens can also be listed in the config |
| OIDC login | the browser UI | Authorization code + PKCE with the operator's IdP, implemented natively in Rust. A server-side session behind a signed `HttpOnly` `SameSite=Lax` cookie |
| Trusted headers | forward-auth proxies (oauth2-proxy, Authelia, Tailscale serve, Cloudflare Access) | `Remote-User`/`X-Forwarded-User` and similar headers, only from configured proxy CIDRs or the Unix socket |
| CLI login | `sparkles auth login --server URL` | A browser loopback redirect or RFC 8628 device code. It yields an API token, stored in `~/.config/sparkles/credentials.toml` (0600) |
| Anonymous | public datasets | none; the config holds its grants |

On top of these, one permission model:

- per-dataset levels `read` < `write` < `admin`;
- server permissions `metrics`, `federate` and `server-admin`;
- roles, and a mapping from IdP or proxy groups to roles.

A single fail-closed authorization middleware enforces it. It hides datasets the caller
cannot read (404, never 403) and filters every listing.

**Without `--auth-config` nothing changes**: no credentials, same CORS, same routes, same
benchmark numbers. `/$/auth/config` says `{"enabled": false}`, and the other `/$/auth/*`
routes answer 404.

### 1.1 Threat model

Deployment: one Sparkles process serving several users or teams. It sits behind a
TLS-terminating proxy or on a private network (tailnet, VPN).

| Adversary | Can | Must not be able to |
|---|---|---|
| Unauthenticated network client | reach the port | read or change any dataset not granted to `anonymous`; learn which datasets exist; read metrics; make the server fetch URLs or files; forge proxy identity headers |
| Authenticated user | use their grants | read, change or see other datasets; escalate (create datasets, clone into foreign names, mint tokens broader or longer-lived than themselves); use outbound HTTP or local files without a grant |
| Malicious web page in the user's browser | send cross-origin requests with ambient credentials (session cookie, proxy cookie, cached Basic) | read responses; trigger writes (CSRF); log the user into another account (login CSRF) |
| Local process on the CLI user's machine | connect to 127.0.0.1 ports | obtain the token from the loopback flow without the PKCE verifier |
| Someone who sees a device-code prompt | guess or observe a user code | approve it without an authenticated UI session |
| Reader of logs, metrics or `<data>/auth/*.json` | read them | recover passwords, tokens, session ids or `Authorization` headers |

Out of scope:

- a malicious operator or host;
- a compromised IdP or proxy;
- DoS by authorized heavy queries ([C01](C01-observability-and-budgets.md) budgets);
- timing side channels in query execution;
- TLS itself (§10.4).

### 1.2 Goals and non-goals

**Goals**

- No auth by default, unchanged behavior for local use.
- With auth, **deny by default**.
- A dataset's existence leaks only to principals with at least `read` on it.
- Every route has an explicit required permission in one table. A router test fails
  when a route lacks an entry, and unknown routes need `server-admin`.
- Existing protocol clients keep working with credentials:
  - Jena `RDFConnection` and `s-query` (Basic, including token-as-password);
  - curl;
  - YASGUI (Bearer header).
- No secrets live in browser-readable storage. The UI authenticates only with an
  `HttpOnly` cookie.
- Phase 1 takes about 3–5 days (§12.1).

**Non-goals (Phase 1)**

- Graph- or triple-level ACLs (Phase 3; §12.3 explains why they are hard).
- IdP-issued JWT access tokens on the API (Phase 2). In Phase 1, OIDC logs in the
  *browser*; programs use Sparkles API tokens.
- Users managed through the API (config users are file-managed; external identities come
  from the IdP or proxy).
- Native TLS.
- Per-IP rate limiting (Phase 2; Phase 1 bounds the cost of failed logins and caps
  pending grants instead).
- Per-principal query budgets.
- Endpoint-level permissions.
- A JavaScript auth server or SSR. **The UI stays static files embedded in the binary.**

## 2. Principals and authentication

### 2.1 Principal kinds and precedence

| Kind | Log name | Source |
|---|---|---|
| `local` | none (not logged) | auth disabled: all permissions |
| `anonymous` | `anonymous` | nothing else applied |
| `user` | `user:{name}` | Basic, or a UI password login (session) |
| `token` | `token:{id}` | Bearer, Basic with a token as the password, or a UI token login (session) |
| `oidc` | `oidc:{name}` | a UI OIDC login (session) |
| `proxy` | `proxy:{name}` | trusted headers from a trusted peer |

Each request resolves to exactly one principal. The first applicable source wins:

1. **The `Authorization` header** (Bearer or Basic). Invalid credentials give **401**.
   They are never downgraded to anonymous, which makes typos visible and prevents
   probing.
2. **The session cookie.** A missing, expired, revoked or badly signed cookie is
   *ignored*, and the response clears it (`Max-Age=0`). A stale cookie must not break
   anonymous access to public data.
3. **Trusted headers**, only when the TCP peer is in `proxy.trusted` or the connection
   came in on the Unix socket (§2.5). Identity headers from any other peer are ignored
   and counted.
4. **Anonymous.**

"**Ambient**" principals are those whose credential the browser attaches by itself:
session and proxy principals, and Basic credentials cached by the browser. They get CSRF
protection (§5.3).

"**Interactive**" principals are session and proxy principals. Only they may approve
CLI logins (§6.3, §6.4).

### 2.2 Config users (Basic)

- `[[users]]` entries hold `name` and `password`, a PHC string
  `$argon2id$v=19$m=…,t=…,p=…$salt$hash` from `sparkles auth hash`.
- Only `argon2id` is accepted. Parameters below the OWASP minimum (`m=19456, t=2, p=1`)
  produce a startup WARN, not an error, so tests can use cheap hashes.
- Header parsing follows RFC 7617:
  - the scheme is case-insensitive;
  - base64 must be strict;
  - the decoded bytes must be UTF-8 (`charset="UTF-8"` is advertised);
  - the user-id is split at the first `:`.

  Anything malformed gets 401 `invalid credentials`.
- An unknown user runs one verification against a fixed dummy hash, so timing does not
  reveal names.
- A verified-credential cache and a semaphore bound the cost (§10.2).

### 2.3 API tokens

**Format.** `spk_` followed by 43 base64url characters (32 bytes from the OS RNG), 47
characters in all. The prefix makes leaked tokens easy to recognize and lets secret
scanners find them. Each token also has a public **id**, `tok_` followed by 12 lowercase
base32 characters, used in URLs, logs and the UI.

**At rest: SHA-256, not argon2id.**

- The store keeps `sha256:<hex>` of the whole token string. The token is shown once, at
  mint time.
- Slow hashes (argon2id, the OWASP password-storage guidance) protect *low-entropy human
  passwords* against offline guessing. A 256-bit random token cannot be guessed offline
  whatever the hash.
- argon2id on every API request would cost about 20 ms of CPU and 19 MiB of memory,
  which would be both a throughput limit and a DoS lever.
- Lookup is `HashMap<[u8; 32], TokenRef>` keyed by digest. Lookup timing can only reveal
  bits of the digest of attacker-chosen input, which is useless without a preimage.
- Tokens are always generated by Sparkles, so their entropy is guaranteed. Tokens
  supplied by users are never accepted for hashing.

**Two sources, one lookup:**

| | Static tokens | Minted tokens |
|---|---|---|
| Defined in | `[[tokens]]` in the config file | the token store `<data>/auth/tokens.json` |
| Created by | `sparkles auth gen-token`, then an edit and SIGHUP | `POST /$/auth/tokens`, the UI tokens page, or the CLI login and `token create` |
| Id | `cfg-{name}` | `tok_…` |
| Owner | none (a machine identity) | the minting identity (§2.3.1) |
| Expiry | optional | required; default `tokens_policy.default_ttl` (30d), at most `tokens_policy.max_ttl` (90d) |
| Revoked by | editing the config and SIGHUP | `DELETE /$/auth/tokens/{id}`, the UI, or `sparkles auth token revoke` / `auth logout` |

Minted token record (`tokens.json`, format 1, written with `write_file_atomic`, mode
0600, in a 0700 directory):

```json
{ "id": "tok_3k9x2m4q7p1z", "name": "laptop (sparkles CLI)", "hash": "sha256:…",
  "owner": { "kind": "oidc", "name": "alice@example.org", "groups": ["kg-editors"] },
  "parent": null,
  "scope": { "datasets": { "*": "admin" }, "server": ["*"] },
  "created": "2026-09-30T12:00:00Z", "expires": "2026-10-30T12:00:00Z",
  "via": "cli-device", "client": { "label": "sparkles CLI", "hostname": "laptop" } }
```

`lastUsed` is kept in memory and written with the next store write or at graceful
shutdown. The file never contains a token.

#### 2.3.1 Effective permissions of a token

```
eff(token) = scope(token) ∩ eff(parent)        if the token was minted by another token
           = scope(token) ∩ eff(owner)         otherwise
eff(owner) = grants of that identity under the *current* policy:
             user  → the [[users]] entry (gone → the token is invalid)
             oidc/proxy → default_roles + group_roles(groups recorded at mint)
                          + user_roles(name), and the identity must still be admitted (§2.7)
```

- Intersection is per dataset:
  `level(t, N) = min(scope_level(N), eff_level(parent or owner, N))`.
- Server permissions intersect as sets, where `"*"` means all of the owner's.
- A scope of `{"*": "admin"}` with `server: ["*"]` is therefore "everything I have",
  the default for CLI logins.
- **A token never exceeds its minter, now or later.** Removing a user's grant, a role
  mapping or an admission rule shrinks every token they minted at the next request.
- **Chains.** A token minted by a token records `parent`, and its expiry must be ≤ the
  parent's. When the parent is revoked or expires, the child is invalid (the lookup of
  the parent fails). Chains are limited to 4 levels.
- **Static tokens** have no owner. `eff` is their own grants: their `datasets`, `server`
  and `roles`.
- The groups of an oidc or proxy owner are those recorded at mint. They cannot be
  refreshed without the IdP, so the TTL bounds staleness, and admins can revoke by owner
  (§6.1).

### 2.4 OIDC login for the UI (native)

The server is an OIDC **relying party** using the authorization code flow with PKCE
(OpenID Connect Core §3.1, RFC 7636 `S256`), and discovery (OpenID Connect Discovery,
falling back to RFC 8414 metadata).

**Crate.** `openidconnect` 4.x (MIT), with `default-features = false`. A 30-line
`AsyncHttpClient` adapter over the workspace `reqwest` 0.13 avoids a second reqwest.
The adapter **does not follow redirects** (the crate's SSRF advice) and uses a 10 s
timeout. Custom `AdditionalClaims`
(`#[serde(flatten)] HashMap<String, serde_json::Value>`) exposes arbitrary claims such as
`groups`.

Flow:

1. **Start.** The UI's "Sign in with {display_name}" navigates to
   `GET /$/auth/oidc/login?return_to=/ui/datasets`.
   - `return_to` must start with `/ui/` and not with `//`; otherwise it becomes `/ui/`
     (no open redirect).
   - The server creates `state`, `nonce` and a PKCE verifier (32 random bytes each). It
     keeps `pending[state] = {nonce, verifier, return_to, created}` in memory (TTL 10 min,
     at most 10 000 entries, oldest evicted).
   - It sets the login cookie `__Host-sparkles_oidc=<state>` (signed, `HttpOnly`,
     `Secure`, `SameSite=Lax`, `Path=/`, `Max-Age=600`).
   - It answers 302 to the IdP's `authorization_endpoint`, with `response_type=code`,
     the configured `scopes`, `code_challenge_method=S256` and
     `redirect_uri = {public_url}/$/auth/oidc/callback`.
2. **Callback.** `GET /$/auth/oidc/callback?code&state`:
   - the `state` query value must equal the signed login cookie *and* be pending; it is
     removed in the same step (one-time). A mismatch → `error=state` (this is the
     login-CSRF defense);
   - IdP `error=…` → `error=idp`;
   - exchange the code with the verifier (`client_secret_basic` when
     `client_secret_file` is set, otherwise a public client);
   - verify the ID token: the JWS signature with a JWKS key, `alg` on the allow-list
     (default `RS256`, `ES256`), `iss`, `aud` containing `client_id`, `exp`/`iat` with
     60 s leeway, and `nonce`;
   - read the name from `name_claim` (default `email`; `preferred_username` and `sub`
     are allowed). Read groups from `groups_claim`; if that is absent from the ID token
     and a `userinfo_endpoint` exists, call UserInfo once (OIDC Core §5.3);
   - check admission (§2.7). Not admitted → `error=not_allowed` and no session;
   - create a session (§2.6), clear the login cookie, and redirect **303** to `return_to`.

   All errors redirect 303 to `/ui/login?error=<code>`, so the server renders no HTML.
3. **Logout.** `POST /$/auth/logout` (CSRF-checked) deletes the session and clears the
   cookie. It answers `{"redirect": url | null}`:
   - when the IdP advertises `end_session_endpoint` (RP-Initiated Logout 1.0), the URL is
     that endpoint with `id_token_hint` (kept in memory for the session; omitted after a
     restart, with `client_id` sent instead) and
     `post_logout_redirect_uri={public_url}/ui/`;
   - for proxy principals, it is `proxy.logout_url`.

**Discovery and keys.**

- Discovery runs at startup. When the IdP is down, the server still starts, logs a WARN,
  and retries lazily on the next login (30 s backoff). Until then, login answers
  `error=idp_unavailable`.
- JWKS is re-fetched on an unknown `kid` (at most once per 5 min) and hourly.
- The metadata `issuer` must equal the configured one exactly.

`[server] public_url` is **required** with `[oidc]`. It fixes the redirect URI, the
cookie attributes and the own-origin for CSRF checks, so they do not depend on `Host`.

### 2.5 Trusted-header auth (forward-auth proxies)

This mechanism is **off by default**; the `[proxy]` table in the config turns it on.

```toml
[proxy]
preset = "authelia"          # fills the header names; each can be overridden
trusted = ["127.0.0.1/32", "::1/128", "unix"]
# user_header = "Remote-User"; email_header = "Remote-Email"; groups_header = "Remote-Groups"
groups_separator = ","
name_from = "user"           # or "email": which header names the principal
logout_url = "https://auth.example.org/logout"
```

Presets (names from each product's documentation, cited from working knowledge; verify
when implementing):

| Preset | User | Email | Groups |
|---|---|---|---|
| `oauth2-proxy` | `X-Forwarded-User` | `X-Forwarded-Email` | `X-Forwarded-Groups` |
| `authelia` | `Remote-User` | `Remote-Email` | `Remote-Groups` |
| `tailscale` (`tailscale serve`) | `Tailscale-User-Login` | `Tailscale-User-Login` | none |
| `cloudflare-access` | `Cf-Access-Authenticated-User-Email` | the same | none (groups need Access's JWT; Phase 2) |

**Rules**

- **Trust.** Headers are honored only when the peer address (via
  `into_make_service_with_connect_info`) is inside a `trusted` CIDR, or when the
  connection arrived on `serve --unix-socket PATH` and `trusted` contains `"unix"`.
- **Untrusted peers.** Identity headers from any other peer are ignored (the request
  proceeds as if they were absent), counted in
  `sparkles_auth_untrusted_proxy_headers_total`, and logged at WARN once a minute.
- **Precedence.** An `Authorization` header wins over proxy headers, so CLI tokens work
  through the proxy. A valid session cookie also wins.
- **Validation.** The user header must be 1–256 bytes of visible ASCII, else ignored.
  Groups are split on `groups_separator` and trimmed.
- **Admission and roles** follow §2.7. Proxy principals are ambient: CSRF rules apply
  (§5.3).
- **Hard errors at startup:** `trusted` contains `0.0.0.0/0` or `::/0`; a CIDR does not
  parse; `[proxy]` has no user header.
- **Spoofing WARNs at startup.** Header spoofing is the whole risk of this mechanism.
  - `[proxy]` with the server listening on TCP at a **non-loopback** address:
    "trusted-header auth is enabled and the server listens on 0.0.0.0:3030. Any host in
    `trusted` can impersonate any user; make sure only the proxy can reach this port".
  - Any `trusted` range wider than a single host (for example `10.0.0.0/8`): "every host
    in 10.0.0.0/8 can set Remote-User".
  - `docs/API.md` states:
    - the proxy must **overwrite or strip** client-supplied identity headers on every
      route, including routes it exempts from auth;
    - the Unix socket (`--unix-socket`, mode 0660) is the safest transport;
    - `tailscale serve` connects from 127.0.0.1 through tailscaled.
- **Routes the proxy must let through unauthenticated** so the CLI can log in:
  `/$/auth/config`, `/$/auth/device`, `/$/auth/token`, and requests that carry
  `Authorization: Bearer spk_…`. Otherwise CLI clients use a separate proxy route that
  skips forward auth.

### 2.6 Sessions

**Store.** `<data>/auth/sessions.json` (0600) holds `sha256(session_id) → record`. The
store is written atomically on login and logout. Expired records are pruned at startup
and hourly, and there are at most 10 000 sessions (oldest evicted).

**Why a file.** Deploys and restarts should not log everyone out. Hashing the ids means
a leaked file grants nothing. The write rate (logins) is tiny. In-memory only was
rejected for the restarts; a signed stateless cookie was rejected because it cannot be
revoked.

Record:

```json
{ "id": "sha256:…", "method": "oidc", "principal": { "kind": "oidc",
  "name": "alice@example.org", "groups": ["kg-editors"] },
  "created": "…", "expires": "…" }
```

A session's permissions are recomputed at each request from its identity, as for token
owners (§2.3.1). A session from a token login stores `tokenId`: it dies with the token,
and `expires ≤` the token's expiry.

**Cookie.**

- Name: `__Host-sparkles_session`. The `__Host-` prefix requires `Secure`, `Path=/` and
  no `Domain`. When `public_url` is `http://localhost…` or `http://127.0.0.1…`, the
  cookie is `sparkles_session` without `Secure`, for development.
- Attributes: `HttpOnly`, `SameSite=Lax`, `Max-Age = session.ttl` (default 12h,
  absolute, not sliding).
- Value: 32 random bytes (base64url), **signed** with the `cookie` crate's `SignedJar`
  (HMAC-SHA256; `cookie` 0.18, MIT/Apache-2.0, feature `signed`). The signature rejects
  garbage cheaply and makes the key rotatable: replacing the key file logs everyone out.
- Why `Lax` and not `Strict`: `Strict` would drop the cookie when the user follows a
  link to the UI from elsewhere. CSRF is handled by §5.3.

**Key.** The session key is 64 random bytes (base64) read from `session.key_file`. The
default is `<data>/auth/session.key`, created with mode 0600 on first start. Subkeys are
derived with HMAC-SHA256: `k_cookie = HMAC(key, "cookie")`, `k_csrf = HMAC(key,
"csrf")`.

**Other UI logins** with the same session mechanism, so the UI never holds secrets in
JavaScript:

- `POST /$/auth/login` with `{"user": "bob", "password": "…"}` or `{"token": "spk_…"}`
  → 204 plus `Set-Cookie`, or 401 `invalid credentials`.
- This route is subject to the Origin check (login CSRF, §5.3).

### 2.7 External identities: admission, groups and roles

This applies to `oidc:` and `proxy:` principals:

```toml
[external]
allowed_users  = []                 # names; empty = no restriction
allowed_groups = ["sparkles"]       # empty = no restriction
default_roles  = []                 # roles every admitted identity gets
[external.group_roles]
"kg-editors" = ["wiki-editors"]
"kg-admins"  = ["server-admins"]
[external.user_roles]
"alice@example.org" = ["server-admins"]
```

- **Admission.** When both lists are empty, everyone the IdP or proxy authenticates is
  admitted. Otherwise the name must be in `allowed_users` or one of its groups in
  `allowed_groups`.
- **Not admitted.** OIDC → no session, `error=not_allowed`. Proxy → 403
  `{"error":"user not allowed"}` on every route except the public ones.
- **Grants** = `default_roles` ∪ `group_roles[g]` for each group ∪ `user_roles[name]`.
  An admitted identity with no roles can log in but sees nothing (deny by default).
- **WARN** when both admission lists are empty while `default_roles` is not: "every
  account at the IdP gets …".

### 2.8 Anonymous

`[anonymous]` holds grants. By default it has none: an anonymous caller can reach only
the Public and Caller routes of §3.4 and sees no datasets. Typical use is
`datasets = { public = "read" }`.

## 3. Permission model

### 3.1 Dataset levels

| Level | Allows |
|---|---|
| `read` | query (including `text:query` and `spk:vectorSearch`), explain, GSP GET/HEAD, SHACL, `DatasetInfo`, stats, schema, prefixes, commits, reasoning status and diagnostics, text status, `/$/ready/{ds}`, the dataset's tasks |
| `write` | `read`, plus SPARQL Update, GSP PUT/POST/DELETE, upload |
| `admin` | `write`, plus compact, backup, reason and unreason, text enable/disable/rebuild, result-cache clear, clone (source side), delete |

The levels are hierarchical. A write-only level would be meaningless: `DELETE WHERE` and
`INSERT … WHERE` read data, and their counts reveal it. Solid WAC's modes informed this:
`Control` becomes `admin`, and `Append` is dropped (§14).

### 3.2 Server permissions

| Permission | Allows |
|---|---|
| `metrics` | `/$/metrics` (text and JSON); the full dataset list in `/$/ready` |
| `federate` | outbound HTTP from queries and updates: `SERVICE`, `LOAD <http(s)…>` |
| `server-admin` | `admin` on every dataset, plus `metrics`, `federate`, `POST /$/datasets`, all tasks, all tokens (list and revoke), `LOAD <file:…>` |

- `--no-service` still disables SERVICE for everyone.
- `--read-only` is checked **after** authorization, for everyone (403
  `server is read-only`). It does not block auth state writes (sessions, tokens).

### 3.3 Grants and evaluation

Grantees are users, static tokens, roles and anonymous. Each has:

- `datasets: { pattern → level }`;
- `server: [permission]`;
- `roles: [role]`, for users and static tokens (roles do not nest).

Effective grants are the union over the grantee and its roles:

```
level(p, N) = Admin                                   if server-admin ∈ server(p)
            = max { lvl | (pat, lvl) ∈ grants(p), glob(pat, N) }   (None if no match)
```

Tokens intersect this with their owner or parent (§2.3.1).

- **`glob`.** `*` matches any run (possibly empty) of dataset-name characters, and every
  other character matches itself. Matching is case-sensitive; `"*"` matches all
  datasets.
- **No deny rules.** Grants are monotonic, so entry order never matters.
- **By name.** Grants are by dataset name, not id (as Fuseki's `allowedUsers`). A
  dataset recreated under the same name gets the same grants (open question 4).
- **Existence-independent.** `level(p, N)` never depends on whether `N` exists, which
  §5.1 relies on.

**Create and clone.**

- `POST /$/datasets` needs `server-admin`.
- `POST /$/datasets/{ds}/clone?name=NEW` needs `admin` on `ds` **and**
  `level(p, NEW) == Admin`. Otherwise it is 403
  `no admin access to the target name /NEW`. That leaks nothing (the caller typed the
  name), and the 409 existence check runs only after the target check passes.

### 3.4 Route table

`Need` values:

- `Public`: no check; invalid `Authorization` is still 401.
- `Caller`: anyone, anonymous included.
- `Authed`: any principal except anonymous.
- `Interactive`: a session or proxy principal (§2.1).
- `R`/`W`/`A`: that level on `{ds}`.
- `S(x)`: server permission `x`.

"Filtered" means the handler restricts output to datasets with `level ≥ read`.

| Route | Method | Need | Notes |
|---|---|---|---|
| `/`, `/ui`, `/ui/`, `/ui/{*path}` | GET | Public | static SPA, including `/ui/login`, `/ui/tokens`, `/ui/cli/*` |
| `/$/ping` | GET, POST | Public | |
| `/$/ready` | GET | Public | filtered `datasets` (all with `metrics`) |
| `/$/ready/{ds}` | GET | R | |
| `/$/whoami` | GET | Public | §5.5 |
| `/$/server` | GET | Caller | filtered `datasets`, plus `auth` |
| `/$/metrics` | GET | S(metrics) | |
| `/$/datasets` | GET | Caller | filtered, with `access` |
| `/$/datasets` | POST | S(server-admin) | |
| `/$/datasets/{ds}` | GET | R | |
| `/$/datasets/{ds}` | DELETE | A | |
| `/$/datasets/{ds}/clone` | POST | A | plus the target check (§3.3) |
| `/$/stats/{ds}`, `/$/schema/{ds}`, `/$/schema/{ds}/classes`, `/$/schema/{ds}/predicates`, `/$/prefixes/{ds}` | GET | R | |
| `/$/compact/{ds}`, `/$/backup/{ds}`, `/$/cache/clear/{ds}` | POST | A | |
| `/$/reason/{ds}` | GET | R | |
| `/$/reason/{ds}` | POST, DELETE | A | |
| `/$/reason/{ds}/diagnostics` | GET | R | |
| `/$/tasks` | GET | Caller | filtered by `task.dataset` or `task.target` |
| `/$/tasks/{id}` | GET | Caller | 404 `no such task` unless `read` on `task.dataset` |
| `/$/text/{ds}` | GET | R | |
| `/$/text/{ds}` | PUT, DELETE | A | |
| `/$/text/{ds}/rebuild` | POST | A | |
| `/$/commits/{ds}`, `/$/commits/{ds}/{reference}` | GET | R | |
| `/{ds}/sparql`, `/{ds}/query` | GET, HEAD, POST | R | plus `federate` for SERVICE (§3.5) |
| `/{ds}/explain` | GET, POST | R | |
| `/{ds}/update` | POST | W | plus LOAD rules (§3.5) |
| `/{ds}/data` | GET, HEAD | R | |
| `/{ds}/data` | PUT, POST, DELETE, and any other method | W | other methods get 405 |
| `/{ds}/get` | GET, HEAD | R | |
| `/{ds}/upload` | POST | W | |
| `/{ds}/shacl` | POST | R | |
| `/{ds}` | any | classified | see below |
| `/$/auth/config` | GET | Public | §5.6 |
| `/$/auth/login` | POST | Public | UI password or token login (§2.6) |
| `/$/auth/logout` | POST | Caller | a no-op 200 for non-session principals |
| `/$/auth/oidc/login`, `/$/auth/oidc/callback` | GET | Public | §2.4 |
| `/$/auth/tokens` | GET | Authed | own tokens; `?all=true` needs `server-admin` |
| `/$/auth/tokens` | POST | Authed | minting rules (§6.1) |
| `/$/auth/tokens/{id}` | DELETE | Authed | own, `self`, or any with `server-admin` |
| `/$/auth/tokens` | DELETE | S(server-admin) | `?owner=oidc:alice@…`: revoke all of an owner's tokens |
| `/$/auth/device` | POST | Public | RFC 8628 device authorization (§6.3) |
| `/$/auth/device/{user_code}` | GET | Interactive | grant details for the approval page |
| `/$/auth/device/{user_code}/approve`, `…/deny` | POST | Interactive | |
| `/$/auth/cli/authorize` | POST | Interactive | loopback approval (§6.4) |
| `/$/auth/token` | POST | Public | token endpoint for both CLI grants (§6.2) |
| any other matched route | any | S(server-admin) | fail closed |
| unmatched path | any | none | the router's 404 |

**`/{ds}` classification** (the inputs of `obs::route_op`):

- `update=` in the query string, or `application/sparql-update` → W;
- `query=`, or `application/sparql-query` → R;
- GET, HEAD or OPTIONS → R;
- POST `application/x-www-form-urlencoded` → R in the middleware, and `dataset_root`
  **re-checks W** after parsing the body, before delegating to the update handler;
- any other POST, PUT or DELETE → W.

**Protocol fix riding along.** The SPARQL 1.1 Protocol allows updates only via POST, so
`GET /{ds}?update=…` answers 405 `use POST for SPARQL Update`, with auth on or off.

### 3.5 Outbound requests and local files

| Operation | Auth disabled | Auth enabled |
|---|---|---|
| `SERVICE <http(s)…>` | allowed unless `--no-service` | needs `federate` |
| `LOAD <http(s)…>` | allowed | needs `federate` |
| `LOAD <file:…>` | allowed (today; open question 7) | needs `server-admin` |

- `QueryOptions` gains `allow_remote_load` and `allow_file_load` (default `true`);
  `update.rs::load` checks them.
- Handlers set `allow_service = st.allow_service && p.has(Federate)`, and the load flags
  in the same way.
- A refusal is a new `Error::NotPermitted(String)`, mapped to **403** and raised before
  any connection or file open:
  - `SERVICE requires the federate permission`;
  - `LOAD <http…> requires the federate permission`;
  - `LOAD <file:…> requires server-admin`.
- SERVICE and LOAD never forward the caller's credentials.

## 4. Configuration

### 4.1 File format and example

TOML, parsed with `serde` and `toml` (MIT/Apache-2.0) using `deny_unknown_fields`.

```toml
# /etc/sparkles/auth.toml. Mode 0600/0640, owned by the service user; never in /nix/store.
version = 1
realm = "sparkles"

[server]
public_url = "https://sparql.example.org"   # required with [oidc]; recommended always

[anonymous]
datasets = { public = "read" }

[roles.wiki-editors]
datasets = { wiki = "write", "wiki-*" = "write" }
[roles.server-admins]
server = ["server-admin"]
[roles.scrapers]
server = ["metrics"]

[[users]]
name = "bob"
password = "$argon2id$v=19$m=19456,t=2,p=1$3m9ZzM1uXbVnK0zq0N3f0Q$Qy2Zt3xw…"
roles = ["wiki-editors"]
datasets = { "team-*" = "read" }

[[tokens]]                       # static machine token
name = "prometheus"
hash = "sha256:6b3a55e0261b0304143f805a24924d0c1c44524821305f31d9277843b8a10f4e"
roles = ["scrapers"]

[tokens_policy]
default_ttl = "30d"
max_ttl = "90d"

[oidc]
issuer = "https://auth.example.org"
client_id = "sparkles"
client_secret_file = "/run/secrets/sparkles-oidc"  # omit for a public client (PKCE only)
scopes = ["openid", "profile", "email", "groups"]
name_claim = "email"
groups_claim = "groups"
display_name = "Example SSO"
# algorithms = ["RS256", "ES256"]

[external]
allowed_groups = ["sparkles"]
[external.group_roles]
"kg-editors" = ["wiki-editors"]
"kg-admins" = ["server-admins"]

[session]
ttl = "12h"
# key_file = "/var/lib/sparkles/auth/session.key"

# [proxy] …            (§2.5; off unless present)

[cors]
origins = ["https://yasgui.example.org"]  # default: none (same-origin only)
```

Durations are `<n>s|m|h|d`.

**Validation** (startup and reload; errors carry the line and column):

- `version` must be `1`.
- Names:
  - users, static tokens and roles: `[A-Za-z0-9_.@-]{1,64}`, with no `:`;
  - unique within each kind;
  - every referenced role exists.
- Passwords must be `$argon2id$`. Token hashes must be `sha256:` plus 64 lowercase hex
  characters, unique.
- Levels and server permissions come from §3; `"*"` in `server` is allowed only on
  minted scopes.
- Patterns are dataset names that may contain `*`.
- `[oidc]` requires `server.public_url` over `https`, except for localhost.
- `max_ttl` ≥ `default_ttl`.
- CORS origins must be `scheme://host[:port]`, and not `*`.
- `[proxy]` rules are in §2.5.
- **WARNs:**
  - the file is readable by group or others;
  - argon2 parameters are below the OWASP minimum;
  - a grantee has no grants;
  - `anonymous` holds more than `read`;
  - the proxy and external admission warnings of §2.5 and §2.7.

### 4.2 Files under `<data>/auth/` (directory 0700, files 0600)

| File | Contents | Written |
|---|---|---|
| `session.key` | 64 random bytes, base64 (when `session.key_file` is not set) | once, at first start |
| `tokens.json` | minted token records (hashes only) | on mint and revoke |
| `sessions.json` | session records (hashed ids) | on login, logout and prune |

All are written with `state::write_file_atomic`. A corrupt `tokens.json` or
`sessions.json` stops startup with the path in the error; nothing is silently reset.

### 4.3 Reload

- **SIGHUP** re-reads `--auth-config`. If the new file validates, the policy is swapped
  atomically (`ArcSwap<Policy>`), the password cache is cleared and OIDC discovery is
  redone if `[oidc]` changed. Otherwise the old policy stays, an ERROR is logged, and
  `sparkles_auth_reloads_total{result="error"}` is incremented.
- In-flight requests keep their policy.
- Sessions and minted tokens survive a reload, and their permissions follow the new
  policy at the next request.
- Auth cannot be switched on or off by reload.
- The file is not watched.

### 4.4 NixOS module

- `services.sparkles.auth.configFile` (`nullOr path`). It is passed as `--auth-config`.
  An assertion rejects paths under `/nix/store`. Use agenix or sops-nix secrets owned by
  the service user.
- `ExecReload = "kill -HUP $MAINPID"` when set.
- `services.sparkles.unixSocket` (`nullOr path`). It adds `--unix-socket`, and the nginx
  `upstream` then uses `unix:` so proxy headers can be trusted with `trusted = ["unix"]`.
  The socket is mode 0660, group `sparkles`, and nginx is added to that group.
- `nginx.virtualHost` description: drop "the server itself has no authentication". Say
  that nginx `basicAuthFile` and Sparkles auth must not both be enabled: nginx forwards
  its own `Authorization`, which Sparkles would then reject.
- The VM test gains an auth case: a token-protected dataset, 401 without the token, 200
  with it.

## 5. HTTP behavior

### 5.1 Decision procedure (the middleware)

For each request whose route matched:

1. **Peer.** Take `ConnectInfo<Peer>` (`Peer::Tcp(SocketAddr) | Peer::Unix`) and decide
   whether the peer is a trusted proxy.
2. **Authenticate** (§2.1).
   - A malformed or invalid `Authorization` header → 401.
   - A password verification that cannot get a semaphore permit within 5 s → 503
     `authentication busy` with `Retry-After: 1`.
   - A non-admitted proxy identity → 403 `user not allowed`.
3. **CSRF gates** (§5.3). They can refuse with 403.
4. **Need** from the route table (§3.4), with `N = {ds}` (`obs::ds_param`,
   percent-decoded).
5. **Dataset needs.** With `lvl = level(p, N)`:

   | `lvl` | Principal | Dataset exists | Response |
   |---|---|---|---|
   | None | anonymous | either | **401** + challenge |
   | None | authenticated | either | **404** `{"error":"no such dataset: /N"}` |
   | < need | anonymous | either | **401** |
   | < need | authenticated | no | **404** (same body) |
   | < need | authenticated | yes | **403** `{"error":"write access to /N required"}` (or `admin`) |
   | ≥ need | any | either | pass |

   The first two rows are existence-independent. The 404 body is byte-identical to the
   handler's (`http::dataset()`); `delete_dataset`'s `"no such dataset"` changes to
   match. A 403 happens only where `lvl ≥ read`, so the caller may already know the
   dataset exists.
6. **Other needs.**
   - `S(x)`: anonymous → 401; authenticated without `x` → 403, for example
     `metrics permission required`.
   - `Authed`: anonymous → 401.
   - `Interactive`: anonymous → 401; a Bearer or Basic principal → 403
     `this action requires signing in to the web UI`.
7. **Pass.** Insert `Extension<Principal>` into the request, record `principal` and
   `auth` on the request span, and put an `AuthReport` in the response extensions for
   `obs`.

With auth disabled, the middleware only inserts `Principal::local()`: one `Arc` clone.

**Layer order**, from outermost to innermost:

```
alloc → observe → trace → cors → auth → compression → body limit → handler
```

CORS preflights are answered before authentication. 401 and 403 responses still carry
CORS headers and `X-Request-Id`. `axum::serve` switches to
`into_make_service_with_connect_info::<Peer>()`: implement `Connected` for `Peer` over
both `TcpListener` and `UnixListener`, which both implement axum 0.8's `serve::Listener`.

### 5.2 401, challenges and 403 bodies

All auth errors use the existing `{"error": …}` JSON.

- **401 without credentials** (RFC 6750 §3.1: no `error` attribute when the request had
  no credentials):
  `WWW-Authenticate: Bearer realm="sparkles"` plus
  `WWW-Authenticate: Basic realm="sparkles", charset="UTF-8"`, and
  `{"error":"authentication required"}`.
- **401 with bad credentials:** `Bearer realm="sparkles", error="invalid_token",
  error_description="invalid credentials"` plus the Basic challenge, and
  `{"error":"invalid credentials"}`. The message is the same for an unknown user, a
  wrong password and an unknown token. `token expired` is the one distinct description.
- **Basic challenge suppression.** The `Basic` challenge is omitted when
  `Sec-Fetch-Mode` is `cors`, `same-origin` or `no-cors` (a script fetch), and whenever
  the server is configured without `[[users]]`. This stops the browser's native login
  dialog from covering the UI and caching ambient credentials. Navigations and
  non-browser clients still get it.
- **403** for a Bearer caller also carries `WWW-Authenticate: Bearer realm="sparkles",
  error="insufficient_scope"` (RFC 6750 §3.1).

### 5.3 CSRF

These gates apply only with auth enabled.

- **(a) Origin check.** It applies to every non-safe method (not GET, HEAD or OPTIONS)
  and to any request whose need is W, A or `S(server-admin)`. The request is refused
  with 403 `cross-origin request refused` when:
  - `Sec-Fetch-Site: cross-site` is present, or
  - `Origin` is present and is neither the server's own origin (`server.public_url`, or
    the scheme and `Host` of the request when that is unset) nor in `cors.origins`.

  Requests without `Origin` (curl, the CLI, Jena) pass. This is OWASP's "verify origin
  with standard headers". It also protects `POST /$/auth/login` against login CSRF and
  guards Basic credentials cached by a browser.
- **(b) Synchronizer token for ambient principals** (session or proxy) on non-safe
  methods: the `X-Sparkles-CSRF` header must equal
  `base64url(HMAC-SHA256(k_csrf, principal-binding))`, compared in constant time.
  - The binding is the raw session id for sessions, and `"proxy:" + name` for proxy
    principals.
  - `/$/whoami` returns this value as `csrfToken`.
  - Missing or wrong → 403 `CSRF token missing or invalid`.
  - A custom header cannot be sent cross-origin without a CORS preflight, which §5.4
    refuses. The HMAC binding also defeats a cross-site page that guesses the header
    name.
- The CLI grant endpoints (`/$/auth/device`, `/$/auth/token`) are not ambient-sensitive:
  they identify the client by the device code or PKCE verifier, never by cookies.

### 5.4 CORS

| | Auth disabled | Auth enabled |
|---|---|---|
| Origins | unchanged by this spec (see [Outcome](#outcome)) | `cors.origins` only; none by default |
| `Access-Control-Allow-Credentials` | unchanged by this spec | never sent |
| Request headers | mirrored | `authorization`, `content-type`, `accept`, `x-request-id` (not `x-sparkles-csrf`) |
| Methods | mirrored | GET, HEAD, POST, PUT, DELETE, OPTIONS |
| Exposed headers | `Sparkles-Commit`, `Sparkles-Dataset-Id`, `X-Request-Id`, `Sparkles-Inferences` | the same, plus `WWW-Authenticate` |

Cross-origin tools such as YASGUI send `Authorization: Bearer` explicitly, which needs
no credentialed CORS. Refusing credentialed CORS means no origin can read responses
using the session cookie or cached Basic credentials. The embedded UI is same-origin.

### 5.5 Filtered listings and `GET /$/whoami`

- `GET /$/datasets` and `/$/server`'s `datasets` list only readable datasets. Each
  `DatasetInfo` gains `access: "read" | "write" | "admin"`; it is absent when auth is
  disabled, and the UI then assumes `admin`.
- `/$/server` gains `"auth": {"enabled": bool}`.
- `/$/tasks` is filtered, and `/$/ready`'s `datasets` too unless the caller has
  `metrics`.
- `/$/whoami` returns 401 only for invalid `Authorization`:

```json
{ "authEnabled": true,
  "principal": { "kind": "oidc", "name": "alice@example.org", "displayName": "Alice",
                 "groups": ["kg-editors"] },
  "method": "session",
  "expires": "2026-09-30T23:59:00Z",
  "csrfToken": "q3v…",
  "server": ["metrics"],
  "datasets": { "team-a": "read", "wiki": "write" },
  "canMintTokens": true,
  "logout": true }
```

- `datasets` lists **existing** datasets with their effective level; patterns are never
  revealed.
- `method` is `none`, `basic`, `bearer`, `session` or `proxy`.
- `csrfToken` is present only for ambient principals.
- `logout` is true for session principals, and for proxy principals with `logout_url`.
- With auth disabled: `{"authEnabled": false, "principal": {"kind": "local"},
  "server": ["server-admin"], "datasets": {…all: "admin"}}`.

### 5.6 `GET /$/auth/config`

This route is public and always 200. Both the UI login page and the CLI discover the
available methods from it:

```json
{ "enabled": true,
  "methods": ["oidc", "token", "password", "proxy"],
  "oidc": { "loginUrl": "/$/auth/oidc/login", "displayName": "Example SSO" },
  "cli": { "authorizeUrl": "/ui/cli/authorize",
           "deviceAuthorizationEndpoint": "/$/auth/device",
           "tokenEndpoint": "/$/auth/token",
           "deviceVerificationUri": "/ui/cli/device" } }
```

- `password` is listed only when `[[users]]` is non-empty, and `proxy` only when
  `[proxy]` exists.
- URLs are relative to the server root. The CLI resolves them against `--server`.
- With auth disabled, the body is `{"enabled": false}`.

## 6. Token API and CLI grants

### 6.1 Tokens

`POST /$/auth/tokens` (Authed) takes a JSON body:

```json
{ "name": "ci-loader", "datasets": { "wiki": "write" }, "server": [],
  "expiresIn": "30d" }
```

Responses:

- **201** `{"token": "spk_…", "id": "tok_…", "name", "scope", "created", "expires"}`.
  The token appears only in this response.
- **Rules:**
  - the minter must not be anonymous;
  - `name` is 1–80 characters;
  - `expiresIn` ≤ `max_ttl`, and ≤ the minter's expiry when the minter is a token or a
    session;
  - scope entries must be valid levels and patterns; an empty scope is allowed but
    useless.

  Scopes need not be subsets of the minter's permissions, because §2.3.1 intersects
  them at use. The UI offers only subsets for clarity.
- 400 for bad input; 403 for a static token that tries to mint (static tokens are
  machine identities with no owner to bind to).
- Records `owner` (for a session or proxy minter: its identity and groups; for a user:
  `{kind: "user", name}`), and `parent` when minted by a token.

`GET /$/auth/tokens` returns
`{"tokens": [{id, name, scope, created, expires, lastUsed, via, client, owner}]}`.

- It lists tokens with the caller's owner; a token caller sees tokens of its own owner.
- `?all=true` (server-admin) lists every token plus static tokens (`cfg-…`, with
  `static: true`).
- It never returns hashes.

`DELETE /$/auth/tokens/{id}` → 204.

- `{id}` may be `self`, meaning the token in use.
- Allowed for the owner's principals (a session, or tokens with the same owner) and for
  `server-admin`. Otherwise 404 (hiding).
- Static tokens → 403 `static tokens are revoked in the auth config`.
- Revocation takes effect at the next request.

`DELETE /$/auth/tokens?owner=oidc:alice@example.org` (server-admin) →
`{"revoked": n}`.

### 6.2 Token endpoint

`POST /$/auth/token` takes an `application/x-www-form-urlencoded` body (JSON is accepted
too). This is the OAuth token endpoint shape of RFC 6749 §4.1.3 and RFC 8628 §3.4.

| `grant_type` | Parameters | Grant |
|---|---|---|
| `urn:ietf:params:oauth:grant-type:device_code` | `device_code` | §6.3 |
| `authorization_code` | `code`, `code_verifier`, `redirect_uri` | §6.4 |

- **Success:** 200
  `{"access_token": "spk_…", "token_type": "Bearer", "expires_in": s, "token_id": "tok_…",
  "principal": "oidc:alice@example.org"}`, with `Cache-Control: no-store`.
- **Errors** are 400 `{"error": code, "error_description": …}`:
  - `authorization_pending`, `slow_down`, `access_denied`, `expired_token` (RFC 8628
    §3.5);
  - `invalid_grant` (a wrong, used or expired code, or a PKCE mismatch);
  - `unsupported_grant_type`, `invalid_request`.

### 6.3 Device flow (RFC 8628)

This follows the flow of nimbus, the maintainer's earlier project (`cli-auth.ts`,
`cli/device`): 600 s expiry, 5 s interval, an unambiguous alphabet, one-time retrieval
and scope-bounded approval.

1. **Start.** `POST /$/auth/device` with optional form fields `label` and `hostname`
   (each at most 80 characters) → 200:

   ```json
   { "device_code": "<64 hex>", "user_code": "WDJB-MJHT",
     "verification_uri": "https://sparql.example.org/ui/cli/device",
     "verification_uri_complete": "https://sparql.example.org/ui/cli/device?code=WDJB-MJHT",
     "expires_in": 600, "interval": 5 }
   ```

   - The user code is 8 characters from `ABCDEFGHJKLMNPQRSTUVWXYZ23456789` (no 0, O, 1
     or I), formatted `XXXX-XXXX`. Input is case- and dash-insensitive.
   - `verification_uri` uses `public_url`, or the request's scheme and `Host`.
   - Grants live in memory (lost on restart; the CLI then gets `expired_token`). At most
     1 000 are pending; beyond that the answer is 503 `too many pending logins`.
2. **Poll.** `POST /$/auth/token` with the device `grant_type` returns
   `authorization_pending` until the grant is decided.
   - Polling faster than `interval` returns `slow_down`, and the grant's interval grows
     by 5 s (RFC 8628 §3.5).
   - Approved → the token, **once**; the grant is then deleted and a second poll gets
     `expired_token`.
   - Denied → `access_denied`. Past expiry → `expired_token`.
3. **Approval page.** `/ui/cli/device?code=…` requires an interactive principal: the UI
   redirects to `/ui/login?return_to=…` first.
   - The page calls `GET /$/auth/device/{user_code}`, which returns
     `{userCode, label, hostname, expiresIn, status}` or 404 for unknown or expired
     codes.
   - It shows the requesting client and a scope form: preset "All my access" (the
     default), or per-dataset levels among the user's own datasets, plus the expiry
     (default `default_ttl`).
   - **Approve** → `POST …/approve` with `{name, datasets, server, expiresIn}` mints the
     token (`via: "cli-device"`, `client: {label, hostname}`), owned by the approver, and
     stores the plaintext token in the grant until it is retrieved or expires.
   - **Deny** → `POST …/deny`.
4. **Guessing defense** (RFC 8628 §5.1).
   - Approval needs a logged-in interactive user, and the code alone grants nothing.
   - Each session may fail at most 20 user-code lookups per 10 min; after that, 429.
   - The code space is 32⁸ ≈ 1.1·10¹², and at most 1 000 codes are pending.

### 6.4 Browser loopback flow (RFC 8252 §7.3 with PKCE)

This improves on nimbus: the token never appears in a URL. The browser carries a
one-time **code** instead, which is useless without the PKCE verifier held by the CLI.

1. **CLI setup.** The CLI binds `127.0.0.1:0` and creates `state` (16 random bytes, hex)
   and a PKCE verifier (32 random bytes, base64url), with
   `challenge = base64url(SHA-256(verifier))`. It opens
   `{server}/ui/cli/authorize?port=P&state=S&code_challenge=C&code_challenge_method=S256&label=sparkles%20CLI&hostname=H`.
2. **Approval page.** `/ui/cli/authorize` (interactive; login first if needed) shows the
   same client and scope form as the device page.
   - **Approve** → `POST /$/auth/cli/authorize` with
     `{port, state, codeChallenge, name, datasets, server, expiresIn, label, hostname}`.
     - The server validates `port` (1024–65535) and the challenge (43 base64url
       characters).
     - It mints the token (`via: "cli-loopback"`), keeps it under a one-time code (32
       random bytes, TTL 120 s) bound to `(challenge, port)`.
     - It answers
       `{"redirect": "http://127.0.0.1:P/callback?code=…&state=S"}`, and the page
       navigates there.
     - The host is fixed to `127.0.0.1`; only the port comes from the caller, so there is
       no open redirect.
   - **Deny** → the page navigates to
     `http://127.0.0.1:P/callback?error=access_denied&state=S` without calling the
     server.
3. **CLI callback.** The CLI's listener accepts `GET /callback` only with the matching
   `state`, and answers a small UTF-8 page (with a `<meta charset>`, as nimbus notes)
   saying "Sparkles CLI is authorized; you can close this tab".
4. **Exchange.** The CLI calls `POST /$/auth/token` with `grant_type=authorization_code`,
   `code`, `code_verifier` and `redirect_uri=http://127.0.0.1:P/callback`.
   - The server checks `S256(verifier) == challenge` and the port, then removes the
     code.
   - A wrong verifier also consumes the code, so it cannot be retried; the answer is
     `invalid_grant`.
5. **Timeout.** The CLI gives up after 10 minutes (`browser authorization timed out`).

## 7. CLI

### 7.1 Offline commands (no server)

```
sparkles serve --auth-config FILE [--unix-socket PATH]
sparkles auth hash                 # password → $argon2id$…   (no-echo prompt on a TTY, else stdin)
sparkles auth gen-token --name N   # static token: token on stdout, [[tokens]] snippet on stderr
sparkles auth check --config FILE  # validate and summarize; exit 1 on errors
```

- `auth hash` sets its parameters explicitly (`Params::new(19456, 2, 1, None)`) with a
  16-byte `OsRng` salt. It uses `rpassword` (Apache-2.0/MIT) for the prompt.
- `--unix-socket` replaces TCP listening. It is created with mode 0660, and a stale
  socket file is removed at startup.

### 7.2 Login and token commands

```
sparkles auth login  --server URL [--web | --device] [--name LABEL] [--token TOKEN] [--set-default]
sparkles auth logout [--server URL]
sparkles auth status [--server URL]
sparkles auth token create --name N [--dataset DS=LEVEL]… [--server-perm P]… [--expires 30d] [--server URL]
sparkles auth token list   [--all] [--server URL]
sparkles auth token revoke ID [--server URL]
```

**`login`**

1. Normalize the URL (scheme, lowercase host, port, no trailing slash). Plain `http` is
   refused for non-loopback hosts unless `--insecure-http` is given, because tokens are
   bearer secrets.
2. `GET /$/auth/config`.
   - `enabled: false` → print `URL does not require authentication` and exit 0 without
     saving.
   - `--token` → validate it with `GET /$/whoami` and store it.
3. Otherwise pick the flow as nimbus does.
   - `--web` / `--device` force a flow.
   - Without a flag, use the browser when the session looks graphical: no
     `SSH_CONNECTION`/`SSH_TTY`; macOS and Windows always; elsewhere `DISPLAY` or
     `WAYLAND_DISPLAY` set.
   - Open the browser via `$BROWSER`, else `open`, `xdg-open` or
     `rundll32 url.dll,FileProtocolHandler`.
   - If the browser flow cannot start (no `cli.authorizeUrl`, bind or launch failure)
     and `--web` was not given, print the reason and fall back to the device flow.
   - Device output:
     `To authorize this device, visit:\n\n    {verification_uri_complete}\n\nand confirm the code {user_code}`.
4. On success, `GET /$/whoami` with the token, save it, and print
   `Logged in to https://sparql.example.org as oidc:alice@example.org (token tok_…,
   expires 2026-10-30)`.

**`logout`**: `DELETE /$/auth/tokens/self`, then remove the local entry. If the server
is unreachable or answers 401, remove it anyway and print a warning.

**`status`**: for each saved server (or the given one), print the server, whether it is
the default, and `GET /$/whoami` as principal, token id, expiry and dataset levels, or
`token invalid or expired: run sparkles auth login --server URL`.

**`token create|list|revoke`**: call §6.1 with the saved token.

- `create` prints the new token once on stdout, and id and expiry on stderr.
- `list` prints a table `ID NAME SCOPE EXPIRES LAST USED`.
- Minting from a CLI token works through the chain rules (§2.3.1).

### 7.3 Credentials file

`$XDG_CONFIG_HOME/sparkles/credentials.toml` (default
`~/.config/sparkles/credentials.toml`).

```toml
default_server = "https://sparql.example.org"

[servers."https://sparql.example.org"]
token = "spk_…"
token_id = "tok_3k9x2m4q7p1z"
principal = "oidc:alice@example.org"
expires = "2026-10-30T12:00:00Z"
```

- The directory is created with mode 0700.
- The file is written atomically: a temporary file created with mode 0600 (`OpenOptions`
  plus `mode(0o600)` before any byte is written), then renamed.
- On read, a file readable by group or others produces a warning with the `chmod 600`
  fix.
- The first login sets `default_server`, and `--set-default` overrides it.
- **Token resolution** for remote commands: `SPARKLES_TOKEN` env, then the entry for the
  normalized server URL, then no token (anonymous).

### 7.4 Minimal remote client

The CLI has no remote mode today. Add `--server URL` (clap `env = "SPARKLES_SERVER"`;
enable clap's `env` feature) and `--dataset NAME` to three commands. `--server` conflicts
with `--loc` and `--data`, and `--dataset` is required with it.

| Command | Request |
|---|---|
| `sparkles query --server URL --dataset DS [--results F] [--timeout S] [TEXT \| --query FILE]` | `POST /{ds}/sparql` with `Content-Type: application/sparql-query` and an `Accept` derived from `--results` (`text` → `text/tab-separated-values, text/turtle;q=0.9`; `json`, `xml`, `csv`, `tsv`, `sparkles`, `ttl`, `nt`, `nq`, `trig`, `jsonld`, `rdfxml` map to their media types). The body is streamed to stdout unchanged. `--explain` → `/{ds}/explain` |
| `sparkles update --server URL --dataset DS [TEXT \| --update FILE]` | `POST /{ds}/update` with `application/sparql-update`; prints the stats JSON |
| `sparkles load --server URL --dataset DS [--graph IRI] FILES…` | per file, `POST /{ds}/data?default` (or `?graph=IRI`), streamed from disk, with the content type from the extension. A `.gz` file is decompressed client-side while streaming. Prints `loaded N quads from FILE` |

- **HTTP client:** the workspace `reqwest` 0.13 blocking client (rustls), added to the
  server crate.
- **Exit status** is 0 or 1. On a non-2xx response, print the server's `error` field.
- **401** prints `not logged in to URL (run: sparkles auth login --server URL)`.
- **404** prints `no such dataset: /ds (or no access)`.

## 8. UI

The UI stays a static SvelteKit build embedded in the binary. Every auth flow that
needs a server-side step lives in the Rust server. The UI holds **no secrets**: it
authenticates with the `HttpOnly` session cookie, or not at all (proxy principals), and
keeps only the non-secret `csrfToken` in memory.

**Startup.** The layout loads `/$/auth/config` and `/$/whoami` into an `auth` store. When
auth is enabled, the principal is anonymous, and anonymous has no datasets, it redirects
to `/ui/login?return_to=<current path>`. Any 401 from a data call redirects the same way
(for example, an expired session).

**`/ui/login`**:

- a button "Sign in with {displayName}", shown with `oidc`; it navigates to
  `/$/auth/oidc/login?return_to=…`;
- an "API token" field, shown with `token`;
- a user and password form, shown with `password`.

The forms `POST /$/auth/login` and then navigate to `return_to`, validated as starting
with `/ui/`. `?error=` codes (`state`, `idp`, `idp_unavailable`, `not_allowed`) show a
plain message, for example "Your account is not allowed to use this server". Proxy
principals never see this page.

**Requests.** `api.ts` `request()` adds `X-Sparkles-CSRF: <csrfToken>` on non-GET/HEAD
requests, and the upload XHR sets it too. Cookies go with requests by default
(same-origin fetch and XHR).

**Sidebar user block** (`+layout.svelte`):

- the display name or principal name, with a method badge (`SSO`, `token`, `password`,
  `proxy`);
- an **API tokens** link when `canMintTokens`;
- **Sign out** when `logout`. It posts `/$/auth/logout`, then follows `redirect` when
  present, else goes to `/ui/login`.

**Gating** uses `can(ds, level)` and `hasServer(perm)` from whoami.

- **Hidden:**
  - **New dataset** unless `server-admin`;
  - **Delete**, **Compact**, **Backup**, **Clone**, **Clear cache**, reasoning Run/Drop
    and text index configuration unless `admin`;
  - **Upload** unless `write`;
  - the server page's metrics panels unless `metrics`.
- **Disabled, with the tooltip "Requires write access to /ds":** the query page's Update
  mode.
- Lists and the switcher show only filtered datasets.
- `readOnly` gating is kept. The server always enforces; the UI only hides.

**`/ui/tokens`** (API tokens):

- a table of own tokens: name, id, scope summary, created, expires, last used, and a
  **Revoke** button;
- a server-admin toggle for "All tokens", including static ones, read-only;
- **New token**:
  - a name;
  - an expiry (7, 30 or 90 days, capped at `max_ttl`);
  - a scope: "All my access", or per-dataset levels for datasets where the user has
    access, each at most the user's level;
  - server permissions the user holds.

  On create, a dialog shows the token once, with **Copy** and "You won't see it again".

**`/ui/cli/device`**: a code input, pre-filled from `?code=`, then the §6.3 approval
view. **`/ui/cli/authorize`**: the §6.4 approval view. Both show the client label and
hostname, the scope form and Approve/Deny, and on approval "Return to your terminal".

**Mock server.** `ui/mock/` gains an auth mode for UI tests.

## 9. Observability

**Access log.** `sparkles::access` events gain:

- `principal`: for example `oidc:alice@example.org` or `token:tok_3k9x2m4q7p1z`; absent
  when auth is disabled;
- `auth`: `none`, `basic`, `bearer`, `session` or `proxy`.

Both are also span fields (declared `Empty` in `observe`'s span, recorded by the auth
layer), so every event of the request carries them. Failed logins log `principal=-` and
`auth_error=invalid|expired|malformed|busy`, and never the attempted user name.

**Never logged:** `Authorization` and `Cookie` headers, passwords, tokens, token or
session hashes, PHC strings, session ids, CSRF tokens, OIDC codes, verifiers, device
codes, ID tokens.

**Audit events** at INFO, target `sparkles::audit`:

- `login`: method, principal, result;
- `logout`;
- `token_minted`: id, owner, via, scope summary, expires;
- `token_revoked`: id, by;
- `device_approved` / `device_denied`: user-code prefix `WDJB-…`, approver;
- `auth_reloaded`.

**Outcome.** A new `Outcome::Denied` (`denied`) covers 401, 403, CSRF refusals and the
hidden-dataset 404s from the auth layer. Clients see identical 404s, while operators can
tell them apart. The UI's `Outcome` type gains `'denied'`.

**Metrics** use closed label sets; there is no principal label.

| Name | Type | Labels |
|---|---|---|
| `sparkles_auth_failures_total` | counter | `scheme` = `basic`\|`bearer`\|`session`\|`oidc`, `reason` = `malformed`\|`invalid`\|`expired`\|`busy`\|`state`\|`idp`\|`not_allowed` |
| `sparkles_auth_denied_total` | counter | `kind` = `unauthenticated`\|`forbidden`\|`hidden`\|`cross_origin`\|`csrf`\|`not_interactive` |
| `sparkles_auth_logins_total` | counter | `method` = `oidc`\|`password`\|`token`, `result` = `ok`\|`denied`\|`error` |
| `sparkles_auth_tokens_minted_total` | counter | `via` = `api`\|`ui`\|`cli-loopback`\|`cli-device` |
| `sparkles_auth_tokens_revoked_total` | counter | |
| `sparkles_auth_tokens_active`, `sparkles_auth_sessions_active`, `sparkles_auth_device_grants_pending` | gauge | |
| `sparkles_auth_password_verifications_total` | counter | (argon2 runs, not cache hits) |
| `sparkles_auth_untrusted_proxy_headers_total` | counter | |
| `sparkles_auth_reloads_total` | counter | `result` = `ok`\|`error` |

## 10. Security details

### 10.1 Hashing, comparison, randomness

- **Passwords.** argon2id `m=19456 KiB, t=2, p=1`, 16-byte salt, via the RustCrypto
  `argon2` crate (MIT/Apache-2.0; PHC `PasswordHasher`/`PasswordVerifier`, whose output
  comparison is constant-time).
- **Tokens, session ids, device codes, loopback codes.** SHA-256 (`sha2`), stored or
  looked up by digest.
- **Direct comparisons** of secret-derived values (CSRF token, PKCE challenge, login
  state) use `subtle::ConstantTimeEq` (`subtle` 2.6, BSD-3, already in `Cargo.lock`).
- **Randomness.** All secrets come from the OS RNG (`rand::rngs::OsRng`, or `getrandom`).
- **Hygiene.** Decoded passwords live in `zeroize::Zeroizing<String>`. `Debug` for
  config, principal and store types redacts hashes and keys.

### 10.2 Bounding login cost (Phase 1) and rate limiting (Phase 2)

- **Password cache.** Key: `HMAC-SHA256(k_proc, user ‖ 0x00 ‖ password)`, with `k_proc`
  random per process. At most 10 000 entries, TTL 5 min, cleared on reload.
- **Semaphore.** At most `max(1, cores / 2)` argon2 verifications run concurrently, in
  `spawn_blocking`. A request that waits more than 5 s gets 503.
- **Caps.** At most 1 000 pending device grants, 10 000 pending OIDC logins, 10 000
  sessions, and 20 failed user-code lookups per session per 10 min.
- **Phase 2: per-IP failure buckets.** 10 failures per minute, burst 20, then 429 with
  `Retry-After`. The client IP comes from `Peer`, plus `X-Forwarded-For` only from
  `proxy.trusted` peers. A global budget is not used: it would let one attacker lock
  everyone out.

### 10.3 Never logging secrets

- The custom `MakeSpan` and `on_request(())` already log no headers, and must stay that
  way.
- A TRACE-level log-capture test (A13) asserts that no secret appears.
- SERVICE and LOAD do not forward credentials.
- The OIDC HTTP adapter does not log bodies.

### 10.4 TLS

- **Phase 1.**
  - Basic, Bearer and session cookies are bearer secrets; TLS is terminated by a proxy,
    or the server runs on a private network.
  - With `--auth-config` and a non-loopback TCP listener, the server logs a startup WARN
    ("credentials are accepted over plain HTTP on 0.0.0.0:3030; terminate TLS in front
    of the server").
  - `[oidc]` requires an `https` `public_url` (except for localhost), which also makes
    the cookie `__Host-` and `Secure`.
  - The CLI refuses plain `http` to non-loopback hosts without `--insecure-http`.
  - The default `--host 0.0.0.0` is unchanged (open question 8).
- **Phase 2.** Native TLS with `--tls-cert` and `--tls-key` (rustls via `tokio-rustls`;
  rustls is already in the tree), reloaded on SIGHUP.

### 10.5 Other hardening in this spec

- Refuse updates over GET (§3.4).
- Gate `LOAD <file:>` and outbound HTTP (§3.5).
- The OIDC HTTP client does not follow redirects.
- `return_to` is validated as a `/ui/` path.
- The loopback redirect host is fixed to `127.0.0.1`.
- `Cache-Control: no-store` on `/$/whoami`, `/$/auth/*` and authenticated `/$/*` JSON.

## 11. Design sketch

**`crates/sparkles-server/src/auth/`** (new):

| Module | Contents |
|---|---|
| `mod.rs` | `Level`, `ServerPerm`, `Principal`, `Kind`, `Need`, `need()`, the `ROUTES` table, `middleware`, `Peer` and its `Connected` impls |
| `config.rs` | TOML types (`deny_unknown_fields`), validation, warnings, `Policy` (immutable, in an `ArcSwap`), glob matching |
| `password.rs` | Basic parsing, argon2 verification, credential cache, semaphore |
| `tokens.rs` | token format, `TokenStore` (`tokens.json`), effective permissions (§2.3.1), handlers for `/$/auth/tokens*` |
| `session.rs` | `SessionStore` (`sessions.json`), key file, signed cookies, CSRF derivation, `/$/auth/login` and `/$/auth/logout` |
| `oidc.rs` | discovery and JWKS cache, the reqwest adapter, pending logins, `/$/auth/oidc/*` |
| `proxy.rs` | presets, CIDR matching (the `ipnet` crate, MIT/Apache-2.0, or a small matcher), header extraction |
| `cli_grants.rs` | device grants, loopback codes, `/$/auth/device*`, `/$/auth/cli/authorize`, `/$/auth/token` |
| `external.rs` | admission and group/user → role mapping |

**`crates/sparkles-server/src/remote/`** (new):

- `credentials.rs`: the credentials file;
- `login.rs`: browser detection and opening, the loopback listener (`std::net`, one
  request), device polling;
- `client.rs`: the reqwest wrapper, error mapping, and the `query`/`update`/`load`
  remote paths.

**Changes to existing files:**

- **`state.rs`:** `AppState.auth: Option<Arc<auth::Auth>>`. `Auth` holds the policy, the
  stores, the caches and the OIDC client.
- **`main.rs`:**
  - `--auth-config` and `--unix-socket`;
  - the SIGHUP handler;
  - `Cmd::Auth` subcommands;
  - `--server`/`--dataset` on `query`, `update` and `load` (`loc` becomes optional,
    `required_unless_present = "server"`);
  - `axum::serve(listener, router.into_make_service_with_connect_info::<Peer>())`;
  - persisting `lastUsed` at graceful shutdown.
- **`http.rs`:**
  - insert the auth layer between `compression` and `cors`;
  - build CORS from the policy;
  - the filtered handlers, the `dataset_root` re-check and the 405 for update over GET;
  - the clone target check;
  - `federate` and load flags in `query_options` and `update_endpoint`;
  - `whoami`; the `/$/auth/*` routes;
  - `dataset_info(ds, access)`; the normalized 404;
  - `Error::NotPermitted` → 403.
- **`obs.rs`:**
  - `ds_param` becomes `pub(crate)`;
  - the new span and event fields, `Outcome::Denied`, and the auth counters in
    `render_prometheus` and `metrics_json`;
  - `/$/ready` takes the principal;
  - `sparkles::audit` helpers.
- **`crates/sparkles`:** `QueryOptions { allow_remote_load, allow_file_load }` and
  `Error::NotPermitted`.

**Dependencies** of `sparkles-server`, all MIT/Apache-2.0 unless noted; record them in
[PROVENANCE](PROVENANCE.md):

- new crates:
  - `argon2` 0.5/0.6;
  - `toml`;
  - `openidconnect` 4.x (MIT, `default-features = false`);
  - `cookie` 0.18 (`signed`);
  - `rpassword`;
  - `ipnet` (optional);
- already in `Cargo.lock`, now direct: `subtle` (BSD-3), `zeroize`, `hmac` (the major
  matching `sha2` 0.11);
- workspace crates: `sha2`, `rand`, `arc-swap`, `chrono`, `reqwest` (blocking and async);
- clap's `env` feature.

**UI files:**

- `lib/auth.ts`: the store, `can`, `hasServer`, CSRF;
- `api.ts`: the CSRF header, `whoami`, `authConfig`, the token API, `DatasetInfo.access`
  and `'denied'`;
- routes `login`, `tokens`, `cli/device`, `cli/authorize`;
- the sidebar user block in `+layout.svelte`;
- gating in the dataset, query and server pages.

## 12. Phasing

### 12.1 Phase 1 (about 3–5 days)

- **Day 1: config and principals.**
  - the config (every section), `Policy`/`Principal`/levels/glob and validation;
  - Basic with argon2 (cache and semaphore), static tokens;
  - `auth hash`, `auth gen-token`, `auth check`;
  - unit tests.
- **Day 2: the middleware.**
  - the route table with its coverage test; 401/403/404; challenges;
  - Origin check, CORS, filtered listings, `whoami`, `auth/config`;
  - `federate`/LOAD gating, the update-over-GET fix, the clone target check;
  - `Peer`, `--unix-socket`, trusted headers with admission and roles;
  - obs fields, metrics, audit; SIGHUP reload.
- **Day 3: stores and flows.**
  - the token store and API (mint, list, revoke, chains, effective permissions);
  - the session store, key, signed cookie and CSRF synchronizer;
  - `/$/auth/login` and `/$/auth/logout`;
  - OIDC (discovery, PKCE, callback, admission, RP logout);
  - device and loopback grants, and `/$/auth/token`.
- **Day 4: CLI.**
  - the credentials file; `auth login` (web, device, fallback);
  - `auth logout`, `auth status`, `auth token create|list|revoke`;
  - remote `query`, `update` and `load`.
- **Day 5: UI and docs.**
  - UI: login page, sidebar user, gating, tokens page, device and authorize pages, CSRF
    header;
  - `docs/API.md` "Authentication" section and README rows;
  - NixOS options and VM test;
  - the remaining acceptance tests.

If time runs short, the order to cut from the end is:

1. the NixOS VM test case;
2. `auth token list --all` and revocation by owner;
3. RP-initiated IdP logout (keep the local logout).

### 12.2 Phase 2

- IdP JWT access tokens on the API (§12.4) and Cloudflare Access JWT verification
  (`Cf-Access-Jwt-Assertion`) instead of the plain header.
- Per-IP rate limiting (§10.2); native TLS (§10.4).
- A strict `Content-Security-Policy` for `/ui/` (hash the inline theme script in
  `app.html`).
- Sliding sessions with idle timeout; OIDC back-channel logout.
- Audit events for dataset admin operations.
- Endpoint-level restrictions, if requested.

### 12.3 Phase 3: graph-level ACLs

Fuseki's `fuseki-access` shows the shape: per user, a list of visible graphs, with
special names for the default graph. By its documentation it applies only to read-only
datasets. Sparkles would add visible-graph sets per grant, with writes still requiring
dataset `write`. The work is in the engine, because many fast paths assume the whole
dataset is visible:

- **Scans and paths.** Every scan must be restricted to allowed graph ids:
  - `GRAPH ?g` enumeration;
  - the union default graph (`--union-default-graph`, `default_graph_extra`);
  - property paths over a union, which must filter each hop, not the result;
  - blank-node graph names.

  The home is a `GraphFilter` in `Ctx`, applied in the scan operators.
- **Result cache** (`sparql/cache.rs`). It is keyed by query and snapshot version, so the
  key needs a visible-graph-set fingerprint, or caching must be off for filtered
  principals. Otherwise one user's cached rows serve another.
- **Statistics shortcuts.**
  - `COUNT` answered from index counts and generation stats (`stats.classes`,
    per-predicate distinct counts) includes hidden graphs.
  - The same statistics drive planner estimates visible in `/explain`.
  - `/$/stats` and `/$/schema` read them directly.
- **Search indexes.**
  - Full text: one Tantivy index per dataset. Top-k then filtering returns too few rows,
    and BM25 IDF leaks term statistics of hidden graphs. [F03](F03-full-text-search.md)
    rejected post-filtering, so the filter goes inside the collector, and scores still
    carry corpus-wide IDF.
  - Vector segments span graphs: filter before top-k.
- **Inferences.** `urn:x-sparkles:inferred` is materialized from all graphs, so derived
  triples can expose hidden facts. Hide it unless every source graph is visible, or
  materialize per graph.
- **Metadata.** Commit counts, `Sparkles-Commit` increments and `DatasetInfo.quads`
  reveal activity in hidden graphs.
- **Updates.** `DELETE/INSERT … WHERE`, `CLEAR ALL`, GSP whole-dataset writes and SHACL
  over the union all touch every graph.

Phase 1 prepares for this: every request carries a `Principal`, which can later reach the
engine through `QueryOptions`.

### 12.4 Phase 2 design note: IdP JWTs on the API

`Authorization: Bearer <JWT>`, where a value with two `.` separators is a JWT and one
with the `spk_` prefix is a Sparkles token.

- Validation follows RFC 7519 §7.2 with the `[oidc]` issuer's JWKS: `alg` allow-list,
  `iss`, `aud = api_audience`, `exp`/`nbf` with 60 s leeway.
- The principal is `oidc:{name_claim}`, mapped through `[external]`.
- Crate: `jsonwebtoken` (MIT; recent majors need an explicit crypto-backend feature), or
  `openidconnect`'s verifier types.

## 13. Acceptance examples

**Fixture.** `router_tests::auth` builds `AppState` with in-memory datasets `wiki`,
`team-a`, `secret` and `public`, each holding one triple, and a temporary data
directory. The config uses `m=8,t=1,p=1` hashes (allowed with a warning):

- anonymous: `public = "read"`;
- alice: `server-admin`, password `alice-pw`;
- bob: `wiki = "write"`, `"team-*" = "read"`, password `bob-pw`;
- carol: `wiki = "admin"`, `"wiki-*" = "admin"`;
- static tokens:
  - `T_PROM`: `metrics`;
  - `T_OLD`: `wiki = "read"`, `expires = "2020-01-01T00:00:00Z"`;
- `[external]`: `allowed_groups = ["sparkles"]`,
  `group_roles."kg-editors" = ["wiki-editors"]`, where role `wiki-editors` grants
  `wiki = "write"`.

`B(u)` is `Authorization: Basic base64(u:u-pw)`. Requests carry
`ConnectInfo(Peer::Tcp(127.0.0.2:1))` unless stated. `S(x)` is a session cookie obtained
through the step named `x`, and `csrf(x)` its whoami `csrfToken`.

### Core permission model

- **A1. Auth disabled.**
  - `GET /wiki/sparql?query=ASK{}` → 200, with no `WWW-Authenticate`.
  - `/$/datasets` lists all 4, with no `access`.
  - `/$/whoami` → `authEnabled: false`.
  - `/$/auth/config` → `{"enabled": false}`; `/$/auth/tokens` → 404.
  - A preflight from `https://x.example` is answered as without this spec.
- **A2. Anonymous.**
  - `/public/sparql?query=ASK{}` → 200.
  - `/wiki/sparql?…` → 401 with both challenges and
    `{"error":"authentication required"}`.
  - `/nope/sparql?…` → 401 with an identical status, headers and body.
- **A3. Invalid credentials are not anonymous.**
  - `/public/sparql` with `Bearer spk_wrong…` → 401 `error="invalid_token"`.
  - `B(bob)` with a wrong password → 401 `invalid credentials`; an unknown user → the
    same body.
  - `Basic !!!` → 401.
- **A4. Hidden versus forbidden** (`B(bob)`):
  - `POST /wiki/update` `INSERT DATA{<a> <b> <c>}` → 200.
  - `POST /team-a/update` → 403 `write access to /team-a required`.
  - `GET /secret/sparql` and `GET /nope/sparql` → 404, with bodies equal after
    substituting the name.
  - `/team-zzz/sparql` → 404.
  - `DELETE /$/datasets/secret` → the same 404 body.
- **A5. Filtered listings.**
  - `B(bob)` → `/$/datasets` is `["team-a", "wiki"]` with `read`/`write`.
  - Anonymous → `["public"]`. `B(alice)` → all 4, with `admin`.
  - `/$/server` as anonymous → 200 with `datasets` `[public]` and `auth.enabled: true`.
- **A6. Form POST re-check.** `B(bob)`: `POST /team-a` with form body
  `update=INSERT DATA{<a> <b> <c>}` → 403, and `team-a`'s head is unchanged. The same
  with `query=ASK{}` → 200.
- **A7. Update over GET.** `GET /wiki?update=CLEAR%20ALL` → 405 with auth on (as alice)
  and off, and the data is unchanged.
- **A8. Admin operations and clone.**
  - `B(bob)` `POST /$/compact/wiki` → 403; `B(carol)` → 202.
  - `B(carol)` `POST /$/datasets` → 403.
  - `B(carol)` clone `wiki` with `name=wiki-sandbox` → 202, then carol's listing includes
    it with `admin`.
  - `B(carol)` with `name=prod` → 403 `no admin access to the target name /prod`.
  - `B(bob)` with `name=wiki-2` → 403.
- **A9. Tasks.** Alice compacts `secret` as task `N`. `B(bob)`: `/$/tasks` lacks `N`,
  and `/$/tasks/N` → 404.
- **A10. Server routes.**
  - `/$/metrics`: anonymous → 401; `B(bob)` → 403; `Bearer T_PROM` → 200.
  - `/$/ping` anonymous → 200.
  - `/$/ready` anonymous → `datasets` `[public]`.
  - `/$/ready/secret` with `B(bob)` → 404.
- **A11. SERVICE and LOAD.** A local listener counts connections.
  - `B(bob)` `SERVICE <http://127.0.0.1:PORT/x>` on `/wiki/sparql` → 403
    `SERVICE requires the federate permission`, with 0 connections.
  - `B(alice)` → 1 connection.
  - `B(bob)` `LOAD <file:///etc/hostname>` → 403, and the head is unchanged.
  - `B(bob)` `LOAD <http://127.0.0.1:PORT/d.ttl>` → 403.
- **A12. Read-only.** With `--read-only`, `B(alice)` `POST /$/compact/wiki` → 403
  `server is read-only`; anonymous → 401.

### CSRF, CORS, logs, metrics

- **A13. Logs contain no secrets.** Capture logs at TRACE for all targets while running
  A2–A30. Every access event of an authenticated request has `principal=` and `auth=`.
  The output contains none of:
  - `bob-pw`, any `spk_` token, any `sha256:` hash, `$argon2id$`;
  - session cookie values, `code_verifier` values, device codes;
  - `authorization` or `cookie` in any case.
- **A14. CORS.**
  - Without `cors.origins`, a preflight from `https://evil.example` → no
    `Access-Control-Allow-Origin`.
  - With `origins = ["https://yasgui.example"]` → the ACAO echoes that origin, allowed
    headers include `authorization`, and there is no `Allow-Credentials`.
- **A15. Origin check.**
  - `POST /wiki/update` with `B(bob)` and `Origin: https://evil.example` → 403
    `cross-origin request refused`, with no commit.
  - With `Origin` equal to `public_url` → 200.
  - `POST /$/auth/login` with a cross-site Origin → 403 (login CSRF).
- **A16. Auth metrics.** After A3, A4 and A15,
  `sparkles_auth_failures_total{scheme="bearer",reason="invalid"}`,
  `sparkles_auth_denied_total{kind="hidden"}` and `{kind="cross_origin"}` are each ≥ 1,
  and `sparkles_requests_total` has `outcome="denied"`.
- **A17. Reload.**
  - Remove bob's `wiki` grant and call `reload()`: bob's next `/wiki/sparql` → 404.
  - An invalid file (key `dataset = {}`) → `reload()` errs, the old policy stays, and
    `reloads_total{result="error"}` is incremented.

### API tokens

- **A18. Mint, use, list, revoke.**
  - `S(password bob)` plus `csrf`: `POST /$/auth/tokens`
    `{"name":"ci","datasets":{"wiki":"read"},"expiresIn":"7d"}` → 201 with `token`
    matching `^spk_[A-Za-z0-9_-]{43}$`.
  - `Bearer token`: `/wiki/sparql` → 200; `/wiki/update` → 403 (the scope is `read`);
    `/team-a/sparql` → 404 (not in the scope).
  - `GET /$/auth/tokens` → one entry, with no `hash` and no `token` field.
  - `DELETE /$/auth/tokens/{id}` → 204, and the token then gets 401.
  - `tokens.json` contains no `spk_`, and its mode is 0600.
- **A19. Owner shrink and chains.**
  - Bob mints `{"datasets":{"*":"admin"},"server":["*"]}` as `T1`.
  - `T1`: `POST /team-a/update` → 403 (bob has only `read` on it); `/$/metrics` → 403.
  - Remove bob's `team-*` grant and reload: `T1` on `/team-a/sparql` → 404.
  - `T1` mints `T2` with `expiresIn: "365d"` → 400 (exceeds the parent). With `"1d"` →
    201.
  - Revoking `T1` makes `T2` → 401.
- **A20. Mint rules.**
  - Anonymous `POST /$/auth/tokens` → 401.
  - `Bearer T_PROM` (static) → 403.
  - `expiresIn: "400d"` → 400 (above `max_ttl`).
  - `Bearer T_OLD` → 401 `token expired`.
  - `DELETE /$/auth/tokens/cfg-prometheus` as alice → 403.
- **A21. Persistence.** Mint a token, drop the `AppState`, and rebuild it from the same
  data directory: the token still works, and a token revoked before the rebuild still
  fails.

### Trusted headers

- **A22. Trusted peer.** Configure `[proxy] preset="authelia"`,
  `trusted=["127.0.0.1/32","unix"]`. From peer `127.0.0.1`, with
  `Remote-User: dave`, `Remote-Groups: sparkles,kg-editors`:
  - `/$/whoami` → `proxy:dave`, `wiki: write`, and a `csrfToken`;
  - `POST /wiki/update` without `X-Sparkles-CSRF` → 403
    `CSRF token missing or invalid`, and with `csrf` → 200.
- **A23. Untrusted peer.** The same headers from `127.0.0.2` → anonymous (`/wiki/sparql`
  → 401), and `sparkles_auth_untrusted_proxy_headers_total` += 1. From `Peer::Unix` with
  `"unix"` trusted → `proxy:dave`.
- **A24. Admission and precedence.**
  - `Remote-Groups: kg-editors` only (not in `allowed_groups`) → 403
    `user not allowed`.
  - Proxy headers plus `B(bob)` → the principal is `user:bob`.
  - A config with `trusted = ["0.0.0.0/0"]` → a load error; `trusted = ["10.0.0.0/8"]`
    → a warning naming the range.

### OIDC and sessions

A mock IdP runs in-process in the tests (axum): discovery, a JWKS with a fixed RS256
test key, an `/authorize` that immediately redirects with a code, a `/token` that signs
an ID token with the requested nonce and configurable claims, and `end_session_endpoint`.

- **A25. Login redirect.** `GET /$/auth/oidc/login?return_to=/ui/datasets` → 302 to the
  mock `/authorize`, with `code_challenge_method=S256`, a 43-character `code_challenge`,
  `state`, `nonce`, and `redirect_uri={public_url}/$/auth/oidc/callback`. It sets
  `__Host-sparkles_oidc` (HttpOnly, Secure, SameSite=Lax, Max-Age=600).
  `return_to=https://evil.example` → stored as `/ui/`.
- **A26. Callback success.** Follow the redirects with claims
  `email=alice@example.org, groups=[sparkles, kg-editors]` → 303 to `/ui/datasets`, with
  `Set-Cookie: __Host-sparkles_session=…; HttpOnly; Secure; SameSite=Lax; Path=/;
  Max-Age=43200`.
  - `/$/whoami` with the cookie → `oidc:alice@example.org`, `method: "session"`,
    `wiki: write`, and a `csrfToken`.
  - `sessions.json` holds one record and not the cookie value.
- **A27. Callback failures.**
  - A `state` that does not match the login cookie → 303 `/ui/login?error=state`, with
    no session.
  - A replayed callback (same `state` twice) → `error=state`.
  - The mock returns an ID token with the wrong nonce, the wrong `aud` or an expired
    `exp` → `error=idp`.
  - Groups `[kg-editors]` only → `error=not_allowed`, with no session and
    `logins_total{method="oidc",result="denied"}` += 1.
- **A28. Session CSRF and logout.**
  - With `S(oidc alice)`, `POST /wiki/update` without the header → 403; with `csrf` →
    200.
  - `POST /$/auth/logout` → 200 `{"redirect": "<mock end_session>?id_token_hint=…&
    post_logout_redirect_uri=…"}`, the cookie is cleared, and the old cookie then
    resolves to anonymous (`/wiki/sparql` → 401).
  - A cookie with a tampered signature → anonymous, and the response clears it.
- **A29. Session persistence and key rotation.** Rebuild `AppState` from the same data
  directory: the session still works. Replace `session.key` and rebuild: it resolves to
  anonymous.
- **A30. Password and token UI login.**
  - `POST /$/auth/login` `{"user":"bob","password":"bob-pw"}` from the same origin → 204
    plus `Set-Cookie`, and whoami → `user:bob`.
  - A wrong password → 401 with no cookie.
  - `{"token": T}` → a session whose `expires` ≤ `T`'s expiry, which dies when `T` is
    revoked.

### CLI grants

- **A31. Device happy path.**
  - `POST /$/auth/device` with `label=sparkles CLI&hostname=h` → 200 with
    `device_code` (64 hex), `user_code` matching
    `^[A-HJ-NP-Z2-9]{4}-[A-HJ-NP-Z2-9]{4}$`, `expires_in: 600`, `interval: 5`.
  - An immediate poll → 400 `authorization_pending`. Another within 5 s → `slow_down`.
  - With `S(oidc alice)` plus `csrf`: `GET /$/auth/device/{code}` → label and hostname;
    `POST …/approve` `{"name":"laptop","datasets":{"*":"admin"},"server":["*"],
    "expiresIn":"30d"}` → 200.
  - The next poll → 200 `access_token`, `token_type: "Bearer"`,
    `principal: "oidc:alice@example.org"`. A further poll → `expired_token`.
  - The token lists as `via: "cli-device"`.
- **A32. Device refusals.**
  - `…/deny` → the poll gets `access_denied`.
  - With the clock advanced 601 s → `expired_token`.
  - Approval with `Bearer <token>` instead of a session → 403
    `this action requires signing in to the web UI`.
  - An unknown code → 404; the 21st failed lookup within 10 min → 429.
- **A33. Loopback.**
  - With `S(oidc alice)`: `POST /$/auth/cli/authorize` `{port: 50123, state: "s1",
    codeChallenge: C, …}` → 200 with `redirect` matching
    `^http://127\.0\.0\.1:50123/callback\?code=[A-Za-z0-9_-]{43}&state=s1$`.
  - `port: 80` → 400.
  - `POST /$/auth/token` with `grant_type=authorization_code`, the code and a wrong
    verifier → 400 `invalid_grant`; the correct verifier afterwards → still
    `invalid_grant` (the code was consumed).
  - A fresh authorization with the correct verifier → 200 token; reuse →
    `invalid_grant`; after 121 s → `invalid_grant`.

### CLI end to end

Run against a real listener on 127.0.0.1 with `HOME` and `XDG_CONFIG_HOME` in a
temporary directory.

- **A34. `auth login --device`.** A helper thread reads the printed user code and
  approves it with an alice session. The command exits 0 and prints
  `Logged in to http://127.0.0.1:PORT as oidc:alice@example.org`.
  `credentials.toml` has mode 0600, its directory 0700, and it contains the token and
  `token_id`.
- **A35. `auth login --web`.** `BROWSER` is set to a test helper that performs the
  §6.4 approval with an alice session and then GETs the redirect URL. The command exits
  0 with a stored token.
  - Without `--web` and with `SSH_CONNECTION` set, the device flow is chosen.
  - With `--web` against a server without `cli.authorizeUrl` → exit 1; without
    `--web` → fallback to the device flow.
- **A36. Remote client.**
  - `sparkles query --server URL --dataset wiki --results json 'ASK{}'` → exit 0 with
    `"boolean":true`.
  - `sparkles update --server URL --dataset wiki 'INSERT DATA{<a> <b> <c>}'` → exit 0.
  - `sparkles load --server URL --dataset wiki data.ttl.gz` → `loaded N quads`.
  - With `SPARKLES_TOKEN=bad` → exit 1 with `not logged in to URL (run: sparkles auth
    login --server URL)`.
  - `--dataset secret` → exit 1 `no such dataset: /secret (or no access)`.
  - `--server http://example.org` → exit 1 unless `--insecure-http` is given.
- **A37. Status, tokens and logout.**
  - `auth status` prints the principal, token id and expiry.
  - `auth token create --name ci --dataset wiki=read --expires 7d` → one `spk_` line on
    stdout. `auth token list` shows it; `auth token revoke ID` → it then gets 401.
  - `auth logout` → the server-side token then gets 401, and the entry is gone from
    `credentials.toml`.
- **A38. Offline CLI.**
  - `printf 'pw\n' | sparkles auth hash` matches
    `^\$argon2id\$v=19\$m=19456,t=2,p=1\$[A-Za-z0-9+/]{22}\$[A-Za-z0-9+/]{43}$`, and the
    hash verifies.
  - `auth gen-token --name x` → a `spk_` line on stdout and a `[[tokens]]` snippet on
    stderr whose hash matches.
  - `auth check --config bad.toml` → exit 1 naming the key, line and column.
  - `serve --auth-config missing.toml` → exit 1 before binding.

### UI (Playwright against the mock server)

- **A39. Login and gating.**
  - Anonymous `/ui/datasets` → redirect to `/ui/login?return_to=%2Fui%2Fdatasets`.
  - Token login → back to Datasets, showing only `team-a` and `wiki`.
  - On `team-a`, the Upload and Delete buttons are absent, and Update mode is disabled
    with the tooltip.
  - The sidebar shows `bob` with a `password` or `token` badge, and Sign out returns to
    the login page.
- **A40. Tokens and device pages.**
  - `/ui/tokens` → New token → the token is shown once. After a reload, the list shows
    the name without the token. Revoke removes the row.
  - `/ui/cli/device?code=WDJB-MJHT` while signed out → redirect to login, then back.
    Approve → "Return to your terminal".

### Structure

- **A41. Route coverage.** A test enumerates every route template in `router()` and
  asserts each is in `auth::ROUTES` with a `need` for each method it serves. An unknown
  template gets `S(server-admin)` at run time, and the test fails.
- **A42. Login cost.**
  - 50 concurrent `B(bob)` requests run argon2 once (`password_verifications_total`
    += 1).
  - 50 concurrent wrong-password requests for `nobody` never run more than `permits`
    verifications at a time, and each runs exactly one.

## 14. Rejected alternatives

- **Auth only in the proxy.** It cannot filter `/$/datasets`, hide datasets, or tell a
  read from a write on `/{ds}` (the operation is in the body). Trusted headers keep
  proxy SSO while Sparkles authorizes.
- **A JS auth server (better-auth) or SvelteKit SSR for OIDC.** It would be a second
  process and runtime, and the UI would no longer be static files in the binary. The
  maintainer ruled it out; the Rust relying party is about one day of work with
  `openidconnect`.
- **Tokens or passwords in `sessionStorage` or `localStorage` for the UI.** XSS could
  read them, and the Basic variant stores a password-equivalent. `HttpOnly` cookies with
  a CSRF synchronizer keep secrets out of JavaScript.
- **Stateless signed session cookies (no store).** They cannot be revoked at logout or
  after admission changes. A store of hashed ids costs little.
- **Session store in memory only.** Every deploy would log everyone out.
- **argon2id for API tokens.** It costs about 20 ms per request for no gain on 256-bit
  random secrets (§2.3).
- **JWT (self-contained) Sparkles tokens.** Revocation would need a denylist anyway, and
  scope changes would need reissuing. Opaque tokens plus a lookup are simpler and
  immediately revocable.
- **The token in the loopback redirect URL (as in nimbus).** It would land in browser
  history. A one-time code plus PKCE costs one extra request.
- **Mint-time scope checks only.** The token would outlive the owner's permission
  changes. Intersection at use (§2.3.1) is always current.
- **Shiro-style URL rules in the config, or full Solid WAC ACL documents.** Grants by
  dataset and level are easier to audit, and the route table keeps URL mapping in
  tested code. WAC needs WebIDs and per-resource ACL discovery.
- **Deny rules, and 403 for hidden datasets.** Deny rules make evaluation
  order-dependent, and a 403 leaks existence.
- **Trusting proxy headers from any peer, or with a global on/off flag.** Anyone who
  reaches the port could then impersonate anyone. Headers are honored only per peer
  CIDR or on the socket.

## 15. Open questions

1. Should `DELETE /$/datasets/{ds}` need `server-admin` rather than dataset `admin`?
2. Should a pattern `admin` grant allow `POST /$/datasets` for matching names
   (self-service namespaces)? Currently only clone creates there.
3. Pepper for password hashes, from a separate key file?
4. Should grants bind to dataset ids as well as names, so that a recreated dataset does
   not inherit access?
5. Should authenticated principals get `federate` by default, for compatibility?
   Currently no.
6. Should `/$/server` require an authenticated principal, hiding the version from
   anonymous callers?
7. With auth disabled, should `LOAD <file:>` stay allowed? (Perhaps a
   `--allow-file-load` flag that defaults to off on non-loopback hosts.)
8. Should the CLI default `--host` become `127.0.0.1`?
9. Should device grants persist across restarts? Currently they are in memory only;
   they expire within 10 min anyway.
10. Should oidc and proxy token owners' groups refresh on each UI login, updating their
    tokens' recorded groups? That would be friendlier, but changes token permissions
    retroactively.
11. Should credentialed CORS be allowed for listed origins, so that a separate web app
    can use session cookies?

## 16. Sources

- **Sparkles repository** (read when writing this spec):
  - `crates/sparkles-server/src/http.rs`: `router()` and its layers, all handlers,
    `dataset()`, the read-only checks, `dataset_root`, clone, tasks, CORS.
  - `http/router_tests.rs`: the harness.
  - `state.rs`: `AppState`, `valid_name`, registry, `write_file_atomic`, tasks.
  - `obs.rs`: `observe`, `MakeSpan`, `route_op`, `ds_param`, `Outcome`, metrics,
    readiness.
  - `main.rs`: `serve` flags; the `query`, `update` and `load` definitions (local only);
    `axum::serve`.
  - `ui.rs`; `crates/sparkles-server/Cargo.toml`, `Cargo.toml`, `Cargo.lock`.
  - `crates/sparkles/src/sparql/exec.rs` (`service`), `sparql/update.rs` (`load`,
    including `file://`), `vector.rs`.
  - `nix/module.nix`; `docs/API.md`; `README.md` (auth rows).
  - `ui/src/lib/api.ts` (`request`, `ready`, the `upload` XHR),
    `ui/src/lib/storage.ts`, `ui/src/routes/**` (`readOnly` gating).
  - the specs [C01](C01-observability-and-budgets.md), [C06](C06-clone-to-sandbox.md),
    [F04](F04-vector-search.md) and [PROVENANCE](PROVENANCE.md) (format and style only).
- **nimbus, the maintainer's own earlier project** (read with the maintainer's
  permission):
  - `cmd/nimbus/login.go`: flow selection, browser detection and opening, the loopback
    listener with state, device polling, fallback;
  - `web/src/lib/server/cache/cli-auth.ts`: device start and poll, the user-code
    alphabet, expiry and interval, one-time retrieval, auth-config discovery;
  - `web/src/routes/cli/+page.server.ts` and `web/src/routes/cli/device/+page.server.ts`:
    the approval pages, scope bounded by the approver, port validation, the fixed
    loopback host;
  - `internal/api/client.go` (`PollDeviceToken`).
- **Apache Jena Fuseki documentation** (Apache-2.0):
  - "Data Access Control for Fuseki", fetched 2026-09-30,
    https://jena.apache.org/documentation/fuseki2/fuseki-data-access-control.html.
    Used for `--auth`/`--passwd`, `allowedUsers` at server, dataset and endpoint levels,
    where both levels must allow, `"*"` meaning any authenticated user, and graph-level
    `access:entry` being limited to read-only datasets.
  - The Shiro `shiro.ini` URL filters: cited from working knowledge.
- **IETF RFCs** (cited from working knowledge, not re-fetched):
  - RFC 9110 §11 (401, 403, `WWW-Authenticate`);
  - RFC 7617 (Basic);
  - RFC 6750 (Bearer; header method only; challenge attributes);
  - RFC 6749 §4.1.3 and §5.2 (token endpoint, error responses);
  - RFC 7636 (PKCE, S256);
  - RFC 8628 (device authorization grant: §3.2 response, §3.4 grant type, §3.5 polling
    errors and `slow_down`, §5.1 user-code guessing);
  - RFC 8252 §7.3 (loopback redirect for native apps);
  - RFC 8414 (authorization-server metadata);
  - RFC 7519 §7.2 and RFC 7517 (JWT validation, JWK sets; used for the Phase 2 note and
    ID-token checks);
  - RFC 6265 (cookies) and the `__Host-` prefix of the cookie-prefixes draft;
  - RFC 3339.
- **OpenID Connect** Core 1.0 (§3.1 code flow, §3.1.3.7 ID token validation, §5.3
  UserInfo), Discovery 1.0, and RP-Initiated Logout 1.0: cited from working knowledge.
- **W3C** Solid Web Access Control (access modes) and the SPARQL 1.1 Protocol (updates
  are POST-only): cited from working knowledge.
- **OWASP Cheat Sheet Series**:
  - Password Storage, fetched 2026-09-30: argon2id minimum `m=19456, t=2, p=1`, pepper
    guidance; nothing specific about high-entropy tokens.
  - Cross-Site Request Forgery Prevention (synchronizer token, custom request header,
    verifying Origin and `Sec-Fetch-Site`), Session Management (cookie attributes,
    server-side invalidation) and HTML5 Security (no secrets in web storage): cited from
    working knowledge.
- **Rust crate documentation** (docs.rs), fetched 2026-09-30:
  - `argon2` 0.6 (MIT/Apache-2.0; Argon2id v19 default, `t=2, p=1`, PHC hash/verify);
  - `openidconnect` 4.0.1 (MIT: reqwest default feature, custom `AsyncHttpClient`,
    "do not follow redirects", the discovery, PKCE, exchange and ID-token verifier API,
    typestate endpoints);
  - `axum-extra` cookie jars (MIT; not adopted in favor of the `cookie` crate
    directly);
  - `axum` 0.8 `serve::Listener`, implemented for `TcpListener` and `UnixListener`;
  - `tower-http` `CorsLayer::very_permissive`/`permissive`.

  Cited from working knowledge: `cookie` (signed jar), `subtle`, `zeroize`, `hmac`,
  `sha2`, `toml`, `rpassword`, `ipnet`, `jsonwebtoken`, `arc-swap`, clap `env`.
- **Forward-auth header names** (oauth2-proxy, Authelia, Tailscale serve, Cloudflare
  Access): cited from working knowledge of each product's public documentation; verify
  when implementing.
- **Not consulted:** anything from Fluree (source, documentation, policy or design
  documents, website, blog or talks). No Fluree checkout was opened or searched. No
  project planning notes other than the specs listed above were read.

## Outcome

**Delivered.** Phase 1 landed on 2026-09-30 in five steps: the authorization middleware
and route table (`e404c18`), tokens, sessions, OIDC, trusted headers and CLI grants
(`f43dfb3`), the CLI (`45aa952`), the UI (`a9216e5`), and docs and the NixOS module
(`255f4a2`); merged as `acd6da3`. It sits behind the default-on `auth` feature of
`sparkles-server`; without `--auth-config` the auth layer only inserts the local
principal.

**Deviations from the spec.**

- `openidconnect` and `cookie` were not adopted. The relying party (discovery, code flow
  with PKCE, claim checks), HMAC-SHA-256 and cookie signing are Sparkles code over
  `jsonwebtoken` (ID-token signatures only, `aws_lc_rs` backend), `reqwest` and `sha2`.
  Only asymmetric algorithms can be configured (RS, PS, ES and EdDSA families), and the
  ID token's `azp` is checked as well. `ipnet` is a plain dependency.
- `QueryOptions` got `forbid_service`, `forbid_remote_load` and `forbid_file_load`
  (refusals with `Error::NotPermitted`), plus `file_loads` (which files may be read at
  all) and `outbound` (which destinations may be reached), instead of the two
  `allow_*_load` flags of §3.5.
- Beyond §10.2, the auth layer gained limits of its own, documented in
  [API: Authentication](../API.md#authentication-and-access-control):
  - at most 50 sessions per owner (a full store evicts from the owner holding the most);
  - per-owner token quotas (`max_active_per_owner`, `mint_rate`);
  - device logins and unknown user codes limited per client network and per owner;
  - password checks shared fairly between client networks;
  - a 64 KiB body cap on `/$/auth/*`.
- Failed-login limiting (planned for Phase 2) arrived as the `preauth` stage of the
  shared rate limiter, on by default with auth; signed-in callers are rate-limited per
  owner ([API: Rate limiting](../API.md#rate-limiting)). The UI's pages got a hash-based
  Content Security Policy.

**Decided by the maintainer.**

- Open question 8: `serve` listens on `127.0.0.1` by default and refuses a non-loopback
  address without `--auth-config` unless `--allow-open-network` is given.
- Open question 7: `LOAD <file:…>` needs `serve --load-dir` and reads only under it, with
  or without auth.
- `federate` does not open every URL: `SERVICE` and `LOAD <http…>` also follow the
  server's outbound policy (public addresses only unless allowed).

Without `--auth-config` the server also refuses cross-site writes and unknown `Host`
names, and sends CORS headers only for `--cors-origin` origins (see the
[divergences table](../COMPARISON.md#divergences-from-jena--qlever-decisions)); this
replaced the "unchanged" auth-disabled column of §5.4. Anonymous callers get no version or limits from `/$/server`.

**Tests at landing.** The acceptance examples are router tests
(`crates/sparkles-server/src/http/router_tests/auth/`: permissions, tokens, sessions,
OIDC against an in-process mock provider, proxy headers, CLI grants, limits), CLI tests,
and a Playwright test signing in with a local user and an API token. The NixOS VM test
gained an authentication node later (`b149524`).

**Not built.** Native TLS, IdP JWT access tokens on the API and Cloudflare Access JWTs,
sliding sessions and back-channel logout, endpoint-level permissions, and graph-level ACLs
(Phase 3). Device grants stay in memory (open question 9), and the groups recorded for
OIDC and proxy token owners are not refreshed (open question 10).
