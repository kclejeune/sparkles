# Backup repositories

This record preserves design sources, licensing and adoption decisions at the
stages recorded below. Delivery statements in the record are historical. Current
capabilities and remaining work are described by the spec status and Outcome.

See the [spec landing page](README.md), [design](SPEC.md) and
[delivery record](OUTCOME.md) for their respective contracts.

- **Spec:** [`F05-snapshot-repositories.md`](README.md), written on
  2026-09-30 independently from:
  - the Sparkles code and specs (CI and F06 in full; C09 §§3–4, 7, 9);
  - Elasticsearch and Kibana public user documentation on snapshot and restore, as
    evidence of desired behaviour only. Both products are source-available under
    non-permissive licenses, and no source code was read;
  - the `object_store` 0.14.2 docs (MIT/Apache-2.0), the AWS S3 User Guide on
    conditional writes and consistency, restic's design references (BSD-2-Clause, ideas
    only) and the `croner` 4.0.0 docs (MIT), all fetched;
  - `chrono-tz`, `lz4_flex`, `age`, BLAKE3, S3 limits, the borg chunker (BSD-3-Clause),
    `s3s-fs`, the MinIO license, RFCs 9110, 3339 and 9562 and the IANA time zone
    database, cited from general knowledge.
- **Implementation:** done (2026-09-30/10-01), from the spec, the implementation plan
  and the Sparkles code only. Where the plan and the spec differ, the plan wins. The
  implementation consists of:
  - the `sparkles-backup` crate, with the repository engine and policies as pure
    functions;
  - the engine's capture, lease, `reidentify` and `restoredFrom` support in `sparkles`;
  - the server's `backup` module, behind the default-on `backup` feature of
    `sparkles-server`. It holds the registry, task slots, HTTP routes, in-place restore
    and startup recovery, the policy scheduler and metrics;
  - the `sparkles repo` / `sparkles backup` commands, the UI's Backups page,
    `docs/API.md` ("Backup repositories") and `scripts/backup-bench.sh`.

  As the maintainer decided, the delivered Phase 1 is the spec's Phase 1 plus policies,
  GC and the full UI.
  - **Dependencies.** Versions come from `Cargo.lock`. Licenses come from each crate's
    `Cargo.toml` and were checked against its shipped license files.
    - `object_store` 0.14.2 (`MIT/Apache-2.0`), with `default-features = false` and the
      features `tokio`, `fs` and `aws`. These map to the crate features `fs` and `s3` of
      `sparkles-backup`, both on by default. `gcp` and `azure` stay behind the
      off-by-default `gcs` and `azure` features. The crate reuses the workspace's
      reqwest 0.13, hyper 1 and aws-lc-rs/rustls. The published crate ships only the
      Apache-2.0 text (`LICENSE.txt`) and the ASF `NOTICE.txt`. A binary distributed
      under the Apache-2.0 option must carry that NOTICE ("Apache Arrow Object Store,
      Copyright 2020-2026 The Apache Software Foundation"). This is not a blocker.
    - `croner` 4.0.0 (MIT; `LICENSE.md`), with `derive_builder` 0.20.2 (MIT OR
      Apache-2.0), `strum` 0.27.2 and `strum_macros` 0.27.2 (MIT). `derive_builder` uses
      `darling` 0.20.11 (MIT), a build-time proc macro. `croner` builds with chrono
      0.4.45, so the planned fallback parser was not needed.
    - `chrono-tz` 0.10.4 (MIT OR Apache-2.0), with `phf` / `phf_shared` 0.12.1 (MIT) and
      `siphasher` 1.0.4 (MIT OR Apache-2.0). The bundled IANA tz data is public domain.
      There is no `chrono-tz-build` step, because the 0.10 tables are pre-generated.
    - New through `object_store`:
      - `crc-fast` 1.10.0 (MIT OR Apache-2.0), pure Rust with no build script, with
        `digest` 0.10.7 and `crypto-common` 0.1.7 (MIT OR Apache-2.0), `generic-array`
        0.14.7 (MIT) and `spin` 0.10.1 (MIT);
      - `humantime` 2.4.0 (MIT OR Apache-2.0);
      - `itertools` 0.15.0 (MIT OR Apache-2.0), a second version next to 0.14.0;
      - `nix` 0.31.3 (MIT);
      - `quick-xml` 0.41.0 (MIT), next to 0.37.5;
      - `wasm-streams` 0.5.0 (MIT OR Apache-2.0), for wasm targets only and never built
        here.
    - Reused, already in the workspace: `sha2`, `lz4_flex`, `futures`, `bytes`,
      `async-trait`, `percent-encoding`, `tokio`, `uuid`, `chrono`, `serde_json`, `toml`.
    - Nothing is non-permissive. Every new crate is MIT, Apache-2.0 or both. MinIO
      (AGPL-3.0) is only an external test process, and the S3 tests run only with
      `SPARKLES_TEST_S3_ENDPOINT`. `age`, for Phase 3 encryption, is not a dependency
      yet.
  - **Deviations from the spec.** The plan and the implementation made these decisions:
    - Leases are keyed by generation, not by commit (`Lease { generation, label }`).
      After a compaction, the new generation's base covers the captured commit, so a
      lease by commit would not have kept the old generation. History GC keeps a leased
      generation unconditionally and shows it as `backup:<name>`.
    - `validation.json` and `validation-shapes.ttl` are backed up as meta files, and the
      restore path grammar accepts them. The spec's file list omitted them, and clones
      copy them as well.
    - Repositories registered through the API are restricted beyond the spec:
      - S3 credentials are named only (`{"source": "named", "name": …}`) and resolved
        from the config file's `[credentials.<name>]` tables. The caller can never
        choose `env`, `file` or `default`.
      - S3 endpoints, and every address their host resolves to, are under the server's
        outbound policy. A custom DNS resolver pins connections to the checked
        addresses.
      - `fs` paths must lie under `[api] fs_roots` when it is set, and outside the
        directories of the server's config files.
      - `gcs` and `azure` repositories come from the config file only.
      - Endpoint URLs refuse userinfo, queries and fragments.
    - Blob header byte 5 is the codec (0 raw, 1 LZ4, 2 zstd reserved), and byte 6 is
      the encryption (0 none). The format is still 1.
    - During an in-place restore, a router layer answers requests for the dataset with
      `503 dataset-restoring` and `Retry-After: 5`. The spec's check in `dataset()`
      could not guarantee "never 404".
    - Server-wide tasks have `dataset: ""` and are listed for `server-admin` only.
    - `verified` comes from a server-local `<data>/backup/verify.json`, and
      `Repository.lastGc` from `gc/last.json`.
    - `<data>/sparkles-server.lock` allows one server per data directory and guards
      `sparkles backup restore --data`.
    - Schedules use croner in naive local time with a DST mapping written for Sparkles.
      Skipped times run after the gap, and repeated times run once. `every <duration>`
      is aligned to the epoch in UTC.
    - At most 4 tasks wait per `--backup-max-tasks` slot. Beyond that, requests get
      `503 too-many-tasks`.
    - Some parts are not built as specified:
      - `507 insufficient-storage` is defined, but no restore checks free space.
      - `sparkles_backup_object_requests_total` counts only the successful requests
        that verification and GC reports carry (`result="ok"`), not every request with
        `ok|error`.
      - In-memory datasets answer `501 backup-unsupported`.
      - `sparkles backup policy run` (offline) is refused until Phase 2.
- **Rejected** (spec §8):
  - N-Quads dumps as the backup format;
  - a tar of the database directory per backup;
  - mirroring Elasticsearch's repository layout, blob naming or REST paths;
  - "snapshots" as the name, or `/$/snapshots/…` paths (F06 uses them);
  - `/$/repositories/{repo}/backups/{name}` for per-dataset operations;
  - content-defined chunking and pack files in Phase 1;
  - multipart uploads;
  - BLAKE3 (SHA-256 is already in the workspace);
  - a mutable repository index object;
  - client-clock lock staleness;
  - always keeping, or always replacing, the dataset id on restore;
  - backing up the Tantivy index;
  - blocking writes for the whole backup;
  - filesystem snapshots or rsync;
  - inline secrets in the API or UI in Phase 1.
