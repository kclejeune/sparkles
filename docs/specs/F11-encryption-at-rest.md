# F11: Encryption at rest and encrypted backups

> **Status:** implemented in part
>
> **Phases:** Part of Phase 1 is built: an optional local-key/passphrase encrypted
> repository engine in `sparkles-backup`, local provider lookup and offline key/backup
> CLI commands, and trusted operator TOML server support. Authorized API/UI/Nix
> integration and further operator recovery workflows remain. Phase 2 content-defined chunking and write-only
> repositories, Phase 3 database-directory encryption, and Phase 4 full-text/vector/spatial
> file encryption remain unbuilt.
>
> **User docs:** [Encrypted repositories](../USAGE.md#encrypted-repositories).
> The engine uses `sparkles-backup/encryption`; the facade and CLI/server use
> `backup-encryption`.
>
> This is the design as written before implementation. The [Outcome](#outcome) section
> at the end records the delivered scope and remaining work.

This spec's sources are NIST SP 800-38D, the XChaCha20-Poly1305
draft, the public documentation of AWS KMS, Google Cloud KMS, HashiCorp Vault Transit,
CockroachDB, MongoDB, SQLCipher, OpenZFS and Linux fscrypt, the PostgreSQL wiki page on
transparent data encryption, RocksDB's public `env_encryption.h` header, the design
documents of restic and borg, the age format and the FastCDC paper. §15 lists them. It
builds on backup repositories ([F05](F05-snapshot-repositories.md)), automatic compaction
([C13](C13-automatic-compaction.md)), the full-text, vector and spatial indexes
([F03](F03-full-text-search.md), [F04](F04-vector-search.md),
[G01](G01-geosparql.md)), the codecs of [X01](X01-compression-codecs.md), durable commit
identity ([CI](CI-commit-identity.md)) and retained history
([F06](F06-snapshots-and-point-in-time.md)).

## 1. Summary, goals, non-goals

Sparkles writes every dataset to disk in plain form. Terms sit in `vocab.dat` as
front-coded UTF-8, the delta vocabulary holds every term an update introduced, Tantivy's
term dictionary holds the indexed words, and the spatial index holds the coordinates.
Backups copy those files into a repository, which can be an S3 bucket run by someone
else. Today the protection at rest is whatever the operating system or the storage
service provides, and the README says so.

This spec adds three things:

* **Encrypted backup repositories.** A repository can be initialized with a random
  master key. Blobs and manifests are encrypted and authenticated on the client before
  they are uploaded, and blob ids become keyed hashes, so deduplication keeps working
  inside the repository without revealing content to the storage service.
* **Content-defined chunking for backups.** FastCDC with a secret, per-repository gear
  table splits the files whose content shifts between compactions, which is mainly the
  vocabulary. It is adopted only if a measurement shows that it saves enough.
* **Encryption at rest for database directories.** Every file Sparkles writes for a
  dataset, apart from a few that hold no data, is encrypted with AES-256-GCM under a
  per-dataset data key. The data key is wrapped by a master key that comes from a file,
  an environment variable, a systemd credential, a command, or a key management service
  (AWS KMS, Google Cloud KMS or Vault Transit). Data keys rotate, and compaction
  re-encrypts.

For most deployments the honest default answer is full-disk or filesystem encryption,
such as LUKS2, OpenZFS native encryption or fscrypt. It covers every file, including
logs, swap and temporary files. It keeps memory-mapped reads at full speed, and it is
reviewed far more widely than anything Sparkles can write. §3 compares the options.
Sparkles' encryption at rest covers cases that operating-system encryption does not. A
storage administrator or a volume snapshot can see the disk without the host's keys.
Some deployments need per-dataset keys that can be destroyed to erase one dataset, and
some compliance regimes require the database to take its keys from a KMS. Backups are
different. They leave the host by design, so encrypting
them on the client is the first phase and the one the documentation recommends for any
repository off the host.

**Goals.**

* Confidentiality and integrity of dataset content against someone who holds a copy of
  the disk, a volume snapshot or a backup repository, but not the keys.
* Deduplication and incremental backups keep working in encrypted repositories.
* Nonces never repeat under one key, including after crashes, truncations, copies of a
  database directory and restores of one backup into several datasets.
* Every encrypted unit is authenticated, and a failed check reports the file and the
  unit. Damage never produces wrong query answers.
* Keys rotate without downtime. The master key rewraps data keys in place, and a new data
  key re-encrypts the data through the background compaction of C13.
* The cost is measured and stated. Warm queries at 10.5M quads stay within 10% of an
  unencrypted dataset, or the documentation says they do not and recommends
  operating-system encryption.
* No plaintext is written to disk on the way in or out. That covers spill files, spooled
  request bodies, restores and in-memory backup captures.

**Non-goals.**

* Protection against root on the running host, a debugger attached to the process, or a
  memory dump. Keys and decrypted data live in the server's memory.
* Hiding sizes, file counts, access patterns, commit frequency or which datasets exist.
* Protection against rollback of a whole database directory to an older copy of itself.
  §4.4 states what is and is not detected.
* Encryption in transit. TLS and the reverse proxy cover it.
* Searchable or order-preserving encryption that lets an untrusted server answer queries.
* Encrypting server-wide files outside dataset directories (`config.json`, `auth/`,
  `backup/`) in this spec. Open question 3 covers them.

The main design decisions are these.

| Item | Decision |
|---|---|
| First phase | Encrypted backup repositories, because backups are where the data leaves the host. |
| Cipher | AES-256-GCM from `aws-lc-rs` everywhere. It is FIPS-approved, hardware-accelerated and already in the dependency tree. |
| Nonces | Deterministic counters under keys that are used once. Per-file keys come from random salts, and every append session gets a fresh key (§4.3). |
| Key hierarchy | A master key wraps a per-dataset data key. File and session keys are derived with HKDF. A backup repository has its own master key with several key slots. |
| mmap | Ciphertext stays memory-mapped. Permutation columns are decrypted into the existing block cache, and paged files go through a new cache of decrypted pages (§7.3). |
| Backups of encrypted datasets | Backups carry the logical plaintext of each file, encrypted with the repository key. Backups and at-rest keys stay independent. |
| Blob ids | HMAC-SHA256 of the plaintext under a repository key. Convergent encryption is rejected (§5.2). |
| CDC | FastCDC with a keyed gear table, 1/4/16 MiB, for the vocabulary only, adopted if it saves at least 30% (F05's gate). |
| Default | Operating-system encryption is the recommended default. Sparkles at-rest encryption is opt-in. Encrypted repositories are recommended whenever a repository is off the host. |

## 2. Threat model

### 2.1 What is protected

The assets are the terms and quads of each dataset, its history (the WAL, the commit
catalog and retained generations), the derived indexes, and the metadata files that hold
IRIs, such as `prefixes.json`, the stored queries in `queries.json`, the validation
shapes and the reasoning rules. The keys that protect them are assets too.

### 2.2 Adversaries

| Adversary | Operating-system encryption (LUKS2, fscrypt, ZFS) | Sparkles encryption at rest | Encrypted backup repository |
|---|---|---|---|
| A stolen, discarded or returned disk | Protected. | Protected for dataset files. Logs, swap and the system temporary directory are not covered. | Not involved. |
| A volume snapshot or SAN copy taken by a storage or cloud administrator without the host's keys | Protected when the encryption runs inside the guest. A provider-managed disk key does not protect against the provider. | Protected. | Not involved. |
| The operator of the backup storage, or a leaked bucket | Not protected. Repositories hold plain files. | Not protected. Backups carry logical plaintext (§5.8). | Protected. |
| An unprivileged local user who can read the files | File permissions decide. | Protected without the key, even if permissions are wrong. | Protected. |
| Someone who can modify the disk while the host is down | XTS modes detect nothing. ZFS with GCM detects changed blocks. | Changed units are detected. Wholesale rollback is not (§4.4). | Changed blobs and manifests are detected. |
| Root on the running host, or a memory dump | Not protected. | Not protected. | The repository key is in memory too. Write-only repositories (§6.5) keep old backups unreadable to a compromised writer. |

### 2.3 Rollback of a backup repository

Someone with write access to the bucket cannot read or forge encrypted objects, but
they can delete objects and put back older copies. Two kinds of rollback follow from
that, and Sparkles treats them differently.

**Manifests can be replayed.** An attacker can delete a backup and publish an older
envelope under the same name, or restore a backup that retention deleted. The envelope
still authenticates, because its associated data binds the repository, the epoch and
the name but not a version. Detecting this needs a monotonic counter or index outside
the bucket. Restoring the wrong generation of a backup is outside the threat model, and
operators who need that assurance should compare backup times and ids with their own
records.

**Key epoch metadata is protected.** The marker's descriptor decides which epoch seals
new backups, so rolling it back after a master-key rotation would make new backups use
a key that may have leaked. Three checks prevent that. The descriptor carries an HMAC
under a subkey of the active epoch's master key (§5.4). The active epoch must be the
highest listed epoch, so a retired epoch can never become active again. Each host also
records the highest epoch it has accepted for each repository id, and refuses a
descriptor whose highest epoch is lower. The record also notes whether the host has
seen an authenticated descriptor, after which a descriptor without a tag is refused.
The record is kept in memory and in the repository's cache directory as
`.key-epoch.json`. It is the check that stops an attacker who holds the leaked old
master key, because that key can sign a descriptor that lists only its own epoch. A
host that has never opened the repository, or whose cache directory was cleared, has
no record and relies on the tag and the epoch ordering alone.

### 2.4 What still leaks

Encryption at rest hides content, not shape. An observer of the disk still sees file
names (`gen-0004/spo.dat`), file sizes and how they grow, the number of generations,
when commits happen and roughly how large they are, the number of datasets and the
plaintext files of §7.1. An observer of a repository sees blob sizes, which blobs two
backups share, backup names and times. §6.3 reduces what blob sizes reveal. The user
documentation lists these leaks.

## 3. Operating-system encryption first

| | LUKS2 / dm-crypt | OpenZFS native encryption | fscrypt | Sparkles at rest (Phases 3–4) |
|---|---|---|---|---|
| Unit | Disk sector, AES-XTS by default | Record, AES-256-GCM by default | File contents, AES-256-XTS | Column, page or log frame, AES-256-GCM |
| Integrity | None, unless dm-integrity is added | Yes, per record | None | Yes, per unit |
| Covers logs, swap, `/tmp` | Yes, when they are on the volume | Yes, on encrypted datasets | Only encrypted directories | No. §7.5 gives mitigations. |
| mmap and page cache | Unchanged. The page cache holds plaintext. | Unchanged for readers | Unchanged | Ciphertext is mapped. Plaintext lives in Sparkles' caches. |
| Warm query cost | None | None | None | Estimated 0–10% (§10) |
| Per-dataset keys and crypto-erasure | No | Per ZFS dataset | Per directory | Yes |
| Keys from a KMS | Through external tooling (Clevis, TPM2) | Key files or prompts | Kernel keyring | Built in |
| Who can read the files on the running host | Anyone with file access | Anyone with file access | Anyone with file access once the key is added | Only the Sparkles process, which holds the keys |

The documentation's recommendation follows from this table. Use LUKS2 or ZFS native
encryption for the data directory, swap and the temporary directory. Enable Sparkles
encryption at rest when a threat in §2.2 needs it, typically untrusted storage
administration or a requirement for KMS-held, per-dataset keys. Encrypt every backup
repository that is not on the same encrypted host. Several established databases took
the same position. CockroachDB's encryption at rest uses AES in counter mode with an
estimated 5–10% CPU cost and no authentication. The PostgreSQL TDE discussions record
that integrity remains unsolved and that the motivation is largely regulatory.

## 4. Cryptographic design

### 4.1 Primitives

* **AES-256-GCM** (NIST SP 800-38D) with 96-bit nonces and 128-bit tags, from `aws-lc-rs`.
  It runs at about 5.8 GB/s per core on 16 KiB units on the development laptop (§10).
  `aws-lc-rs` is already in the tree through `rustls`, and its FIPS build is available to
  deployments that need a validated module (open question 4).
* **HKDF-SHA256** (RFC 5869) derives every subkey. Each derivation names its purpose in
  the `info` field, so no key serves two purposes.
* **HMAC-SHA256** (RFC 2104) computes keyed blob ids and key fingerprints.
* **Argon2id** (RFC 9106) protects passphrase key slots, through the `argon2` crate the
  server already uses for password hashes. The default parameters are RFC 9106's second
  recommended option: 64 MiB of memory, 3 passes and 4 lanes.
* **The operating system's random generator** (`getrandom`) supplies every key, salt and
  session id.

XChaCha20-Poly1305 was the main alternative. Its 192-bit random nonces remove the need
for counters, but it is not FIPS-approved and measured about 5.4 times slower than
AES-256-GCM on the development laptop. AES-GCM-SIV (RFC 8452) tolerates nonce reuse but
needs two passes over the data and is not in NIST's approved list. §13 records both.

### 4.2 Key hierarchy

```
master key (KEK)                     file, env, systemd credential, command, AWS KMS, GCP KMS, Vault Transit
  └─ wraps → dataset data key (DEK) v1, v2, …        random 256-bit, stored wrapped in <db>/keys.json
       └─ HKDF → file key                            per file, from a random 32-byte salt in the file header
            └─ HKDF → session key                    per append session of a log file (§4.3)

repository master key (RMK)          random 256-bit, per backup repository, one per key epoch
  ├─ wrapped by each key slot → keys/<id>.json       key file, passphrase, command, KMS, age identity (Phase 2)
  └─ HKDF → id key, manifest key, blob key, chunker key
```

**Master keys** are never stored by Sparkles. They come from the key sources of §8.1.
With a KMS, the master key never leaves the service. Sparkles sends the wrapped data key
to the KMS's decrypt operation when it opens the dataset, as envelope encryption in the
AWS and Google Cloud documentation describes. A dataset can carry wraps under several
master keys at once. That gives escrow, for example a KMS key for daily use and an
offline recovery key in a safe.

**Data keys** are per dataset. A data key encrypts only that dataset's files, so
destroying all its versions erases the dataset cryptographically, even where the
filesystem leaves deleted blocks behind. Each version has a state. The `active` version
encrypts new files, `retired` versions only decrypt, and `destroyed` versions are gone.
Every file header names the version it was written with.

**File keys** are `HKDF(DEK_v, salt = file salt, info = "sparkles/f07/file" ‖ keyring
id ‖ file kind)`. The salt is 32 random bytes in the file's header, drawn when the file
is created. Two files never share a key, even if they hold the same content.

`<db>/keys.json` is plaintext JSON and holds only wrapped keys:

```json
{ "format": 1, "kind": "sparkles-keyring",
  "keyringId": "5b9e1c0a-…", "cipher": "aes-256-gcm",
  "deks": [
    { "version": 1, "state": "retired", "created": "2026-10-02T12:00:00.000Z",
      "wraps": [ { "kek": "main", "kekId": "file:9f2c41d07a1e", "nonce": "…", "ct": "…" } ] },
    { "version": 2, "state": "active", "created": "2027-01-02T12:00:00.000Z",
      "wraps": [ { "kek": "main", "kekId": "file:9f2c41d07a1e", "nonce": "…", "ct": "…" },
                 { "kek": "kms", "kekId": "aws-kms:arn:aws:kms:eu-central-1:111122223333:key/…", "ct": "…" } ] } ],
  "rotateAfter": null }
```

* `keyringId` is random and independent of the dataset id. The identity rule of a F05
  restore (`reidentify`) therefore never touches encrypted data.
* A wrap under a local key is AES-256-GCM with a random nonce and `keyringId ‖ version`
  as associated data. A KMS wrap passes the same pair as the encryption context or
  associated data, where the service supports it.
* `kekId` identifies the master key without revealing it. For a local key it is the first
  six bytes of `HMAC-SHA256(KEK, "sparkles/f07/kek-id")` in hex. A wrong key file then
  gives "the master key does not match keyring 5b9e…: expected kek 9f2c41d07a1e, got
  03ab…" instead of an authentication failure.
* The file is written with `write_atomic` and the directory is synced. A new data key
  version is durable in `keys.json` before any file is written under it.

### 4.3 Nonces

GCM loses both confidentiality and integrity when a nonce repeats under one key. NIST SP
800-38D allows deterministic nonces (a fixed field plus an invocation counter) and
random nonces, and limits random nonces to 2^32 invocations per key. Sparkles uses
deterministic nonces only, and makes every counter run under a key that is never used
for anything else. Four rules achieve this:

1. **Write-once files** get a fresh file key from a fresh salt every time a file is
   created, including a retry after a failed build. The nonce of a unit is `0x00000000 ‖
   u64 unit index`. A file is never modified after it is complete, so each index is
   sealed once.
2. **Append-only files** are written in sessions. Each time a writer opens the file for
   appending, it draws a 16-byte random session salt, writes a session record, and seals
   frames under `HKDF(file key, info = "sparkles/f07/session" ‖ session salt)` with
   nonces counting from 0. Any truncation ends the session, whether it removes a torn
   tail at open or rolls back the delta vocabulary. The next append starts a new
   session. A crash can therefore never lead to a second, different frame under the same
   key and counter. Two random 128-bit salts collide with negligible probability, about
   2^-65 after 2^32 sessions, so copies of a directory, clones and repeated restores of
   one backup are safe too.
3. **Fixed-stride records** in `commits.bin` each carry a 16-byte random salt. The record
   is sealed under `HKDF(file key, info = "sparkles/f07/record" ‖ salt)` with a zero
   nonce, so a record rewritten at the same position after a crash gets a new key. The
   catalog keeps its O(1) lookup by position, which sessions would break.
4. **Ephemeral files** (spills, spooled bodies, temporary generations) use a key drawn at
   random when the file is created and held only in memory. Nothing that survives a
   restart can decrypt them, which is the intent.

Counters never wrap. A session or file that would exceed 2^48 units refuses to grow and
starts a new session or file, which never happens in practice.

### 4.4 Integrity

Every sealed unit carries a GCM tag. The associated data of a unit is the file's 64-byte
header and the file's path relative to the database directory, in the form it has once
complete (`gen-0004/spo.dat`). The nonce carries the unit index. Together they detect:

* any changed byte of a unit, its header or its length field;
* a unit moved to another position, or a page or frame swapped between files;
* a file renamed to another path or kind, or copied from another dataset, whose keyring
  and therefore keys differ.

**Generation seals.** The sealed `meta.json` of an encrypted generation lists every file
of the generation with its salt and logical length. Opening the generation checks each
file's header against the list. A valid file of the same path and keyring from another
build is then detected, for example one left by an interrupted build that used the same
generation number.

**What is not detected.** An attacker with write access can replace the whole database
directory, or a whole generation together with its `meta.json` and `CURRENT`, with an
older copy that was valid at the time. They can also cut a log file back to an earlier
frame boundary, which looks like a crash. Detecting this needs a monotonic counter
outside the disk, such as a TPM counter or a KMS-held value. Open question 9 covers it.
The documentation states this limit.

**Errors.** A unit that fails authentication is reported as `Error::Corrupt` with the
file, the unit and the data key version, for example `authentication failed:
gen-0004/spo.dat block 812 column 2 (dek v2)`. A query that touches it fails with
`500 corrupt`. Units that pass keep serving, and `sparkles check` reports every bad unit.

### 4.5 Keys in memory

Unwrapped keys live in `zeroize`-on-drop buffers. Their pages are locked with `mlock` and
excluded from core dumps with `madvise(MADV_DONTDUMP)` on Linux. Short-lived copies are
not fully covered. Moving a buffer in Rust copies its bytes and leaves the old stack
slot unzeroized, and key material read from a file, a command or the environment passes
through ordinary heap buffers before it reaches a locked page. Those buffers are
zeroized when dropped, but the guarantee is best effort rather than complete. Decrypted data in caches
is ordinary heap memory and can reach swap, so §7.5 recommends encrypted swap. Keys
never appear in logs, error messages, metrics, API responses or `Debug` output. The
`Debug` implementation of a key prints its fingerprint.

## 5. Encrypted backup repositories (Phase 1)

This section is the client-side encryption of F05 Phase 3. It replaces the sketch in F05
§7 and answers F05's open question 11. Everything F05 specifies about capture, planning,
locks, leases, restore modes and the identity rule still applies.

### 5.1 Repository master key and key slots

`repo add --encrypt` (or `"encryption": {…}` at registration) draws a 256-bit repository
master key and writes it, wrapped, into one key slot. A slot is an object
`keys/<uuid>.json`:

```json
{ "kind": "sparkles-key-slot", "format": 2, "id": "c1d2…", "epoch": 1,
  "label": "ops key file", "created": "2026-10-02T12:00:00.000Z",
  "source": "file", "kekId": "file:9f2c41d07a1e",
  "nonce": "…", "ct": "…" }
```

| Slot source | How the master key is unwrapped | Phase |
|---|---|---|
| `file`, `env`, `credential` | AES-256-GCM under a 32-byte key read from a file, an environment variable or a systemd credential | 1 |
| `passphrase` | AES-256-GCM under Argon2id(passphrase, salt). The slot stores the salt and parameters. | 1 |
| `command` | AES-256-GCM under 32 bytes that a configured command prints | 1 |
| `vault-transit` | Vault Transit `decrypt` of the stored ciphertext, with the repository id as context | 1 |
| `aws-kms`, `gcp-kms` | The service's `Decrypt` of the stored ciphertext, with the repository id as encryption context or associated data | 1, behind cargo features |
| `age` | An age X25519 stanza, opened with an age identity file | 2 |

The associated data of every local wrap starts with `repository id ‖ epoch ‖ slot id`. A
slot copied into another repository does not open there. Format 2 slots also bind their
metadata: the label, the creation time, the source, the `kekId` and the Argon2
parameters follow as length-prefixed fields. A slot whose metadata was edited fails to
open. Format 1 slots, written before this change, bind only the first three fields and
stay readable. Their metadata can be relabeled by anyone with bucket access. Removing
such a slot and adding the key again writes a format 2 slot.

A passphrase label wraps at most one slot per epoch. Opening refuses a repository with
two passphrase slots of the same label in one epoch before it runs any Argon2
derivation, so planted slots cannot multiply the cost of an unlock.

Opening a repository tries the configured key against the slots, and keeps the master
key of every epoch it opens in memory for the repository's lifetime. Each operation
reads the marker. When the descriptor is unchanged, the operation reuses the unlocked
keys without listing slots, running Argon2 or locking new pages. A changed descriptor is
verified again in full, and keys of epochs that were already unlocked are reused. `repo key add`
creates a slot and needs an open slot first. `repo key remove` refuses to remove the
last slot of an epoch that still holds backups (`409 last-key-slot`). `repo key list`
shows each slot's id, label, source, `kekId` and creation time, never key material.

The subkeys are derived with HKDF from the master key of the epoch:

| Subkey | `info` | Use |
|---|---|---|
| id key | `sparkles/f07/repo/id` | Blob ids (§5.2) |
| blob key | `sparkles/f07/repo/blob` | Parent of per-blob keys (§5.3) |
| manifest key | `sparkles/f07/repo/manifest` | Manifests (§5.4) |
| chunker key | `sparkles/f07/repo/chunker` | The FastCDC gear table (§6.3) |

### 5.2 Keyed blob ids and deduplication

F05 names a blob by the SHA-256 of its plaintext. In an encrypted repository the name
would let anyone test whether the repository holds a known file or piece, such as a
public vocabulary dump. Encrypted repositories therefore name a blob by
`HMAC-SHA256(id key, plaintext)`. The consequences are:

* Deduplication within the repository is unchanged. Every writer that holds the key
  computes the same id, so parent reuse, the `HEAD` check and the 412-as-success rule of
  F05 §5.4 work as before.
* Two repositories with different keys share no ids, so their contents cannot be linked.
* Equality inside one repository stays visible. Whoever sees the bucket learns which
  backups share which blobs. Deduplication needs this, and restic and borg accept the
  same leak with keyed ids.

**Convergent encryption is rejected.** Deriving the encryption key from the content would
let different keys and different repositories deduplicate against each other. It also
lets anyone holding a candidate plaintext confirm its presence, and lets them guess
low-entropy fields such as a single changed literal by brute force. A Sparkles repository
is written by few writers who share a key, so cross-key deduplication buys nothing that
is worth that risk.

### 5.3 Blob format

The F05 blob header keeps its first eight bytes. The encryption byte becomes `1`, the
plaintext length moves inside the sealed payload, and the payload is sealed:

| Bytes | Field |
|---|---|
| 0..4 | magic `SPKB` |
| 4 | format `1` |
| 5 | codec `0`. The real codec is inside the sealed payload. |
| 6 | encryption `1`: AES-256-GCM, key epoch scheme 1 |
| 7 | zero |
| 8..12 | key epoch (u32 LE) |
| 12..16 | zero |
| 16..32 | blob salt, 16 random bytes |
| 32..n−16 | ciphertext of `codec (1 byte) ‖ plaintext length (u64) ‖ payload ‖ padding` |
| n−16..n | GCM tag |

The key of one blob is `HKDF(blob key, salt = blob salt, info =
"sparkles/f07/repo/blob")`, and the nonce is zero. The associated data is the 32 header
bytes, the repository id and the blob id. A reader checks the tag, decodes, then checks
the length and recomputes the keyed id. Moving a blob object to another id's path fails
the tag check before any byte is used. Two writers that upload the same id race
harmlessly. Whichever `PUT` wins, its object decrypts to the same plaintext.

Padding is zero bytes after the payload, up to the size PADMÉ gives for the payload
length (§6.3). The plaintext length field lets the reader drop it.

### 5.4 Manifests and the marker

The marker's `encryption` field, which F05 builds already refuse when it is not `null`,
names the scheme:

```json
"encryption": { "scheme": "sparkles-repo-v1", "cipher": "aes-256-gcm", "ids": "hmac-sha256",
                "kdf": "hkdf-sha256", "epochs": [ { "epoch": 1, "state": "active", "created": "…" } ],
                "padding": "padme", "writeOnly": false, "mac": "9a41…" }
```

`mac` is the hex HMAC-SHA256 of `"sparkles/f07/repo/descriptor" ‖ 0x00 ‖ repository id ‖
descriptor JSON without mac`, under `HKDF(master key of the active epoch, info =
"sparkles/f07/repo/descriptor")`. A reader verifies it once the active epoch is
unlocked. The active epoch must also be the highest listed epoch. §2.3 explains why
these checks are needed and what the per-host epoch record adds. Descriptors written
before the field existed have no `mac`. They still open, and the next `repo key
rotate-master` or `repo key retire` writes a tagged descriptor. Builds that predate the
field refuse a marker that has one, and also refuse format 2 key slots.

A manifest in an encrypted repository is an envelope:

```json
{ "format": 1, "kind": "sparkles-backup-sealed", "name": "b2",
  "repositoryId": "7d0e5c1a-…", "epoch": 1, "salt": "…", "ct": "…" }
```

`ct` is the F05 manifest JSON, sealed with a key derived from the manifest key and the
salt. The associated data is the repository id, the epoch and the name. A manifest
renamed to another name, or copied from another repository, fails to open. The name
remains visible in the object key, so the documentation advises names that do not reveal
anything sensitive. A policy's name template can use `{run}` instead of the dataset name.

The F05 manifest cache stores the envelopes as fetched, never the decrypted JSON. The
server decrypts them in memory.

### 5.5 Create, restore, verify and GC

* **Create** hashes each piece with the id key instead of plain SHA-256, encrypts it while
  it uploads, and seals the manifest. Hashing costs about the same as before, because
  HMAC-SHA256 is one SHA-256 pass over the data plus a few extra blocks.
* **Restore** decrypts and authenticates each blob, checks its keyed id, and verifies each
  file's `sha256` from the manifest as before.
* **Verify** at the `exists` level needs the key only to open manifests. At the `data`
  level it decrypts every blob. A blob that fails its tag is reported in `corrupt`.
* **GC** needs the key to read manifests during the mark. Orphans are counted and swept by
  id, as in F05.

Every operation on an encrypted repository needs an open key slot. Without one, the API
answers `409 repository-key-required` and the CLI exits 1 with the same message. With a
key that matches no slot, the answer is `422 wrong-repository-key`, and the message lists
the `kekId`s the slots expect. A repository is encrypted or not from its initialization
on. Converting means copying it with the Phase 3 `repo copy` of F05.

### 5.6 Master key rotation

`repo key rotate-master` starts a new epoch with a new master key and new slots. New
backups use the new epoch. The new id key gives new ids, so the first backup of each
dataset in the new epoch uploads everything again. Old backups remain readable for as
long as the old epoch keeps a slot. Once retention and GC have removed every blob and
manifest of the old epoch, `repo key retire --epoch 1` deletes its slots and marks it
`retired` in the marker. Rotating the master key is the response to a suspected leak. A
routine change of who can open the repository only needs `repo key add` and
`repo key remove`.

### 5.7 Key loss

Without a slot that opens, the repository cannot be read, by design. Sparkles cannot
recover it. The mitigations are part of the feature:

* `repo add --encrypt` refuses to finish with a single slot unless `--single-key-ok` is
  given. The second slot is meant to live somewhere other than the server, such as a
  passphrase in a password manager or a KMS key.
* `repo key export --paper` prints the master key of the current epoch as an armored
  block with a checksum, for printing and offline storage. It is audited.
* The Repositories tab warns when a repository has a single slot, and `repo test`
  checks that every configured slot opens.

### 5.8 Backups of encrypted datasets

A backup of a dataset that is encrypted at rest carries the **logical plaintext** of each
file. The capture of F05 §5.3 reads through the decrypting reader of §7.2, so the
repository receives the same bytes it would receive from an unencrypted dataset with the
same content. Three things follow:

* At-rest keys and repository keys are independent. A restore needs only the repository
  key, and losing a dataset's master key does not lose its backups.
* Deduplication between an encrypted dataset and its unencrypted copy, or across a data
  key rotation, still works.
* Content-defined chunking (§6) sees plaintext, which is the only place it can find
  shifted content.

To keep the plaintext from leaving the host unprotected, a backup of an encrypted dataset
to an unencrypted repository fails with `409 unencrypted-repository`. A repository
setting `allowPlaintext: true` overrides it, and the override is audited. A restore of
such a backup into a server or directory without encryption at rest fails with
`409 plaintext-target` unless the request sets `allowPlaintext: true`. The manifest
records `"source": {"encryptedAtRest": true}` for both checks.

F05 promises that a restored directory is byte-identical to the source. For encrypted
datasets the promise becomes logical identity. Every file's plaintext is identical, and
the sealed bytes differ because restores draw new salts and sessions.

## 6. Content-defined chunking and write-only repositories (Phase 2)

### 6.1 Where chunking helps

F05 splits files at fixed 32 MiB offsets. Between compactions that is exact, because
generation files never change and the WAL grows by appended segments. A compaction
changes the picture:

* The permutation files are renumbered completely. Ids change, so chunking finds nothing
  to share in them.
* `vocab.dat` stores the sorted terms front-coded. Terms added since the last compaction
  are inserted at their sorted positions, so every fixed piece after the first insertion
  shifts and is uploaded again, although most of its bytes are unchanged. Chunking
  resynchronizes after each insertion.
* `vocab.off` holds absolute offsets that all change. It is small, about 0.5 bytes per
  term.

Chunking therefore applies to `vocab.dat` only, unless the measurement of §6.4 finds
another file that benefits.

### 6.2 FastCDC parameters

The chunker is FastCDC as published by Xia et al. at USENIX ATC 2016. It uses the Gear
rolling hash, skips the cut-point test below the minimum size, and uses normalized
chunking (NC-2) to narrow the size distribution. The paper reports it about 10 times
faster than the best open-source Rabin-based chunker and 3 times faster than other
Gear-based chunkers, with nearly the same deduplication ratio as Rabin-based chunking.
Sparkles implements it from the paper, because it needs its own gear table (§6.3).

| Parameter | Value | Reason |
|---|---|---|
| Minimum | 1 MiB | Keeps the object count low. 300 MB of vocabulary gives about 75 objects. |
| Average | 4 MiB | One `PUT` per chunk without multipart. Much larger than restic's 1 MiB, because Sparkles has few, large files. |
| Maximum | 16 MiB | Below F05's 32 MiB piece limit, so manifest validation is unchanged. |
| Normalization | NC-2 | The paper's recommended level |

The marker records the chunker and its parameters per file pattern:
`"chunking": {"vocab.dat": {"algo": "fastcdc-2016", "min": 1048576, "avg": 4194304,
"max": 16777216, "nc": 2}}`. Every writer of the repository uses the same parameters,
or the chunks would not match.

### 6.3 Chunk sizes as a side channel

Chunk boundaries depend on content. With a public gear table, the sequence of chunk
sizes fingerprints a file, and an observer could recognize a known vocabulary from blob
sizes alone. borg makes its chunker seed a per-repository secret for this reason, and
restic chooses its Rabin polynomial at random per repository.

* In encrypted repositories the 256-entry gear table is derived from the chunker key
  (HKDF output expanded to 256 × 64 bits). Boundaries then depend on the key.
* In unencrypted repositories the table is derived from the repository id, which is
  public. Nothing is secret there anyway.
* Encrypted blobs are padded with PADMÉ (Nikitin et al., PETS 2019). It rounds a length
  L up so that only O(log log L) bits of it remain. The overhead is at most 12% and about
  3% at chunk sizes of a few MiB. `padding: "none"` turns it off per repository.

### 6.4 The gate

F05 adopted chunking only if it saves more than 30%. The measurement is part of Phase 2:

1. Load 10.5M quads (`scripts/gen-data.py 1000000`) and back up with fixed pieces.
2. Insert 1% new terms, spread uniformly over the sorted key space, then compact and back
   up again. Record the bytes of `vocab.dat` uploaded.
3. Repeat with FastCDC at the parameters of §6.2, and at 512 KiB / 2 MiB / 8 MiB.
4. Repeat steps 2 and 3 with 10% new terms, and with all `.dat` and `vocab.off` files
   chunked, to confirm that they gain nothing.

Chunking is enabled for `vocab.dat` in new repositories if the second backup uploads at
least 30% fewer bytes of `vocab.dat` with chunking than without. Otherwise the code stays
behind a repository option and the default does not change. Existing repositories keep
their marker's settings.

### 6.5 Write-only repositories

A write-only repository lets a server create backups it cannot read. Ransomware or an
intruder on that server cannot then read old backups, and a stolen server key does not
expose the history. This mode uses age X25519 recipients:

* The marker lists the recipients' public keys. The private identities stay off the
  server, for example on an operator's machine or a hardware token.
* Each backup draws a random data key and wraps it to every recipient as an age stanza
  in the manifest's key table. Its new blobs are sealed under that key, and the blob
  header names the key by an 8-byte id. A manifest lists the wrapped data key of every
  blob it references, including blobs reused from earlier backups.
* The writer holds a **writer key** instead of the master key. It derives the id key, the
  manifest key and the chunker key, so deduplication, manifests, listing, retention and
  GC work as in §5.5. Manifests stay readable to the writer. They reveal file names,
  sizes and commit metadata, which the writer produced in the first place.
* Restore and verification at the `data` level need an identity. A writer without one
  answers `403 read-capability-required`. Verification at the `exists` level works.

The writer cannot detect a lost identity. The documentation therefore asks for a
periodic `backup verify --level restore` on the host that holds an identity.

## 7. Encryption at rest (Phases 3 and 4)

### 7.1 File inventory

The table lists every file of a dataset directory and how it is treated. "Sealed" means
one of the formats of §7.2.

| File | Today | Holds | Treatment | Phase |
|---|---|---|---|---|
| `gen-N/<perm>.dat` | Written once, memory-mapped | LZ4 columns of ids | Each column sealed as one unit. `col_len` in the metadata includes the 16-byte tag. | 3 |
| `gen-N/<perm>.meta` | Written once, read whole at open | Block bounds as ids, offsets | Paged, read whole at open | 3 |
| `gen-N/vocab.dat` | Written once, memory-mapped | Every base term, front-coded | Paged, 16 KiB pages, through the decrypted-page cache | 3 |
| `gen-N/vocab.off` | Written once, memory-mapped | Offsets of the front-coded blocks | Paged, decrypted whole at open | 3 |
| `gen-N/wal.log` | Appended, synced per commit | Ids of changed quads, commit records | Sealed log, one frame per commit | 3 |
| `gen-N/delta.vocab` | Appended | Every term an update introduced | Sealed log, one frame per flush | 3 |
| `commits.bin` | Appended, fixed 64-byte records | Commit metadata | Sealed records of 96 bytes, so commit `s` stays at a computed offset | 3 |
| `annotations.bin` | Appended | Commit messages, digests | Sealed log, one frame per record | 3 |
| `gen-N/meta.json`, `stats.json`, `commit.json` | Written once | Counts, ids, the prefixes at build | Small sealed. `meta.json` also carries the generation seal (§4.4). | 3 |
| `prefixes.json`, `queries.json`, `history.json`, `text.json`, `geo.json`, `reasoning.json`, `validation.json`, `validation-shapes.ttl`, `validation-schema.json`, `rdfs.json`, `rdfs-schema.nt`, `origin.json`, `restore.json`, `quota.json`, `compaction.json` | `write_atomic` | IRIs, query text, shapes, rules, configuration | Small sealed | 3 |
| `gen-N/geo/*.spkg` | Written once, memory-mapped in place | Coordinates, geometries | Phase 3: not written. The spatial index is built in memory at open. Phase 4: paged, decrypted into memory at open. | 3, 4 |
| `gen-N/vectors/*.spkv` | Written once, memory-mapped in place | Vectors, HNSW graph | The same as the spatial index | 3, 4 |
| `text/` (Tantivy) | Segment files written once, `meta.json` and `.managed.json` by atomic write | Words, stored literals | Phase 3: a full-text index cannot be enabled. Phase 4: an encrypting Tantivy directory (§7.3). | 4 |
| `tmp/` of the builder (`b*.voc`, `b*.q`, `b*.map`, `run-*`), `*.tmp` | Temporary | Terms and ids | Ephemeral sealed streams (§7.4) | 3 |
| `keys.json` | New | Wrapped data keys | Plaintext, by necessity | 3 |
| `CURRENT` | `write_atomic` | The current generation's name | Plaintext. It holds no data, and opening needs it first. | 3 |
| `dataset.json` | `write_atomic` | Dataset id, origin, `forkedFrom` | Plaintext. It holds no data, and tools read it without a key. | 3 |
| `sparkles.lock`, `text/.tantivy-*.lock` | Lock files | A pid or nothing | Plaintext | 3 |

Server files outside dataset directories are listed in §7.4 and §7.5.

### 7.2 Sealed file formats

Every sealed file starts with the same 64-byte header, which is the associated data of
all its units together with the file's path:

| Bytes | Field |
|---|---|
| 0..4 | magic `SPKE` |
| 4 | format `1` |
| 5 | container: `1` units, `2` paged, `3` log, `4` small, `5` ephemeral stream, `6` records |
| 6 | cipher: `1` AES-256-GCM |
| 7 | file kind (permutation data, permutation metadata, vocabulary, offsets, WAL, delta vocabulary, catalog, annotations, JSON, text, vector, spatial, spill) |
| 8..12 | data key version (u32 LE), or `0` for an ephemeral key |
| 12..16 | page size in bytes (paged), otherwise zero |
| 16..48 | file salt |
| 48..64 | keyring id |

During the conversion of §7.7, a dataset holds sealed and plaintext files side by side,
and the reader tells them apart without a list. A generation is sealed when its
`meta.json` is sealed, and then every file in it must be sealed. A plaintext file inside
a sealed generation is `Corrupt`. A file in the dataset's root directory is sealed when
it starts with `SPKE` and its first unit authenticates. The root files' plaintext forms
start with their own magic (`SPKCMTS` for `commits.bin`, `SPKANNO` for
`annotations.bin`), with `{` for JSON, or with Turtle or N-Triples text. A plaintext file
that happens to start with `SPKE` fails authentication and is read as plaintext only
while a conversion is in progress. Otherwise it is `Corrupt`.

**Units** (container 1) hold the permutation data. Each column of each block is
`ciphertext ‖ tag`, sealed with nonce counter `block × 4 + column`. The block metadata
already records each column's offset and length, so a reader decrypts exactly the
columns it decodes. An empty column stays empty.

**Paged** files (container 2) split the logical content into pages of a fixed plaintext
size, 16 KiB by default, each sealed with its page number as the counter. A sealed
trailer unit with the counter `u64::MAX` holds the logical length and the number of
pages. A file without a valid trailer is incomplete and refused. A read of a logical
range decrypts the pages it overlaps.

**Logs** (container 3) are sequences of records after the header:

| Record | Layout |
|---|---|
| Session | `0x53`, 16-byte session salt, CRC-32 of the record |
| Frame | `0x46`, u32 LE ciphertext length n, n bytes of ciphertext, 16-byte tag |

A frame's associated data adds the session salt and the length to the header and path.
The logical content is the concatenation of frame plaintexts. Readers use physical
offsets, which F06's `WalIndex` already stores as the byte offset after each commit.
They now point after a commit's frame. At open, a scan stops at the first record that is
incomplete or fails authentication. The WAL, the delta vocabulary and the annotations
are synced before each commit is acknowledged, so in them a bad record can only be a
torn tail if nothing valid follows it. Such a tail is truncated, as today, and a bad
record followed by a valid one fails the open with `Corrupt`.

**Small** files (container 4) are one sealed unit of the whole file, written to a
temporary name and renamed, as `write_atomic` does today.

**Records** (container 6) hold the commit catalog. The plaintext header of
[CI §5.5](CI-commit-identity.md) becomes the first sealed record, and each 64-byte
catalog record becomes `salt (16) ‖ ciphertext (64) ‖ tag (16)`. Commit `s` is at offset
`64 + 96 · (s − first_seq + 1)`, so lookups and paging stay O(1). `commits.bin` is not
synced per commit, because [CI](CI-commit-identity.md) rejected that, so a crash can
leave its unsynced tail damaged in any order. The open truncates it at the first record
that fails authentication, and the existing catalog recovery of CI restores the missing
records from the WAL.

**Ephemeral streams** (container 5) are sequences of 64 KiB frames under an ephemeral
key, read and written sequentially.

### 7.3 Reading without plain mmap

Encryption breaks the assumption behind every memory-mapped read in Sparkles, that the
bytes on disk are the bytes the code wants. Sparkles keeps mapping the files, because
the kernel's page cache still helps with ciphertext, and adds decryption at the points where data
is decoded anyway.

**Permutations.** A scan already decodes each column it reads (varint deltas, then LZ4)
into the block cache, which holds decoded columns keyed by permutation, block and
column. Decryption becomes the first step of that decode. A cache hit costs nothing
extra. A miss adds about 10 µs for a 64 KiB compressed column at the measured AES-GCM
rate, on top of decompression and varint decoding, which the code does today. The block
cache keeps its size and default of 1 GiB.

**Vocabulary.** `Vocab` reads terms straight from the mapped `vocab.dat` today, with no
cache. Encrypted, it reads pages through a new **decrypted-page cache**, keyed
by file and page and weighted by bytes. A lookup that hits the cache pays one hash
lookup, about 50 ns. `get_sorted` touches each front-coded block once, and therefore
each page about once per batch of ids. A front-coded block that straddles two pages is
copied out of both. `vocab.off` is decrypted whole at open, about 0.5 bytes per term or
50 MB at 100M terms, because the binary search of `find` probes it at random. The
search's probes into `vocab.dat` go through the same cache, where the upper levels of the
search stay resident.

**Full-text index (Phase 4).** An encrypting implementation of Tantivy's `Directory`
replaces the mapped directory that `text/lazydir.rs` wraps today. Segment files are
written as paged files through a `TerminatingWrite`, and `meta.json` and
`.managed.json` as small sealed files through `atomic_write`. A `FileHandle` serves
`read_bytes(range)` from the decrypted-page cache and returns owned bytes. Ranges inside one page
share the decrypted page without a copy. Tantivy reads its term dictionary and posting
lists in small random ranges, so cold searches pay one page decryption per range.

**Vector and spatial indexes.** Both map their files and use the sections in place as
typed slices. Paging would put a cache lookup in front of every vector the HNSW search
visits. Phase 3 therefore does not write these files on encrypted datasets and builds
the indexes in memory when the dataset opens. This costs open time, the same as the
first build after a configuration change. Phase 4 writes them as paged files and
decrypts them whole into aligned memory at open.

**The memory tradeoff.** The page cache of the operating system now holds ciphertext,
and Sparkles' caches hold plaintext. Pages that a query uses can be in memory twice,
once in each. The decrypted-page cache has its own budget, `--crypt-cache-mb` (default 256).
To get warm queries as fast as an unencrypted dataset, it should hold the hot part of
the vocabulary and of the full-text index. That raises the process's memory by up to
that size, while the kernel can still evict the ciphertext copies. Vector and spatial
indexes in Phase 4 move from evictable mapped pages to process memory, so their whole
size counts against the process. For a 10M-vector, 768-dimension index that is about
30 GB. The documentation recommends operating-system encryption for large vector
indexes, and `sparkles stats` reports the memory each encrypted index holds.

### 7.4 Temporary files, spooled bodies and exports

* **Builder spills** in `gen-N/tmp/` hold terms in plain form today. They become
  ephemeral streams. The builder reads them sequentially, so the cost is one AES-GCM pass
  each way. Phase 2 of X01 would compress them, and compression happens before
  encryption.
* **Spooled request bodies** of Graph Store writes and uploads go to the system's
  temporary directory today, and can be gigabytes of RDF. With encryption at rest on,
  they become ephemeral streams. The parser then reads them as a stream, through the
  path compressed N-Triples and N-Quads already take, instead of mapping them.
* **Temporary generations** of in-memory backups (`<data>/tmp/memory-backup-*`) are
  written with an ephemeral data key. The upload reads them through the decrypting
  reader, and the directory is removed afterwards as today.
* **Restores** (`.restore-*`, `restore-*` verify directories) are written sealed under
  the target's keyring while the blobs stream in. The identity rule of F05 is decided
  before the download starts, so the dataset id is written once and nothing has to be
  rewritten in plaintext.
* **N-Quads dumps** through `POST /$/backup/{ds}` are written to the server's disk. On a
  dataset encrypted at rest, the route answers `409 plaintext-export-refused` unless the
  server has `--allow-plaintext-dumps`, or `[encryption] dump_recipients` lists age
  recipients. With recipients, the dump is written as `.nq.zst.age`. This breaks
  Fuseki's backup route for encrypted datasets, and the API documentation says so.
  `sparkles dump` writes to the operator's chosen file or stdout and is allowed, with a
  notice on stderr.

### 7.5 Logs, swap and core dumps

Sparkles cannot encrypt journald, swap or core files. It limits what reaches them.

* **Logs.** Query and update text is logged only at DEBUG under `sparkles::query`. With
  encryption at rest on, the server warns at start if that target is enabled. Error
  messages that quote data, such as parse errors of uploads and validation reports, go
  through a redaction step that replaces quoted literals and IRIs with a short keyed hash.
  `--log-redact` turns it on and defaults to on when any dataset is encrypted at rest.
  MongoDB's `redactClientLogData` is the precedent.
* **Swap.** Decrypted caches are ordinary memory. The documentation asks for encrypted
  swap or zram, and the NixOS module asserts it (§8.4).
* **Core dumps.** The server sets `RLIMIT_CORE` to 0 at start when any key is loaded,
  unless `--allow-core-dumps` is given. Key pages are excluded from dumps either way
  (§4.5).
* **OpenTelemetry.** Span attributes never carry query text today. Exported metrics carry
  dataset names, which are not encrypted.

### 7.6 Crash safety

The store's crash model does not change. Encryption adds three ordering rules:

1. A new data key version is durable in `keys.json` before any file is sealed under it.
   A crash in between leaves an unused version, which `sparkles check` reports as a
   warning.
2. A sealed write-once file is complete only with its trailer, or for permutation data
   with its sealed `.meta`. The generation seal in `meta.json` is written last, before
   `commit.json` and `CURRENT`, as today. A crash before `CURRENT` leaves an unfinished
   generation, which the open removes.
3. A log's session record is written in the same `write` call as the session's first
   frame, and synced with it where the file is synced per commit. A torn session record loses only frames that were never
   acknowledged.

Truncating a torn log tail ends the session, so the next frame is sealed under a new
session key (§4.3). A rollback of the delta vocabulary (`DeltaVocab::rollback`) ends the
session the same way.

### 7.7 Enabling, rotating and re-encrypting

All of these run as background tasks through the compaction build of C13. Writes continue
while the build runs, and the writer lock is held only for the switch.

* **Enabling** on an existing dataset (`sparkles crypt enable`, `POST
  /$/encryption/{ds}/enable`) writes `keys.json` with data key version 1 and starts a
  compaction. The new generation, its log and delta vocabulary are sealed. The root logs
  (`commits.bin`, `annotations.bin`) are copied sealed outside the lock, caught up under
  the lock and renamed into place during the switch. The small files are resealed in the
  same switch. Retained F06 generations stay plaintext until they expire, and
  `crypt status` lists them as `plaintextFiles`.
* **Rotating the data key** (`crypt rotate`, or automatically after `rotateAfter`) adds
  version v+1, makes it active and runs the same compaction. Afterwards every file of the
  current generation and every root file uses v+1. Version v becomes `retired` and stays
  until no file uses it, which happens when the retained generations that still use it
  expire. `crypt retire --version v` then marks it `destroyed` and removes its wraps.
  `--drop-history` first collects the retained generations that still need v, which
  erases that history cryptographically.
* **Rewrapping** (`crypt rewrap --kek NEW [--remove-kek OLD]`) changes the master key. It
  rewrites `keys.json` only and touches no data file. KMS keys that rotate inside the
  service, as AWS KMS and Vault Transit keys do, need no rewrap, because old key
  versions still decrypt. Vault's `rewrap` endpoint can refresh the stored ciphertext to
  the newest key version, and `crypt rewrap --refresh` calls it.
* **Disabling** (`crypt disable`) is the reverse of enabling. It is server-admin only and
  audited.

Converting in place does not scrub old plaintext from the device. Deleted files can stay
readable on SSDs and copy-on-write filesystems. `crypt enable` prints this, and the
documentation recommends restoring into a new dataset on a fresh volume, or relying on
operating-system encryption from the start, when the old plaintext matters.

CockroachDB rotates its data keys every 7 days by default and lets compaction rewrite old
files over time. Sparkles defaults to `rotateAfter: null`. A rotation rewrites the whole
dataset, and C13 already schedules compactions by need. Open question 5 covers the
default.

### 7.8 Key loss and recovery

* A dataset whose master key is unavailable does not open. On a server it is listed with
  `state: "locked"`, its routes answer `503 dataset-locked` with `Retry-After: 30`, and
  the server retries the unwrap with backoff. Other datasets are unaffected. This covers a
  KMS outage at startup. Datasets that are already open keep their keys in memory and
  keep serving.
* A dataset whose master keys are all lost cannot be recovered from its directory. It can
  be restored from a backup, because backups use repository keys (§5.8). The
  documentation pairs every encrypted dataset with an encrypted backup policy for this
  reason.
* A second wrap under an offline recovery key (§4.2) is the other safeguard.
  `sparkles crypt verify-keys` checks that every configured master key unwraps every
  active data key, and runs at server start.
* Revoking a KMS key makes the dataset unreadable at its next open. Open question 6 asks
  whether a running server should re-check its keys periodically and close datasets when
  a key is revoked.

### 7.9 Other features

* **Clone** builds a new generation and gets a new keyring with a new data key. The
  source and the clone share no keys.
* **F06 history.** Retained generations keep the data key version they were written
  with. Point-in-time reads decrypt their logs from `WalIndex` points, which now hold
  frame offsets.
* **`sparkles check`** needs the key. It authenticates every unit with `--full`, checks
  generation seals and session records, and reports plaintext files in an encrypted
  dataset. Without a key it checks only the plaintext files and the sealed headers, and
  says so.
* **Offline commands** (`load`, `query --loc`, `update`, `compact`, `dump`, `stats`,
  `log`, `backup create --loc`, `infer`, `shacl`, `shex`, `schema`) take the same key
  options as the server (§8.2). `sparkles log` reads `commits.bin` and therefore needs
  the key too.
* **In-memory datasets** keep nothing on disk except the ephemeral files of §7.4.
* **Python bindings.** `sparkles.Dataset.open(path, key_file=…)` and `key_env=…` pass a
  key source. P01's wheels build with the `crypt` feature.
* **The `sparkles` library** gets encryption behind a `crypt` cargo feature, which is off
  by default and on in the server. X01 keeps the library's default features free of C
  dependencies, and `aws-lc-rs` contains C. Opening an encrypted dataset without the
  feature fails with "this build cannot read encrypted datasets (feature `crypt`)".

## 8. Configuration

### 8.1 Key sources

Master keys, repository keys and KMS credentials all use one type. Secrets are never
inline in the API or the UI, as in F05 §11. Configuration names the place a secret
comes from.

```ts
type KeySource =
  | { source: "file"; path: string }                 // 32 bytes raw, or 64 hex digits, or base64
  | { source: "env"; var: string }
  | { source: "credential"; name: string }           // $CREDENTIALS_DIRECTORY/<name> (systemd)
  | { source: "passphrase-file"; path: string }      // repositories only: Argon2id slot
  | { source: "command"; argv: string[]; timeoutSecs?: number }   // prints the key on stdout
  | { source: "vault-transit"; address: string; key: string; tokenFile: string; mount?: string; namespace?: string }
  | { source: "aws-kms"; keyId: string; region?: string }         // default AWS credential chain
  | { source: "gcp-kms"; key: string; credentialsFile?: string }  // projects/…/cryptoKeys/…
  | { source: "age-identity"; path: string };        // Phase 2, repositories only
```

A key file must not be group- or world-readable, must be owned by the user Sparkles runs
as or by root, and must lie outside the data directory. Otherwise the server refuses it
with a message that names the path. Raw 32-byte input that consists only of printable
ASCII is refused, because it is almost certainly a typed password. A random key is all
printable with a probability of about 10^-13. Such a key must be given as 64
hexadecimal digits or base64 instead, which decode to the same bytes.

The `command` source covers hardware tokens, password managers and `systemd-creds
decrypt` with a TPM2-sealed credential. A key command starts with an empty environment
plus `PATH`, `HOME`, `LANG`, `XDG_RUNTIME_DIR` and `CREDENTIALS_DIRECTORY` when the
server has them. It therefore never sees `env` keys of other repositories or cloud
credentials. An `env` key stays in the server's own environment after it is read,
because removing variables from a multithreaded process is unsound. Other child
processes and anyone who can read `/proc/<pid>/environ` can still see it.

### 8.2 Server and CLI

```toml
# /etc/sparkles/encryption.toml, passed as --encryption-config. It names key sources and holds no secrets.
version = 1

[encryption]
new_datasets = "encrypted"            # "encrypted" | "plaintext" (default "plaintext")
default_kek = "main"
crypt_cache_mb = 256
rotate_after = "180d"                 # optional; default none
log_redact = true                     # default: true when any dataset is encrypted
dump_recipients = ["age1…"]           # optional; see §7.4

[encryption.keks.main]
source = "credential"
name = "sparkles-master-key"

[encryption.keks.recovery]           # second wrap, for escrow
source = "file"
path = "/mnt/offline/sparkles-recovery.key"
wrap_only = true                      # used to add wraps, never needed to open

[encryption.keks.kms]
source = "aws-kms"
key_id = "arn:aws:kms:eu-central-1:111122223333:key/…"
```

The server takes `--encryption-config FILE`, or the shorthands `--master-key-file PATH`
and `--master-key-env VAR` for a single local master key, plus `--crypt-cache-mb`,
`--log-redact`, `--allow-plaintext-dumps` and `--allow-core-dumps`. Every offline command
accepts the same options, and `SPARKLES_ENCRYPTION_CONFIG` and `SPARKLES_MASTER_KEY_FILE`
set them from the environment.

```
sparkles crypt keygen [--out FILE]                       # 32 random bytes, file mode 0600
sparkles crypt status     --loc DB [--format text|json]  # versions, files per version, plaintext files
sparkles crypt enable     --loc DB [--kek NAME]
sparkles crypt disable    --loc DB
sparkles crypt rotate     --loc DB
sparkles crypt rewrap     --loc DB --kek NEW [--remove-kek OLD] [--refresh]
sparkles crypt retire     --loc DB --version V [--drop-history]
sparkles crypt verify-keys --loc DB
sparkles load --loc DB --encrypt FILES…                  # a new dataset, encrypted

sparkles repo add NAME … --encrypt (--key-file F | --key-env V | --passphrase-file F
                  | --key-command CMD… | --vault … | --aws-kms ARN | --gcp-kms KEY) [--single-key-ok]
sparkles repo key list|add|remove|rotate-master|retire|export --repo R …
```

The backup config file of F05 §2.9 gains an `encryption` table per repository:

```toml
[repositories.s3-main.encryption]
key = { source = "credential", name = "sparkles-repo-s3-main" }
allow_plaintext = false
```

### 8.3 HTTP API

| Method | Path | C09 need | Result |
|---|---|---|---|
| GET | `/$/encryption` | S(server-admin) | The configured master keys (names, sources, `kekId`s, reachability), the cache size and the datasets by state |
| GET | `/$/encryption/{ds}` | A | `{ encrypted, keyringId, cipher, deks: [{version, state, created, files}], plaintextFiles, sealedBytes }` |
| POST | `/$/encryption/{ds}/enable` | S(server-admin) | `202 Task` (kind `crypt`) |
| POST | `/$/encryption/{ds}/disable` | S(server-admin) | `202 Task` |
| POST | `/$/encryption/{ds}/rotate` | S(server-admin) | `202 Task` |
| POST | `/$/encryption/{ds}/rewrap` | S(server-admin) | Body `{ kek, removeKek?, refresh? }`. `200` with the new keyring summary. |
| POST | `/$/encryption/{ds}/retire` | S(server-admin) | Body `{ version, dropHistory? }`. `200`, or `409 dek-in-use` with the files that use it. |
| POST | `/$/datasets` | existing | The body gains `encryption: "default" \| "encrypted" \| "plaintext"`. |
| POST | `/$/repositories` | existing | `RepositoryConfig` gains `encryption?: { key: KeySource; allowPlaintext?: boolean }`. A `KeySource` from the API must name a source that the backup config file allows, as F05's credential sources already do. |
| POST | `/$/repositories/{repo}/keys` | S(server-admin) | Adds a key slot. `201`. |
| DELETE | `/$/repositories/{repo}/keys/{id}` | S(server-admin) | `204`, or `409 last-key-slot` |

`DatasetInfo` gains `encryption: { encrypted: boolean; state: "open" | "locked" }`.
`Repository` gains `encryption: null | { scheme, epochs, slots: number, writeOnly }`. The
new error codes are `repository-key-required`, `wrong-repository-key`, `last-key-slot`,
`unencrypted-repository`, `plaintext-target`, `read-capability-required`,
`dataset-locked`, `dek-in-use`, `plaintext-export-refused`, `encryption-unsupported` and
`key-provider-unavailable` (502, with the provider's message, credentials removed).

### 8.4 NixOS module

```nix
services.sparkles.encryption = {
  enable = true;
  # Loaded with systemd LoadCredential= (or LoadCredentialEncrypted= for a TPM2-sealed file).
  masterKeyFile = "/run/agenix/sparkles-master-key";
  masterKeyEncrypted = false;        # true: the file is a systemd-creds blob
  configFile = null;                 # a full encryption.toml for KMS keys; outside the Nix store
  newDatasets = "encrypted";
  cacheMb = 256;
};
services.sparkles.backup.repositoryKeys = {
  s3-main = "/run/agenix/sparkles-repo-s3-main";   # passed as credential sparkles-repo-s3-main
};
```

The module passes keys with `LoadCredential=` or `LoadCredentialEncrypted=`, so they
appear under `$CREDENTIALS_DIRECTORY` for the service alone and never in the Nix store.
It sets `LimitCORE=0`. Its assertions reject key paths in the Nix store or under
`dataDir`, and reject `encryption.enable` when a swap device has neither
`randomEncryption` nor `encrypted` set and zram swap is off. The assertion message
explains how to override it. The VM test of the module gains an encrypted dataset, a
restart, and a check that no file under `dataDir` contains a loaded literal.

### 8.5 UI

The dataset page shows an "Encrypted" badge with the data key version and the date of
the last rotation, and server admins get **Rotate key**. The Backups page shows a lock
badge on encrypted repositories, a warning on repositories with a single key slot, and
a **Key slots** panel. The Add repository form gains an encryption section whose key
selector offers only the key sources the config file allows. None of these forms has a
secret field.

## 9. Observability

| Name | Type | Labels |
|---|---|---|
| `sparkles_crypt_bytes_total` | counter | `op` = `encrypt`\|`decrypt`, `kind` = `column`\|`page`\|`log`\|`record`\|`small`\|`spill`\|`blob` |
| `sparkles_crypt_auth_failures_total` | counter | `kind` |
| `sparkles_crypt_cache_hits_total`, `…_misses_total` | counter | (none) |
| `sparkles_crypt_cache_bytes` | gauge | (none) |
| `sparkles_crypt_key_provider_requests_total` | counter | `provider` = `vault-transit`\|`aws-kms`\|`gcp-kms`\|`command`, `op` = `unwrap`\|`wrap`, `result` |
| `sparkles_crypt_key_provider_duration_seconds` | histogram | `provider` |
| `sparkles_crypt_dek_age_seconds` | gauge | `dataset` (age of the active data key) |
| `sparkles_crypt_files` | gauge | `dataset`, `state` = `active`\|`retired`\|`plaintext` |
| `sparkles_crypt_datasets_locked` | gauge | (none) |
| `sparkles_backup_repository_encrypted` | gauge (0 or 1) | `repository` |
| `sparkles_backup_cdc_chunks_total` | counter | `repository`, `result` = `new`\|`reused` |

`dataset` and `repository` labels are capped as in C01 and F05. Audit events
(`sparkles::audit`) are `crypt_enabled`, `crypt_disabled`, `dek_rotated`, `dek_retired`,
`kek_rewrapped`, `repo_key_added`, `repo_key_removed`, `repo_master_rotated`,
`repo_key_exported`, and `plaintext_override` for every use of `allowPlaintext`. Each
carries the principal and never key material. `sparkles_crypt_auth_failures_total` is
the metric to alert on, because it counts corruption and tampering.

## 10. Performance

**Measured cipher rates.** `openssl speed -evp` on the development laptop (an Intel Core
Ultra X7 358H with AES-NI and VAES, single run, not the quiet benchmark host) gave these
rates per core:

| Cipher | 128 B | 16 KiB | 64 KiB |
|---|---|---|---|
| AES-256-GCM | 0.63 GB/s | 5.8 GB/s | 6.9 GB/s |
| ChaCha20-Poly1305 | | 1.06 GB/s | |

A 16 KiB page decrypts in about 2.8 µs, and a 64 KiB column in about 9.5 µs. Phase 1 and
Phase 3 each start by repeating these numbers on the benchmark host with `aws-lc-rs`
itself.

**Estimates at 10.5M quads**, with an index of about 286 MB:

| Workload | Estimate | Why |
|---|---|---|
| Warm query suite | 0–10% slower | Decoded columns hit the block cache as today. Term lookups go through the decrypted-page cache, which adds a hash lookup per page. Result-heavy queries that serialize millions of terms see the most. |
| First queries after open | 10–30% slower | Each column miss adds decryption to LZ4 and varint decoding, and each vocabulary page miss adds about 3 µs. |
| Single-quad updates | Under 1% | A commit seals about 100 bytes, under 1 µs, against a `sync_data` of tens of microseconds or more. The WAL grows by 21 bytes per commit for the frame header and tag, and the catalog by 32. |
| Bulk load and compaction | Under 5% | 286 MB of output at about 6 GB/s is under 0.1 s of CPU across threads. The spill files cost one pass each way. |
| Open | +10–50 ms per dataset with a KMS | One network round trip to unwrap the data key. Local keys add microseconds. Decrypting `.meta` and `vocab.off` takes milliseconds. |
| Encrypted backup, full | About +10% over F05's 0.56 s | Encryption adds a pass at about 6 GB/s, and keyed ids cost the same as SHA-256. |
| Memory | +`--crypt-cache-mb` | §7.3. The default adds up to 256 MiB of process memory. A smaller cache costs vocabulary lookups on misses. |

LUKS2 and ZFS native encryption change none of the warm rows, because the page cache
holds plaintext. They add their cipher cost to cold reads and writes, in the kernel.

**Gates.** The gates decide what the documentation says, not whether Phase 3 ships. If
the warm suite at 10.5M is more than 10% slower with encryption at rest, or the first
queries after open are more than 30% slower, the documentation states the measured cost
next to the recommendation of §3, and the README lists encryption at rest as a measured
tradeoff.

**How to measure.** `scripts/bench.sh` gains an `--encrypt` switch. It loads the same
data into an encrypted dataset with a key file and runs the 28-query suite warm and after
a restart, interleaved with the unencrypted run, on the quiet benchmark host with the
client pinned, as `docs/BENCHMARKS.md` describes. `scripts/backup-bench.sh` gains an
encrypted `fs` repository and the CDC run of §6.4. The results go to
`docs/BENCHMARKS.md`.

## 11. Phasing

**Phase 1: encrypted backup repositories.**

* The `sparkles-backup` changes of §5: key slots with the `file`, `env`, `credential`,
  `passphrase-file`, `command` and `vault-transit` sources, and `aws-kms` and `gcp-kms`
  behind cargo features. Keyed ids, sealed blobs with PADMÉ padding, sealed manifests,
  epochs and `repo key …`.
* The checks of §5.8 come with Phase 3, because only Phase 3 creates datasets that are
  encrypted at rest.
* The API, the CLI, the config file, the UI's lock badge and key slot panel, the NixOS
  `repositoryKeys` option, metrics, audit events and acceptance examples A1–A10.

**Phase 2: chunking and write-only repositories.**

* FastCDC with the keyed gear table, the measurement of §6.4 and the default it decides.
* Write-only repositories with age recipients (the `age` crate) and the `age-identity`
  key source. A11–A13.

**Phase 3: encryption at rest.**

* The keyring, key sources, the sealed formats, sealed permutations, paged vocabulary
  with the decrypted-page cache, sealed logs, small sealed files and ephemeral streams.
* Enable, rotate, rewrap, retire and disable through the compaction build.
* Locked datasets, key verification at start, log redaction, core dump limits, dump
  refusal and age-encrypted dumps.
* Vector and spatial indexes in memory only on encrypted datasets. A full-text index
  cannot be enabled on one (`409 encryption-unsupported`).
* `sparkles crypt …`, the API, the NixOS options and assertions, metrics, the benchmark
  run, and A14–A26.

**Phase 4: derived indexes.**

* The encrypting Tantivy directory, and paged vector and spatial index files decrypted
  into memory at open. A27 and A28.

## 12. Acceptance examples

The setup is the one of F05 §9. A key file is created with `sparkles crypt keygen --out
/tmp/k` (mode 0600), and a second one at `/tmp/k2`. A "plaintext scan" means
`grep -rIl -e 'urn:a' -e 'hello world' DIR` plus the same search over `strings` of every
file. It must find nothing.

**Phase 1.**

**A1. Encrypted repository.**
* `sparkles repo add enc --path /tmp/re --encrypt --key-file /tmp/k --single-key-ok` →
  the marker has `encryption.scheme == "sparkles-repo-v1"`, and `keys/` holds one slot.
* Back up `ds` as `b1`. A plaintext scan of `/tmp/re` finds nothing, and no object name
  under `blobs/` equals the plain SHA-256 of any piece of `ds`.
* Without `--single-key-ok`, `repo add` with one slot fails and says why. A key file
  under the data directory is refused (§8.1).

**A2. Deduplication preserved.**
* Commit 4, back up `b2`. Every immutable file has the same blob ids in `b1` and `b2`,
  and `addedBytes` < 12 KB (padding included).
* `sparkles backup restore --repo enc b2 --to /tmp/dr` → a dump of `/tmp/dr` equals a dump
  of `ds` at commit 4.

**A3. Repositories do not link.** The same dataset backed up to `enc` and to `enc2`,
initialized with `/tmp/k2`, shares no blob id.

**A4. Wrong or missing key.**
* `backup list --repo enc` without a key → exit 1, `repository-key-required`.
* With `/tmp/k2` → `422 wrong-repository-key`, listing the expected `kekId`.

**A5. Tampering is detected.**
* Flip one ciphertext byte of a blob → `verify --level data` reports it in `corrupt`, and
  a restore fails with `authentication failed for blob <id> (gen-0001/spo.dat)`. Nothing
  is published.
* Copy blob X's object over blob Y's path → the same failure for Y.
* Copy `backups/b1.json` to `backups/b9.json` → `422 invalid-backup` (manifest
  authentication failed).

**A6. Key slots.**
* `repo key add --repo enc --passphrase-file /tmp/pw`, then `repo key remove` the key-file
  slot. `backup list` works with the passphrase only.
* Removing the last slot → `409 last-key-slot`.

**A7. Master key rotation.**
* `repo key rotate-master` → epoch 2. The next backup `b3` has `addedBytes` ≈
  `logicalBytes`. `b1` and `b2` still restore.
* Delete `b1` and `b2` and run GC with a grace of 0 → no epoch-1 blob remains, and
  `repo key retire --epoch 1` succeeds. Before the GC it fails and names the backups
  that still use epoch 1.

**A8. Vault Transit** (opt-in, when `SPARKLES_TEST_VAULT_ADDR` is set, against a Vault dev
server).
* A repository with a `vault-transit` slot backs up and restores.
* With the token revoked → `502 key-provider-unavailable`, and the message holds no token.

**A9. Older builds** refuse the repository (F05 A16), and a Phase 1 build refuses a
marker with `writeOnly: true` with `422 incompatible-repository`.

**A10. Metrics and audit.** After A1–A7, `sparkles_backup_repository_encrypted{repository="enc"} 1`,
`sparkles_crypt_bytes_total{op="encrypt",kind="blob"}` > 0, and audit events
`repo_key_added`, `repo_key_removed` and `repo_master_rotated` with the principal.

**Phase 2.**

**A11. Chunking after a compaction.** The run of §6.4 prints the uploaded bytes of
`vocab.dat` for fixed pieces and for FastCDC. The result and the default it decided go
into the Outcome.

**A12. Keyed boundaries.** The same `vocab.dat` in two encrypted repositories with
different keys gives different chunk size sequences. In one repository, two backups of
an unchanged file give the same sequence.

**A13. Write-only.**
* A repository with one age recipient. The server holds the writer key only. Backups and
  GC work, and verification at the `exists` level reports ok.
* A restore on that server → `403 read-capability-required`.
* On another host with the identity, `backup restore` succeeds, and so does a `data`
  verify.

**Phase 3.**

**A14. An encrypted dataset.**
* `sparkles load --loc /tmp/edb --encrypt --master-key-file /tmp/k d.nt`, then an
  update that adds `"new term"`. A plaintext scan of `/tmp/edb` finds nothing, and
  `"new term"` is not found either.
* `sparkles query --loc /tmp/edb 'SELECT * {?s ?p ?o}'` without a key → exit 1, `this
  dataset is encrypted (keyring 5b9e…); give --master-key-file or --encryption-config`.
* With the key, the answer equals that of the same data loaded unencrypted.

**A15. Tampering.**
* Flip a byte inside column 2 of block 0 of `spo.dat` → a query that scans it fails with
  `500` and `authentication failed: gen-0001/spo.dat block 0 column 2 (dek v1)`.
  `sparkles check --full` reports it with exit 1.
  `sparkles_crypt_auth_failures_total{kind="column"}` increments.
* Swap `pso.dat` and `pos.dat` → both fail authentication at open.
* A test hook seals a second `gen-0001/spo.dat` with the dataset's own keys and a new
  salt. Swapped in, it fails the generation seal check at open.

**A16. Torn and corrupt logs.**
* A failpoint kills the writer after it writes half a frame. Reopen → the tail is
  truncated, the head is the last acknowledged commit, and `check` is ok.
* Corrupt a frame in the middle of `wal.log` → the open fails with `Corrupt` and names
  the frame's offset.

**A17. No nonce reuse across crashes (unit).** A test hook records every (key
fingerprint, nonce) pair sealed. A loop of 10,000 commits with a crash and reopen every
100 commits, plus delta-vocabulary rollbacks from failed validations, records no pair
twice.

**A18. Copies are safe.** Copy `/tmp/edb` to `/tmp/edb2` with `cp -r`, commit to both,
and check with the hook that their new frames use different session salts.

**A19. Rotation.**
* Name a snapshot `s1`, then `crypt rotate`. After the task, `crypt status` shows every
  file of the current generation and every root file on v2, and `gen-0001` on v1 held by
  `s1`.
* `crypt retire --version 1` → `409 dek-in-use` listing `gen-0001`. Drop `s1`, collect
  history, retire again → v1 is `destroyed`, and its wraps are gone from `keys.json`.
* Updates run throughout. None fails, and the writer lock is held only for the switch.

**A20. Enabling and rewrapping.**
* On a plaintext dataset with history, `crypt enable` → after the task, `plaintextFiles`
  lists only the retained generations, and the command printed the remanence warning.
* `crypt rewrap --kek new --remove-kek main` with `new` = `/tmp/k2` → no file other than
  `keys.json` changed (by checksum). The dataset opens with `/tmp/k2` and not with
  `/tmp/k`.

**A21. Temporary files.**
* Pause a bulk load of 1M quads at a failpoint after the spills → a plaintext scan of
  the builder's `tmp/` finds no term.
* Pause a 100 MB Graph Store PUT while it spools → the spooled file in the temporary
  directory has the `SPKE` magic and passes the plaintext scan.
* `POST /$/backup/ds` → `409 plaintext-export-refused`. With `dump_recipients`, the
  `.nq.zst.age` file decrypts with `age -d` to the dump.

**A22. Backups of encrypted datasets.**
* A backup of the encrypted `ds` to the plaintext repository `local` → `409
  unencrypted-repository`. To `enc`, it succeeds.
* The blob ids of `vocab.dat` equal those of a backup of the same data loaded
  unencrypted.
* Restoring it on a server without encryption at rest → `409 plaintext-target`. With
  `encryption.new_datasets = "encrypted"`, the restored files are sealed, and a plaintext
  scan of the `.restore-*` directory at a failpoint finds nothing.

**A23. Locked datasets.**
* Start the server with a Vault Transit master key and Vault down → `ds` is listed with
  `state: "locked"`, and `GET /ds/sparql?query=…` answers `503 dataset-locked` with
  `Retry-After`. Other datasets serve.
* Start Vault → within the retry backoff `ds` opens and serves.

**A24. Logs.** With `--log-redact`, an upload with a syntax error in a literal logs the
error with the literal replaced by `«redacted:3f9a…»`. The server warns at start when
`RUST_LOG` enables `sparkles::query=debug`.

**A25. Indexes in Phase 3.** On an encrypted dataset, `PUT /$/text/ds` →
`409 encryption-unsupported`. With a vector index and a spatial index configured, no
`.spkv` or `.spkg` file is written, and after a restart both indexes answer as before.

**A26. Performance (bench, not CI).** The runs of §10, with the gates applied.

**Phase 4.**

**A27. Encrypted full-text index.** `text-index --loc /tmp/edb` → a plaintext scan of
`text/` finds no indexed word. `text:query` results equal those of the unencrypted
dataset, and `check --full` authenticates every page.

**A28. Encrypted vector and spatial files.** The files exist sealed, the indexes load
from them at open without parsing literals, and `sparkles stats` reports the memory
they hold.

## 13. Rejected alternatives

* **Doing nothing and documenting operating-system encryption.** This is the recommended
  default (§3) but not sufficient. It does not protect backups in third-party storage,
  per-dataset keys, or disks visible to storage administrators.
* **Unauthenticated modes (CTR or XTS) at the file layer,** as in RocksDB's encrypted
  environment, CockroachDB and the PostgreSQL TDE proposal. They are simpler to place
  under existing code because they keep byte offsets, but a changed byte then turns into
  wrong data, and the storage holder can flip bits in a CTR stream at will. Sparkles
  already decodes in units, so authenticating each unit costs 16 bytes and nothing else.
* **Decrypting whole files into memory at open.** It is the simplest design, but it makes
  the process hold the whole index. That gives up the design point that lets QLever-style
  indexes exceed memory. It is used only for files that are read whole anyway (`.meta`,
  `vocab.off`) and for the Phase 4 vector and spatial files, where §7.3 states the cost.
* **SQLCipher-style fixed pages for every file.** SQLCipher seals each 4096-byte page with
  a random IV and an HMAC. Paging the permutation files would put a second cache in front
  of the block cache. Columns are already the unit of decoding, so they are the unit of
  sealing.
* **Random 96-bit GCM nonces.** NIST limits them to 2^32 invocations per key. The commit
  catalog of a busy dataset can pass that within weeks at a thousand commits per second.
* **XChaCha20-Poly1305 as the only cipher.** Random 192-bit nonces would remove the
  session machinery, and the draft expects the first collision of random nonces only
  after about 2^96 messages. It is
  not FIPS-approved, which matters to most of the deployments that ask for encryption at
  rest, and it measured about 5.4 times slower than AES-256-GCM on the development
  laptop.
* **AES-GCM-SIV.** It tolerates repeated nonces, but it needs two passes to encrypt and
  is not a NIST-approved mode.
* **Convergent encryption for backups** (§5.2).
* **Backing up the sealed at-rest files as they are.** Restores would then depend on the
  dataset's master key, the backup would not deduplicate with an unencrypted copy, and
  chunking would see only ciphertext. Logical plaintext under the repository key keeps
  the two key systems apart.
* **Encrypting only the vocabulary and leaving ids in plain form.** It would be cheaper,
  but the graph's shape, degree distributions and the statistics remain, and they are
  enough to re-identify many datasets. It would also be a second mode to test and
  explain.
* **A FUSE filesystem such as gocryptfs under the data directory.** It is an
  operating-system option and belongs in §3. Built in, it would add a kernel round trip
  to every read.
* **Packing small blobs as restic does.** F05 rejected pack files, and the chunk sizes of
  §6.2 keep object counts low without them.
* **Storing the master key next to the data, as MongoDB's local key file mode does.** A key
  file on the same disk protects nothing against a stolen disk. Sparkles refuses key
  files under the data directory.
* **A 7-day default rotation of data keys,** as CockroachDB does. A Sparkles rotation
  rewrites the dataset, so periodic rotation is opt-in (`rotate_after`).

## 14. Open questions

1. **Page size.** 16 KiB pages cost 0.1% in tags and about 3 µs per miss. 64 KiB pages
   halve the cache's entries and make cold binary searches cheaper. Default: 16 KiB, to
   be measured on the term-heavy queries (`distinct-obj`, `lang-filter`, `regex-iri`).
2. **One cache or two.** Should decrypted vocabulary and text pages share the block
   cache's budget instead of `--crypt-cache-mb`? Default: separate, so that each cost is
   visible.
3. **Server-wide files.** `config.json` names datasets, `auth/` holds password hashes
   and the session key, and `backup/` holds repository settings and cached sealed
   manifests. Encrypt them under a server keyring? Default: no. They hold no dataset
   content, and the auth secrets are hashes or are kept in files outside the data
   directory by the NixOS module.
4. **FIPS builds.** `aws-lc-rs` has a FIPS feature that needs Go and CMake at build
   time. Offer a `fips` build of the server, or document it only? Default: document it.
5. **Rotation default.** `rotate_after = null`, or 365 days? Default: null.
6. **Revocation while running.** Re-check every master key every 15 minutes and close the
   datasets whose key the KMS no longer unwraps? It makes revocation effective, at the
   cost of outages when the KMS is merely unreachable. Default: off, with
   `recheck_interval` as an option.
7. **Which `aws-lc-rs` APIs.** `aead::LessSafeKey` with explicit nonces is required for
   counter nonces. Its use must be confined to one module with the nonce rules of §4.3
   and their tests.
8. **Hiding sizes at rest.** Pad pages and frames to fixed buckets? Default: no. Sizes at
   rest reveal little that the file names do not.
9. **Rollback detection.** Record the latest commit sequence in a TPM monotonic counter or
   in the KMS, and refuse to open a dataset whose head is older? Default: not built, and
   documented as a limit.
10. **Dumps.** Is `409 plaintext-export-refused` on `POST /$/backup/{ds}` too strict for
    Fuseki clients? Default: refuse, with the flag and age recipients as the ways out.
11. **KMS client weight.** `aws-sdk-kms` and Google's client libraries pull large
    dependency trees. Use them behind features, or sign the few requests needed with the
    signing code `object_store` already contains? Default: features, to be checked for
    duplicate HTTP stacks as F05's open question 8 did.
12. **Chunking in unencrypted repositories.** Should the gear table be the paper's fixed
    table, so that unencrypted repositories of different servers chunk alike? Default:
    derived from the repository id, which costs nothing.

## 15. Sources

* **Sparkles repository, read for this spec:**
  * `crates/sparkles-core/src/index.rs`: the permutation file format, `BLOCK_ROWS`,
    `BlockMeta`, `PermIndex::open` (memory-mapped `.dat`, `.meta` read whole) and
    `BlockCache` (decoded columns by permutation, block and column);
  * `crates/sparkles-core/src/vocab.rs`: the front-coded, memory-mapped `Vocab`, `FC_BLOCK`,
    `get`, `get_sorted`, `find`, and `DeltaVocab` (length-prefixed log, torn tails,
    `rollback`, `flush`);
  * `crates/sparkles-core/src/store.rs` and `store/wal.rs`: the 33-byte WAL records, the
    commit path, `WalIndex` and `WalPoint`;
  * `crates/sparkles-core/src/builder.rs`: the spill files in `tmp/`;
  * `crates/sparkles-core/src/vector/persist.rs` and `geo/persist.rs`: the `.spkv` and
    `.spkg` layouts and their in-place mapping;
  * `crates/sparkles-core/src/text/lazydir.rs`: the Tantivy directory wrapper;
  * `crates/sparkles-core/src/annotations.rs`, `commit.rs`, `stored.rs`,
    `store/compaction.rs` and `disk.rs`: `annotations.bin`, the catalog's magic,
    `queries.json`, `compaction.json` and the free-space checks;
  * `crates/sparkles-backup/src/blob.rs`, `layout.rs` and `manifest.rs`: the blob header
    with its reserved encryption byte, and the marker and manifest checks that refuse
    `encryption`;
  * `crates/sparkles-server/src/http.rs`: spooled request bodies in the system's
    temporary directory;
  * `nix/module.nix`: the secret-handling options of `auth` and `backup`;
  * the directory of a small database built with the release binary (`load`, `update`,
    `text-index`, `geo-index`) and listed with `find`, for the file inventory of §7.1;
  * `Cargo.lock`: `aws-lc-rs` 1.18.1, `argon2` 0.6.0, `zeroize` 1.9.0, `sha2` 0.11.0,
    `memmap2` 0.9.11, `quick_cache` 0.7.0, `tantivy` 0.26.2, `object_store` 0.14.2;
  * the specs [F05](F05-snapshot-repositories.md), [C13](C13-automatic-compaction.md),
    [X01](X01-compression-codecs.md), §4.3 and §5.5 of [CI](CI-commit-identity.md), the
    outlines of [F03](F03-full-text-search.md) and
    [F04](F04-vector-search.md), [PROVENANCE.md](PROVENANCE.md), and `docs/API.md` on the
    access log and spooled bodies.
* **Fetched, standards and papers:**
  * NIST SP 800-38D, "Recommendation for Block Cipher Modes of Operation: Galois/Counter
    Mode (GCM) and GMAC", §8 (IV constructions, the 2^32 limit for random IVs),
    https://nvlpubs.nist.gov/nistpubs/Legacy/SP/nistspecialpublication800-38d.pdf;
  * draft-irtf-cfrg-xchacha-03, "XChaCha: eXtended-nonce ChaCha and
    AEAD_XChaCha20_Poly1305" (192-bit nonces, HChaCha20, random nonces),
    https://datatracker.ietf.org/doc/html/draft-irtf-cfrg-xchacha;
  * W. Xia, Y. Zhou, H. Jiang, D. Feng, Y. Hua, Y. Hu, Y. Zhang and Q. Liu, "FastCDC: a
    Fast and Efficient Content-Defined Chunking Approach for Data Deduplication", USENIX
    ATC 2016 (abstract and introduction),
    https://www.usenix.org/conference/atc16/technical-sessions/presentation/xia.
* **Fetched, public product and project documentation (design ideas only):**
  * restic, "References" (AES-256-CTR with Poly1305-AES, scrypt, key files, storage and
    blob ids, the random chunker polynomial),
    https://restic.readthedocs.io/en/stable/100_references.html;
  * borg, "Security" internals (encrypt-then-MAC, keyed chunk ids, the secret chunker
    seed, chunk size fingerprinting and the obfuscation option, counter reservations,
    repokey and keyfile modes),
    https://borgbackup.readthedocs.io/en/stable/internals/security.html;
  * CockroachDB, "Encryption" (AES-CTR, store and data keys, 7-day data key rotation,
    re-encryption through compaction churn, 5–10% CPU),
    https://docs.cockroachlabs.com/docs/stable/security-reference/encryption;
  * MongoDB, "Encryption at Rest" (AES256-CBC and AES256-GCM, master and database keys,
    KMIP, unencrypted audit logs, log redaction),
    https://www.mongodb.com/docs/manual/core/security-encryption-at-rest/;
  * SQLCipher, "Design" (AES-256-CBC per page, random per-page IVs, HMAC-SHA512 per page,
    PBKDF2), https://www.zetetic.net/sqlcipher/design/;
  * RocksDB, `include/rocksdb/env_encryption.h` (block-addressable cipher streams, a
    per-file prefix, no authentication), read as a public header for its design,
    https://github.com/facebook/rocksdb/blob/main/include/rocksdb/env_encryption.h;
  * PostgreSQL wiki, "Transparent Data Encryption" (scope, two-tier keys, XTS and CTR,
    LSN-based IVs, the integrity gap),
    https://wiki.postgresql.org/wiki/Transparent_Data_Encryption;
  * AWS KMS Developer Guide, "AWS KMS cryptography essentials" (AES-GCM, envelope
    encryption, per-call derived keys),
    https://docs.aws.amazon.com/kms/latest/developerguide/kms-cryptography.html;
  * Google Cloud KMS, "Envelope encryption" (DEK and KEK, local DEKs, the 64 KiB limit,
    associated data), https://docs.cloud.google.com/kms/docs/envelope-encryption;
  * HashiCorp Vault, "Transit secrets engine" (data keys, versioned keys, rewrap,
    convergent mode, `aes256-gcm96`), https://developer.hashicorp.com/vault/docs/secrets/transit;
  * OpenZFS, `zfsprops(7)` (the `encryption` property, `aes-256-gcm` default, key
    formats), https://openzfs.github.io/openzfs-docs/man/master/7/zfsprops.7.html;
  * Linux kernel documentation, "Filesystem-level encryption (fscrypt)" (threat model,
    XTS modes, unprotected metadata), https://docs.kernel.org/filesystems/fscrypt.html.
* **Cited from general knowledge, not fetched** (to be checked at implementation):
  * RFC 8439 (ChaCha20-Poly1305), RFC 5869 (HKDF), RFC 2104 (HMAC), RFC 9106 (Argon2),
    RFC 8452 (AES-GCM-SIV), NIST SP 800-108 (KDF in counter mode);
  * the age format, https://age-encryption.org/v1 (the fetch was redirected to the C2SP
    repository, which returned an error), and the `age` and `rage` crates
    (MIT/Apache-2.0);
  * LUKS2 and dm-crypt, dm-integrity, systemd `LoadCredential=`,
    `LoadCredentialEncrypted=` and `systemd-creds`;
  * K. Nikitin, L. Barman, W. Lueks, M. Underwood, J.-P. Hubaux and B. Ford, "Reducing
    Metadata Leakage from Encrypted Files and Communication with PURBs", PETS 2019
    (PADMÉ padding);
  * the `aws-lc-rs` API (`aead`, `hkdf`, `hmac`) and its FIPS feature.
* **Measured:** `openssl speed -evp aes-256-gcm` and `-evp chacha20-poly1305` on the
  development laptop, for §10.
* No source code of restic, borg, CockroachDB, MongoDB, SQLCipher or PostgreSQL was
  read. The RocksDB header was read for its interface and comments only.

## Outcome

The optional `sparkles-backup` `encryption` feature implements the local encrypted
repository engine from part of Phase 1. Existing defaults and plaintext repositories
retain their behavior. `Repository::open_encrypted` accepts explicitly resolved local
32-byte keys or passphrases. The facade adds reference-only file, environment, systemd
credential, passphrase-file and literal-argv command providers. The offline CLI and
trusted operator TOML server path resolve them before opening the engine. Both
encryption features remain off by default.
This delivers part of the operator workflow, with further Phase 1 work remaining.

Repositories use random master keys and wrapped keyslots, the specified AES-256-GCM,
HKDF-SHA256 and HMAC-SHA256 scheme, keyed blob and append IDs, authenticated manifests,
and encrypted manifest caches. Create, list, restore, verification and garbage collection
support encrypted objects. Fixed-piece chunking remains; rotation excludes cross-epoch
append/dedup parents. Additive key listing, addition, removal, rotation and epoch retirement
use exclusive leases and recoverable publication intents. Retirement proves that no
remaining manifest or blob needs the epoch, validates all surviving deletion targets
before mutation, and permits independent offline recovery credentials to remain offline.
Unleased readers retry inconsistent marker/slot snapshots during management publication.
Native filesystem management pins the backend's canonical root, holds an advisory lock
and validates the exclusive lease before durable atomic marker replacement; custom
object stores retain conditional updates. Rotation and retirement survive interruption
before or after publication. A retargeted configured filesystem alias fails closed.

The offline CLI supports encrypted initialization, ordinary backup operations,
configured policy execution and key listing/addition/removal/master rotation/epoch
retirement. Policy execution retains the protected repository handle, resolves only
selected credentials/providers after acquiring the stopped catalog lock, refuses key
overrides and validates catalog/repository locations and protected key inputs before
initialization. The explicit opened-repository API also validates current live attachment
roots; session-only attachments are not persisted for offline discovery. Creation, retention
and GC share cancellation; encrypted repository execution failures use static text. Added recovery references
remain offline, and passphrase additions retry using the existing slot salt. TOML
retains provider references in `ConfiguredRepository`; plain projections are now
fallible and reject encrypted tables. Explicit retaining registry methods support trusted
server TOML without exposing provider references in API views or persistence. API
registration/update and managed API JSON reject encryption metadata, including null,
before it can be silently projected into plaintext configuration.

Trusted configured encrypted repositories support the existing server backup, restore,
verify, GC and scheduled-policy flows on persistent backends. Cache misses resolve
providers outside registry locks, including background startup/reload reachability checks;
listener startup does not wait for those background checks. A per-generation async gate
single-flights provider/KDF/open work. Every encrypted SIGHUP reload invalidates handles,
even with unchanged references, and retains known UUID checks for the same location.
Generation checks before engine open and before return/cache reject stale completions;
operations already underway may finish on retained handles. Status/test-report writes are
also generation guarded, and encrypted reachability/connection-test errors use static
text. Secret files under server data, repositories or attached datasets are refused.
Configured encrypted memory repositories are refused because reload/restart cannot
preserve their ephemeral data/identity.

Command cleanup kills the provider group and retains sole child ownership until it is
reaped, including dropped resolution futures and runtime shutdown. A failed cleanup
thread spawn reaps synchronously rather than abandoning the child.

Task cancellation reaches provider resolution and cache waits from backup/restore/verify/GC
and scheduled-policy list/retention. Engine opening retains existing backend deadlines and
checks cancellation before cache publication. A canceled final capture/retention operation
classifies the policy as canceled and suppresses subsequent retention/GC; ordinary
recoverable dataset failures retain their behavior.

Parsing, ciphertext and Argon2 inputs are bounded before expensive allocation or key
derivation. Secret diagnostics are redacted. Long-lived keys and retained passphrases
occupy dedicated locked pages excluded from dumps and are zeroized before release.
That backend currently supports Linux/Android; unsupported protection fails explicitly
rather than silently weakening it. Master keys are generated, decoded and unwrapped
into zeroizing buffers, but moves and provider input buffers can leave transient copies,
as §4.5 describes. Passphrase work runs on blocking workers limited to two
simultaneous key jobs.

The marker's descriptor is authenticated under the active epoch's master key, its
active epoch must be the newest epoch, and each host keeps an epoch record per
repository in memory and in the repository's cache directory (§2.3). Tamper tests
cover a stripped descriptor, a flipped active epoch, a re-added retired epoch with its
old slots restored, a removed tag, and a rollback signed with a leaked old master key.
Repositories created before this change open unchanged and gain the tag on their next
rotation or retirement. New key slots use format 2, which binds slot metadata into the
wrap. Duplicate passphrase slots are refused before key derivation. Repository handles
keep unlocked master keys for their lifetime and re-verify only when the descriptor
changes. Key commands run with a minimal environment, key files must belong to the
effective user or root, and printable 32-byte raw keys are refused. Manifest replay
remains outside the threat model (§2.3). This implementation does not establish FIPS operational
compliance or protect against privileged access to a running process.

Engine validation passes 131 encryption-enabled tests and 35 focused crypto tests with
the minimal filesystem feature set. Trusted-server acceptance passes 73 encrypted server
backup tests (including 17 encryption cases) and 56 default server backup tests. The
offline-policy additions pass 17 encrypted CLI tests, 15 default CLI tests and 20 server
policy tests. Fresh-catalog repository identity validation passes nine focused facade
policy tests on each default and encrypted build. Strict Clippy passes for the relevant
engine, facade and server targets; default/minimal engine checks also pass. Coverage
includes published crypto vectors,
authenticated corruption and identity/name swaps, wrong/missing keys, input bounds,
sealed caches, create/restore/verify/GC, durable initialization/rotation/retirement
boundaries, stale handles, cancellation, independent protected pages, malformed retirement
targets and unavailable offline recovery credentials. Native tests cover actual filesystem
rotation/retirement/reopen, concurrent rotations, configured alias retargeting, lost/expired
leases during preparation, dropped preparation cleanup, actual lease retention during
canceled/dropped post-rename sync, durable retry before rotation/retirement cleanup and
initializer cleanup that preserves a concurrently rotated marker. Identical local key bytes
recover historical/current epochs across file, environment, credential and command providers
without changing stored slot provenance. Independent review also exercises mixed-epoch reads,
retirement recovery and provider migration through public interfaces.

Remaining Phase 1 work includes Vault, authorized API provider/key-management workflows,
UI/Nix wiring, paper export and operator recovery workflows, metrics/audit, non-Linux
protected-memory backends, FIPS operational-mode evidence and representative performance
measurements.
Cloud KMS providers and all later phases remain unimplemented. No complete Phase 1
claim is made.
