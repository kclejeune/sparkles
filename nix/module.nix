# NixOS module: `services.sparkles` runs the Fuseki-compatible server, optionally behind
# nginx. Datasets can be declared here (`datasets.<name>`) and/or created at runtime
# through the admin API / web UI (those are kept in `dataDir/config.json`).
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.sparkles;
  inherit (lib)
    mkEnableOption
    mkOption
    mkIf
    mkPackageOption
    types
    ;

  json = pkgs.formats.json { };

  rateLimits = cfg.rateLimits;
  rateLimitsFile = "/etc/sparkles/rate-limits.json";

  modelSettingsFile = "/etc/sparkles/models.json";
  modelConfigFile = if cfg.models.settings != null then modelSettingsFile else cfg.models.configFile;
  modelSecretArgs = lib.concatLists (
    lib.mapAttrsToList (name: secret: [
      "--model-secret"
      "${name}=${if secret.file != null then "file:${secret.file}" else "env:${secret.environment}"}"
    ]) cfg.models.secrets
  );

  # behind the bundled nginx the client address comes from its X-Forwarded-For, for the
  # rate limits and for the limits of authentication (on whenever auth is): the server
  # trusts the addresses nginx connects from, or the Unix socket
  nginxPeers =
    if cfg.unixSocket != null then
      [ "unix" ]
    else
      lib.unique (
        [
          "127.0.0.1"
          "::1"
        ]
        # a specific local address is also the source nginx connects from
        ++ lib.optional (
          !lib.elem cfg.listenAddress [
            "0.0.0.0"
            "::"
            "localhost"
          ]
          && builtins.match "[0-9A-Fa-f.:]+" cfg.listenAddress != null
        ) cfg.listenAddress
      );

  # the load directory, and whether it lies where the service's hardening hides homes
  loadDir = if cfg.loadDir == null then null else lib.removeSuffix "/" (toString cfg.loadDir);
  loadDirInHome =
    loadDir != null
    && lib.any (h: loadDir == h || lib.hasPrefix "${h}/" loadDir) [
      "/home"
      "/root"
      "/run/user"
    ];

  datasetPath = name: ds: if ds.path != null then ds.path else "${cfg.dataDir}/declarative/${name}";

  validDatasetName =
    name:
    builtins.match "[A-Za-z0-9_.-]+" name != null
    && !lib.elem name [
      "ui"
      "."
      ".."
    ];

  # The settings file of `serve --settings`. The build runs the server's own
  # `sparkles settings check` on it, with the generated model configuration when there
  # is one, so a wrong value fails the build. The check reads only these two files.
  settingsFile = "/etc/sparkles/settings.json";
  settingsUnchecked = json.generate "sparkles-settings.json" cfg.settings;
  modelsSource = json.generate "sparkles-models.json" cfg.models.settings;
  settingsSource =
    if pkgs.stdenv.buildPlatform.canExecute pkgs.stdenv.hostPlatform then
      pkgs.runCommand "sparkles-settings.json" { } ''
        ${lib.getExe cfg.package} settings check ${settingsUnchecked} ${
          lib.optionalString (cfg.models.settings != null) "--model-config ${modelsSource}"
        }
        cp ${settingsUnchecked} $out
      ''
    else
      settingsUnchecked;
  settingsDatasets =
    if lib.isAttrs (cfg.settings.datasets or null) then lib.attrNames cfg.settings.datasets else [ ];

  # SIGHUP reloads the settings, the model configuration, the rate limits, the auth and
  # backup configurations and the TLS certificate. The server catches it once it has
  # opened its datasets, and before that the signal would stop it. A reload during the
  # start therefore waits until the main process catches SIGHUP.
  reloadScript = pkgs.writeShellScript "sparkles-reload" ''
    pid=$1
    for _ in $(${pkgs.coreutils}/bin/seq 800); do
      mask=$(${pkgs.gnused}/bin/sed -n 's/^SigCgt:[[:space:]]*//p' "/proc/$pid/status") || exit 1
      case $mask in
        *[13579bdfBDF]) exec ${pkgs.coreutils}/bin/kill -HUP "$pid" ;;
      esac
      ${pkgs.coreutils}/bin/sleep 0.1
    done
    echo "sparkles: the server does not catch SIGHUP yet, so it was not reloaded" >&2
    exit 1
  '';

  # the `compaction.auto` options that pass a value, and their flags
  autoCompactFlags = {
    minQuads = "--auto-compact-min-quads";
    ratio = "--auto-compact-ratio";
    maxQuads = "--auto-compact-max-quads";
    maxDeltaMb = "--auto-compact-max-delta-mb";
    maxWalMb = "--auto-compact-max-wal-mb";
    idleSeconds = "--auto-compact-idle";
    maxAgeSeconds = "--auto-compact-max-age";
    minIntervalSeconds = "--auto-compact-min-interval";
    threads = "--auto-compact-threads";
    ioMb = "--auto-compact-io-mb";
    maxRunning = "--auto-compact-max-running";
    partial = "--auto-compact-partial";
  };

  # an optional whole number: `null` keeps the server's default
  optionalInt =
    type: example: default: description:
    mkOption {
      type = types.nullOr type;
      default = null;
      inherit example;
      description = "${description} `null`: the server's default, ${default}.";
    };

  # directories of `fs` backup repositories (writable by the service)
  fsRoots = map (r: lib.removeSuffix "/" (toString r)) cfg.backup.fsRoots;
  dataDirSlash = "${lib.removeSuffix "/" (toString cfg.dataDir)}/";

  datasetArgs = lib.concatLists (
    lib.mapAttrsToList (
      name: ds:
      if ds.type == "mem" then
        [
          "--mem"
          name
        ]
      else
        [
          "--loc"
          "${name}=${datasetPath name ds}"
        ]
    ) cfg.datasets
  );

  args = [
    "--cache-mb"
    (toString cfg.cacheMb)
    "--result-cache-mb"
    (toString cfg.resultCacheMb)
  ]
  ++ lib.optional cfg.unionDefaultGraph "--union-default-graph"
  ++ [
    "serve"
    "--data"
    cfg.dataDir
    "--host"
    cfg.listenAddress
    "--port"
    (toString cfg.port)
    "--timeout"
    (toString cfg.queryTimeout)
    "--shutdown-grace"
    (toString cfg.shutdownGrace)
  ]
  ++ lib.optionals (cfg.auth.configFile != null) [
    "--auth-config"
    cfg.auth.configFile
  ]
  # always passed, so that declaring settings later reloads instead of restarting
  ++ [
    "--settings"
    settingsFile
  ]
  ++ lib.optionals (modelConfigFile != null) [
    "--model-config"
    modelConfigFile
  ]
  ++ modelSecretArgs
  ++ lib.optionals (cfg.backup.configFile != null) [
    "--backup-config"
    cfg.backup.configFile
  ]
  ++ lib.optionals (cfg.backup.maxTasks != null) [
    "--backup-max-tasks"
    (toString cfg.backup.maxTasks)
  ]
  ++ lib.optionals (cfg.maxTasks != null) [
    "--max-tasks"
    (toString cfg.maxTasks)
  ]
  ++ lib.optionals (cfg.maxClones != null) [
    "--max-clones"
    (toString cfg.maxClones)
  ]
  ++ lib.optional (!cfg.compaction.auto.enable) "--no-auto-compact"
  ++ lib.concatLists (
    lib.mapAttrsToList (
      name: flag:
      let
        v = cfg.compaction.auto.${name};
      in
      lib.optionals (v != null) [
        flag
        (toString v)
      ]
    ) autoCompactFlags
  )
  ++ lib.optionals (cfg.unixSocket != null) [
    "--unix-socket"
    cfg.unixSocket
  ]
  ++ lib.optionals tls [
    "--tls-cert"
    cfg.tls.certFile
    "--tls-key"
    cfg.tls.keyFile
  ]
  ++ lib.optional cfg.allowOpenNetwork "--allow-open-network"
  ++ lib.optionals (cfg.loadDir != null) [
    "--load-dir"
    loadDir
  ]
  # nginx forwards the name it was reached by: without auth the server answers only
  # IP addresses, localhost and --public-host names
  ++ lib.optionals cfg.nginx.enable (
    lib.concatMap
      (name: [
        "--public-host"
        name
      ])
      (
        [ cfg.nginx.virtualHost ]
        ++ config.services.nginx.virtualHosts.${cfg.nginx.virtualHost}.serverAliases
      )
  )
  ++ lib.optional cfg.readOnly "--read-only"
  ++ lib.optional (!cfg.allowService) "--no-service"
  ++ lib.optional cfg.mcp.enable "--mcp"
  ++ lib.optional (cfg.mcp.enable && cfg.mcp.allowUpdate) "--mcp-allow-update"
  ++ lib.optionals cfg.mcp.enable (
    lib.concatMap (d: [
      "--mcp-dataset"
      d
    ]) cfg.mcp.datasets
  )
  ++ lib.optional cfg.otel.enable "--otel"
  ++ lib.optional cfg.otel.logs "--otel-logs"
  ++ lib.optional cfg.otel.queryText "--otel-query-text"
  ++ lib.optional cfg.otel.planSpans "--otel-plan-spans"
  ++ lib.optional cfg.metrics.fusekiNames "--metrics-fuseki-names"
  ++ lib.optionals (cfg.metrics.listenAddress != null) [
    "--metrics-addr"
    cfg.metrics.listenAddress
  ]
  ++ lib.optionals (rateLimits != null) [
    "--rate-limit-config"
    rateLimitsFile
  ]
  ++ lib.optionals cfg.nginx.enable (
    lib.concatMap (peer: [
      "--rate-limit-trusted-proxy"
      peer
    ]) nginxPeers
  )
  ++ datasetArgs
  ++ cfg.extraArgs;

  persistent = lib.filterAttrs (_: ds: ds.type == "persistent") cfg.datasets;

  # directories the service may write to. `declarative/` is listed explicitly: an
  # intermediate directory auto-created by tmpfiles would be root-owned, and tmpfiles
  # refuses to create the dataset directory below it (unsafe path transition).
  writablePaths = lib.unique (
    [ cfg.dataDir ]
    ++ lib.optional (lib.any (ds: ds.path == null) (
      lib.attrValues persistent
    )) "${cfg.dataDir}/declarative"
    ++ lib.mapAttrsToList datasetPath persistent
    ++ fsRoots
  );

  tls = cfg.tls.certFile != null;

  upstream =
    if cfg.unixSocket != null then
      "http://unix:${cfg.unixSocket}"
    else
      let
        host =
          if cfg.listenAddress == "0.0.0.0" then
            "127.0.0.1"
          else if cfg.listenAddress == "::" then
            "[::1]"
          else if lib.hasInfix ":" cfg.listenAddress then
            "[${cfg.listenAddress}]"
          else
            cfg.listenAddress;
      in
      "${if tls then "https" else "http"}://${host}:${toString cfg.port}";
in
{
  options.services.sparkles = {
    enable = mkEnableOption "the Sparkles RDF / SPARQL server";

    package = mkPackageOption pkgs "sparkles" { };

    listenAddress = mkOption {
      type = types.str;
      default = "127.0.0.1";
      description = "Address the HTTP server binds to.";
    };

    allowOpenNetwork = mkOption {
      type = types.bool;
      default = false;
      description = ''
        Serve without {option}`auth.configFile` on a non-loopback {option}`listenAddress`
        (`--allow-open-network`). Every client that can reach the port may then read,
        write and administer every dataset; an authenticating proxy in front protects
        nothing if the port can be reached around it.
      '';
    };

    port = mkOption {
      type = types.port;
      default = 3030;
      description = "HTTP port (Fuseki's default).";
    };

    openFirewall = mkOption {
      type = types.bool;
      default = false;
      description = "Open {option}`port` in the firewall (not needed when proxied through nginx).";
    };

    dataDir = mkOption {
      type = types.path;
      default = "/var/lib/sparkles";
      description = ''
        State directory: the dataset registry (`config.json`), databases created through
        the admin API (`databases/`), declarative persistent datasets (`declarative/`),
        N-Quads backups (`backups/`) and the state of backup repositories (`backup/`).
      '';
    };

    user = mkOption {
      type = types.str;
      default = "sparkles";
      description = "User the server runs as (created when left at the default).";
    };

    group = mkOption {
      type = types.str;
      default = "sparkles";
      description = "Group the server runs as (created when left at the default).";
    };

    datasets = mkOption {
      default = { };
      example = lib.literalExpression ''
        {
          wiki = { };                                   # persistent, in dataDir/declarative/wiki
          scratch.type = "mem";
          archive.path = "/srv/rdf/archive";            # existing database directory
        }
      '';
      description = ''
        Datasets served at `/<name>` (`/<name>/sparql`, `/<name>/update`, `/<name>/data`, …).
        Persistent datasets survive restarts; `mem` datasets start empty on every start.
        Datasets can also be created at runtime through `/$/datasets` or the web UI.
      '';
      type = types.attrsOf (
        types.submodule {
          options = {
            type = mkOption {
              type = types.enum [
                "persistent"
                "mem"
              ];
              default = "persistent";
              description = "Storage: an on-disk database or an in-memory dataset.";
            };
            path = mkOption {
              type = types.nullOr types.path;
              default = null;
              description = "Database directory for a persistent dataset (default: `dataDir/declarative/<name>`).";
            };
          };
        }
      );
    };

    settings = mkOption {
      type = types.attrsOf json.type;
      default = { };
      example = lib.literalExpression ''
        {
          defaults = {
            assistant.enabled = true;
            prefixes = {
              kclj = "https://kclj.io/sparkles/";
              memory = "https://kclj.io/sparkles/memory/";
            };
            locked = [ "assistant.send" "prefixes.kclj" ];
          };
          datasets.slurp = {
            assistant = { ingest = true; send = "documents"; };
            memory.agentGraphs = [ "urn:x-sparkles:import/*" ];
          };
        }
      '';
      description = ''
        Declared dataset settings, in the format of the settings file that the
        Settings section of `docs/API.md` describes. `defaults` applies to every
        dataset, `datasets.<name>` to one dataset by name, and a `locked` list in
        either fixes fields so that they cannot be changed at runtime. The top-level
        `server.locked` list is accepted for locks of server-wide settings.

        `prefixes` declares namespace prefixes, as a map from a name to an IRI. The
        query editor and `sparkles lsp` complete them, results are written with them,
        and `GET /$/prefixes/{ds}` lists them. A dataset admin can add, change and
        remove prefixes at runtime unless a lock such as `prefixes.kclj` keeps them, and
        prefixes of loaded data never replace a declared one.

        The assistant stays off until a setting turns it on, even with
        {option}`models` configured. `defaults.assistant.enabled = true` turns it on
        for every dataset, including datasets created later through the API or the
        UI and in-memory datasets.

        A declared value is a default. A dataset admin can change any field that is
        not locked through the API, the CLI or the UI, and the change is kept across
        restarts and reloads. This option does not create datasets, and an entry for a
        name that matches no dataset applies as soon as such a dataset is created.

        The module generates `/etc/sparkles/settings.json` and passes it as
        `--settings`. A change reloads the service (SIGHUP) instead of restarting it.
        The build runs `sparkles settings check` on the file, together with the
        generated {option}`models.settings` when they are set, so a wrong value fails
        the build. The file must not contain `endpoint` or `apiKey` members, which
        belong in {option}`models`.
      '';
    };

    queryTimeout = mkOption {
      type = types.ints.positive;
      default = 60;
      description = "Default query timeout in seconds (clients may ask for another with `timeout=`, up to `--max-timeout`, 1800 s by default).";
    };

    shutdownGrace = mkOption {
      type = types.ints.unsigned;
      default = 20;
      description = ''
        Seconds that requests in flight get to finish when the service stops
        (`--shutdown-grace`). Requests still running after that are cancelled, and a
        cancelled write commits nothing. The unit's `TimeoutStopSec` is this plus 15
        seconds, which covers the cancellation and the final flush.
      '';
    };

    loadDir = mkOption {
      type = types.nullOr types.path;
      default = null;
      example = "/srv/rdf/import";
      description = ''
        Directory that `LOAD <file:…>` over HTTP may read from (`--load-dir`, for
        server-admin callers); without it such loads are refused. The service reads it
        read-only, so it must be readable by {option}`user`; it must not contain
        {option}`dataDir`, and must not be under `/tmp` or `/var/tmp` (the service has a
        private temporary directory).
      '';
    };

    readOnly = mkOption {
      type = types.bool;
      default = false;
      description = "Reject SPARQL Update, uploads, Graph Store writes and admin changes.";
    };

    allowService = mkOption {
      type = types.bool;
      default = true;
      description = "Allow federated `SERVICE` queries to other endpoints.";
    };

    mcp = {
      enable = mkEnableOption ''
        the Model Context Protocol endpoint `/$/mcp` for LLM agents (`--mcp`). Each call
        runs as the request's caller and sees only the datasets it may read'';

      allowUpdate = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Offer the `sparql_update` tool at `/$/mcp` (`--mcp-allow-update`) to callers that
          may write to a dataset. It has no effect with {option}`readOnly`.
        '';
      };

      datasets = mkOption {
        type = types.listOf types.str;
        default = [ ];
        example = [
          "wiki"
          "public-*"
        ];
        description = ''
          The datasets the MCP tools may see, by name or `*` pattern (`--mcp-dataset`).
          Empty (the default): all of them. Permissions still apply within them.
        '';
      };
    };

    cacheMb = mkOption {
      type = types.ints.unsigned;
      default = 1024;
      description = "Decoded index block cache size in MiB.";
    };

    resultCacheMb = mkOption {
      type = types.ints.unsigned;
      default = 512;
      description = "Query (sub)result cache size in MiB; 0 disables it.";
    };

    unionDefaultGraph = mkOption {
      type = types.bool;
      default = false;
      description = "Treat the default graph as the union of all named graphs (TDB2 `unionDefaultGraph`).";
    };

    auth.configFile = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "/run/secrets/sparkles-auth.toml";
      description = ''
        Authentication and per-dataset access control: the TOML file passed as
        `--auth-config` (users, API tokens, OIDC, trusted proxy headers; see
        `docs/API.md`). It holds password and token hashes and an OIDC client secret
        path, so it must not be in the Nix store: use an agenix or sops-nix secret owned
        by the service user. `systemctl reload sparkles` re-reads it. Without it the
        server is open.
      '';
    };

    models = {
      settings = mkOption {
        type = types.nullOr (types.attrsOf json.type);
        default = null;
        example = lib.literalExpression ''
          {
            providers.claude = {
              kind = "anthropic";
              endpoint = "https://api.anthropic.com";
              apiKey.secret = "anthropic";
            };
            roles.draft = [ { provider = "claude"; model = "claude-haiku-5-5"; } ];
          }
        '';
        description = ''
          Operator-controlled model providers, role lists and routing settings,
          using the JSON schema in `docs/API.md`, Model providers. The module
          generates `/etc/sparkles/models.json`, passes it as `--model-config`, and
          reloads the service (SIGHUP) when it changes, without a restart. Requests
          in flight keep the configuration they started with. The build checks
          {option}`settings` against it. Both the bare object with
          `providers`, `roles` and `routing` and the `models` wrapper are accepted
          by the server. Omitted fields retain the server's defaults.

          This JSON is stored in the Nix store. API keys must be named with
          `apiKey.secret` and supplied through {option}`models.secrets`;
          never put secret values in the settings. Use either this option or
          {option}`models.configFile`. `null` leaves model configuration unmanaged.
        '';
      };

      configFile = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "/etc/sparkles/custom-models.json";
        description = ''
          An existing JSON file passed as `--model-config`, as an alternative to
          {option}`models.settings`. It must be an absolute path readable by the
          service user. The file contains provider endpoints and credential names,
          not API keys. `systemctl reload sparkles` reads the file again. The build
          does not check {option}`settings` against this file.
        '';
      };

      secrets = mkOption {
        default = { };
        example = lib.literalExpression ''
          {
            anthropic.file = config.sops.secrets."anthropic-api-key".path;
            openai.environment = "OPENAI_API_KEY";
          }
        '';
        description = ''
          Credential sources keyed by the names used in providers' `apiKey.secret`.
          Each entry generates `--model-secret NAME=file:PATH` or
          `--model-secret NAME=env:VARIABLE`. Set exactly one source per entry.
          These options contain references only, never key values.
        '';
        type = types.attrsOf (
          types.submodule {
            options = {
              file = mkOption {
                type = types.nullOr types.str;
                default = null;
                example = "/run/secrets/anthropic-api-key";
                description = ''
                  An absolute credential file outside the Nix store, readable by
                  the service user, such as a sops-nix or agenix secret owned by
                  {option}`services.sparkles.user`. The server reads it at each
                  request, so rotating its contents requires no restart. The
                  service's sandbox hides home and temporary directories.
                '';
              };
              environment = mkOption {
                type = types.nullOr types.str;
                default = null;
                example = "ANTHROPIC_API_KEY";
                description = ''
                  The name of an environment variable holding the credential.
                  Supply its value at runtime, for example with
                  `systemd.services.sparkles.serviceConfig.EnvironmentFile`;
                  never put the value in Nix's service environment. Updating an
                  environment file requires restarting the service.
                '';
              };
            };
          }
        );
      };
    };

    backup = {
      configFile = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "/etc/sparkles/backup.toml";
        description = ''
          Backup repositories, lifecycle policies, credential sources and the limits of
          repositories registered through the API: the TOML file passed as
          `--backup-config` (see `docs/API.md`, Backup repositories). It names credential
          files and environment variables rather than holding secrets, but like
          {option}`auth.configFile` it must not be in the Nix store: keep it readable by
          the service user only (agenix, sops-nix, or an `environment.etc` entry with a
          `mode`). `systemctl reload sparkles` re-reads it. `fs` repositories may not
          lie in its directory or in {option}`dataDir`; list their directories in
          {option}`backup.fsRoots`. Without it repositories can still be registered
          through the API (`fs` ones only, in a directory the service may write to: one of
          {option}`backup.fsRoots`).
        '';
      };

      maxTasks = mkOption {
        type = types.nullOr types.ints.positive;
        default = null;
        example = 1;
        description = ''
          Backup, restore, verification and GC tasks running at once
          (`--backup-max-tasks`; `null`: the server's default, 2). Up to 4 more per
          slot wait queued.
        '';
      };

      fsRoots = mkOption {
        type = types.listOf types.str;
        default = [ ];
        example = [ "/srv/backups/sparkles" ];
        description = ''
          Directories that hold `fs` backup repositories, from {option}`backup.configFile`
          or registered through the API: created (owned by {option}`user`, mode 0750) and
          made writable for the service, which sees the rest of the file system
          read-only. They must be absolute, outside {option}`dataDir`, and not under
          `/tmp`, `/var/tmp` or a home directory. To hold API registrations to them, also
          set `[api] fs_roots` in the config file.
        '';
      };
    };

    maxTasks =
      optionalInt types.ints.unsigned 2 "4"
        "Background tasks that run at once (`--max-tasks`): compaction, clones, reasoning, full-text, spatial and vector index builds, and N-Quads backups. More wait, queued. `0`: no limit.";

    maxClones =
      optionalInt types.ints.unsigned 1 "2"
        "Clones that run at once, within {option}`maxTasks` (`--max-clones`). More wait, queued. `0`: only {option}`maxTasks` limits them.";

    compaction.auto = {
      enable = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Compact datasets automatically when their policy says so. `false` passes
          `--no-auto-compact`, which turns it off for every dataset. Manual compaction
          (`POST /$/compact/{ds}`) still works. The other options of this group give the
          server-wide policy, which a dataset's own settings (`PUT /$/compaction/{ds}`)
          override. See the Automatic compaction section of `docs/USAGE.md`.
        '';
      };

      minQuads =
        optionalInt types.ints.unsigned 50000 "10000"
          "The floor: the size and idle triggers need a delta of at least this many quads (`--auto-compact-min-quads`).";

      ratio = mkOption {
        type = types.nullOr (types.either types.ints.unsigned types.float);
        default = null;
        example = 0.1;
        description = ''
          Compact when the delta reaches the floor plus this share of the base index's
          quads (`--auto-compact-ratio`, from 0 to 1000). `null`: the server's default,
          0.05.
        '';
      };

      maxQuads =
        optionalInt types.ints.unsigned 5000000 "1000000"
          "Compact at this delta size, whatever the base (`--auto-compact-max-quads`). `0`: no limit.";

      maxDeltaMb =
        optionalInt types.ints.unsigned 1024 "512"
          "Compact when the delta takes about this many MiB of memory (`--auto-compact-max-delta-mb`). `0`: no limit.";

      maxWalMb =
        optionalInt types.ints.unsigned 4096 "1024"
          "Compact when the write-ahead log passes this many MiB (`--auto-compact-max-wal-mb`). `0`: no limit.";

      idleSeconds =
        optionalInt types.ints.unsigned 600 "300"
          "Compact a delta of at least the floor after this many seconds without a commit (`--auto-compact-idle`). `0` turns the trigger off.";

      maxAgeSeconds =
        optionalInt types.ints.unsigned 3600 "86400"
          "Compact when the oldest change not yet compacted is this many seconds old (`--auto-compact-max-age`). `0` turns the trigger off.";

      minIntervalSeconds =
        optionalInt types.ints.unsigned 300 "60"
          "Seconds between the end of a compaction and the start of the next automatic one (`--auto-compact-min-interval`).";

      threads =
        optionalInt types.ints.positive 2 "a quarter of the cores"
          "Threads of an automatic compaction's build, which run at nice 10 (`--auto-compact-threads`).";

      ioMb =
        optionalInt types.ints.unsigned 100 "0"
          "The average MiB per second at which an automatic compaction may write its new index (`--auto-compact-io-mb`). `0`: no limit.";

      maxRunning =
        optionalInt types.ints.positive 2 "1"
          "Automatic compactions that may run on the server at once (`--auto-compact-max-running`).";

      partial = mkOption {
        type = types.nullOr (
          types.enum [
            "auto"
            "off"
            "always"
          ]
        );
        default = null;
        example = "off";
        description = "Whether a compaction, automatic or not, may rewrite only the index blocks its delta touches (`--auto-compact-partial`). `null`: the server's default, `auto`.";
      };
    };

    unixSocket = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "/run/sparkles/sparkles.sock";
      description = ''
        Listen on this Unix socket (mode 0660, group {option}`group`) instead of TCP. The
        nginx virtual host then proxies to it, and nginx joins the group. With
        authentication, trusted proxy headers can be limited to the socket
        (`proxy.trusted = [ "unix" ]`). A socket under `/run/sparkles` uses the service's
        runtime directory.
      '';
    };

    tls = {
      certFile = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "/var/lib/acme/sparql.example.org/fullchain.pem";
        description = ''
          Serve HTTPS natively with this PEM certificate chain (`--tls-cert`; with
          {option}`tls.keyFile`). HTTP/2 and HTTP/1.1 are negotiated through ALPN.
          `systemctl reload sparkles` re-reads the files, and the server also notices
          when they change, so a certificate renewed by `security.acme` needs no
          restart. The service user must be able to read both files: for an ACME
          certificate, add it to the certificate's group, e.g.
          `users.users.sparkles.extraGroups = [ "acme" ]`. Most deployments let a
          reverse proxy terminate TLS instead (see {option}`nginx.enable`). With both,
          nginx connects to the server over https.
        '';
      };

      keyFile = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "/var/lib/acme/sparql.example.org/key.pem";
        description = ''
          The PEM private key of {option}`tls.certFile` (`--tls-key`). It must not be in
          the Nix store.
        '';
      };
    };

    logLevel = mkOption {
      type = types.str;
      default = "sparkles=info,sparkles_server=info,tower_http=warn";
      description = "`RUST_LOG` filter for the service.";
    };

    metrics = {
      fusekiNames = mkEnableOption "Fuseki's Prometheus metric names on `/$/metrics`, next to the Sparkles names";

      listenAddress = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "127.0.0.1:9464";
        description = ''
          Also serve `/$/metrics` on this `HOST:PORT`, with the same authentication as the
          main listener. Without {option}`auth.configFile`, an address that is not
          loopback needs {option}`allowOpenNetwork`. The firewall is not opened for it.
        '';
      };
    };

    otel = {
      enable = mkEnableOption "OpenTelemetry export of traces and metrics over OTLP";

      endpoint = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "http://127.0.0.1:4318";
        description = "Collector address (`OTEL_EXPORTER_OTLP_ENDPOINT`); `null`: the OTLP default.";
      };

      protocol = mkOption {
        type = types.enum [
          "http/protobuf"
          "grpc"
        ];
        default = "http/protobuf";
        description = "OTLP transport (`OTEL_EXPORTER_OTLP_PROTOCOL`).";
      };

      logs = mkEnableOption "export of log events over OTLP";

      queryText = mkEnableOption "query text and plan descriptions in spans (they may hold data)";

      planSpans = mkEnableOption "one span per executed plan operator";

      environment = mkOption {
        type = types.attrsOf types.str;
        default = { };
        example = {
          OTEL_TRACES_SAMPLER = "parentbased_traceidratio";
          OTEL_TRACES_SAMPLER_ARG = "0.1";
          OTEL_RESOURCE_ATTRIBUTES = "deployment.environment.name=prod";
        };
        description = "Further `OTEL_*` variables for the service.";
      };
    };

    rateLimits = mkOption {
      type = types.nullOr json.type;
      default = null;
      example = lib.literalExpression ''
        {
          classes = {
            auth = { rate = "10/min"; burst = 5; failureCost = 3; };
            query = { rate = "100/s"; burst = 200; concurrency = 64; clientConcurrency = 8; };
            update = { rate = "10/s"; };
          };
        }
      '';
      description = ''
        Rate and concurrency limits per request class (`auth`, `query`, `update`,
        `admin`), written to ${rateLimitsFile} and passed as `--rate-limit-config`
        (see the Rate limiting section of `docs/API.md`). Changing it reloads the service
        (SIGHUP) instead of restarting it. `null` (the default): no limits, except the
        failed-authentication budget that is on whenever {option}`auth.configFile` is set.
        With `nginx.enable` the server trusts nginx (the loopback addresses, or the Unix
        socket) to name the client in `X-Forwarded-For`, whether or not this is set.
      '';
    };

    extraArgs = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "Extra arguments appended to `sparkles serve`.";
    };

    installCli = mkOption {
      type = types.bool;
      default = true;
      description = ''
        Put the `sparkles` CLI on the system path. Note that a database directory is locked
        while the server has it open; use the HTTP API for served datasets, and the CLI for
        offline work (bulk loads, compaction) while the service is stopped.
      '';
    };

    nginx = {
      enable = mkEnableOption "an nginx reverse proxy virtual host for the server";

      virtualHost = mkOption {
        type = types.str;
        example = "sparql.example.org";
        description = ''
          Name of the nginx virtual host that proxies to the server. Extend it through
          `services.nginx.virtualHosts.<name>` as usual, e.g. `enableACME = true;
          forceSSL = true;`. Do not set `basicAuthFile` together with
          {option}`auth.configFile`: nginx would forward its own `Authorization` header,
          which the server would then reject.
          The server must be served at the root of the host (the UI lives at `/ui/`, the
          admin API at `/$/`).
        '';
      };

      clientMaxBodySize = mkOption {
        type = types.str;
        default = "4g";
        description = "Maximum request body size (bulk uploads, Graph Store PUT/POST).";
      };

      proxyTimeout = mkOption {
        type = types.ints.positive;
        default = cfg.queryTimeout + 30;
        defaultText = lib.literalExpression "config.services.sparkles.queryTimeout + 30";
        description = "nginx proxy read/send timeout in seconds.";
      };
    };
  };

  config = mkIf cfg.enable {
    assertions = [
      {
        assertion = lib.all validDatasetName (lib.attrNames cfg.datasets);
        message = "services.sparkles.datasets: names must match [A-Za-z0-9_.-]+ and must not be `ui`, `.` or `..`.";
      }
      {
        assertion = lib.all validDatasetName settingsDatasets;
        message = "services.sparkles.settings.datasets: names must match [A-Za-z0-9_.-]+ and must not be `ui`, `.` or `..`.";
      }
      {
        assertion = cfg.models.settings == null || cfg.models.configFile == null;
        message = "services.sparkles.models: set either settings or configFile, not both.";
      }
      {
        assertion = cfg.models.configFile == null || lib.hasPrefix "/" cfg.models.configFile;
        message = "services.sparkles.models.configFile must be an absolute path.";
      }
      {
        assertion = lib.all (name: name != "" && !lib.hasInfix "=" name) (lib.attrNames cfg.models.secrets);
        message = "services.sparkles.models.secrets: names must be nonempty and must not contain `=`.";
      }
      {
        assertion = lib.all (s: (s.file != null) != (s.environment != null)) (
          lib.attrValues cfg.models.secrets
        );
        message = "services.sparkles.models.secrets: set exactly one of file or environment for each credential.";
      }
      {
        assertion = lib.all (
          s: s.file == null || (lib.hasPrefix "/" s.file && !lib.hasPrefix "/nix/store" s.file)
        ) (lib.attrValues cfg.models.secrets);
        message = "services.sparkles.models.secrets: credential files must be absolute paths outside the Nix store.";
      }
      {
        assertion = lib.all (
          s: s.environment == null || builtins.match "[A-Za-z_][A-Za-z0-9_]*" s.environment != null
        ) (lib.attrValues cfg.models.secrets);
        message = "services.sparkles.models.secrets: environment must name a valid environment variable.";
      }
      {
        assertion =
          cfg.auth.configFile != null
          || cfg.allowOpenNetwork
          || cfg.unixSocket != null
          || lib.elem cfg.listenAddress [
            "127.0.0.1"
            "::1"
            "localhost"
          ];
        message = "services.sparkles: listenAddress ${cfg.listenAddress} is not loopback; set auth.configFile, or allowOpenNetwork = true to serve it without authentication.";
      }
      {
        assertion =
          cfg.auth.configFile == null
          || (lib.hasPrefix "/" cfg.auth.configFile && !lib.hasPrefix "/nix/store" cfg.auth.configFile);
        message = "services.sparkles.auth.configFile must be an absolute path outside the Nix store (it holds secrets).";
      }
      {
        assertion = (cfg.tls.certFile == null) == (cfg.tls.keyFile == null);
        message = "services.sparkles: set both tls.certFile and tls.keyFile, or neither.";
      }
      {
        assertion = !tls || cfg.unixSocket == null;
        message = "services.sparkles: tls applies to the TCP listener, not to unixSocket.";
      }
      {
        assertion =
          cfg.tls.keyFile == null
          || (lib.hasPrefix "/" cfg.tls.keyFile && !lib.hasPrefix "/nix/store" cfg.tls.keyFile);
        message = "services.sparkles.tls.keyFile must be an absolute path outside the Nix store (it is a secret).";
      }
      {
        assertion =
          loadDir == null
          || (
            lib.hasPrefix "/" loadDir
            && loadDir != "/"
            && !lib.hasPrefix "${loadDir}/" "${lib.removeSuffix "/" (toString cfg.dataDir)}/"
          );
        message = "services.sparkles.loadDir must be an absolute directory that does not contain dataDir.";
      }
      {
        assertion =
          loadDir == null
          || !lib.any (t: loadDir == t || lib.hasPrefix "${t}/" loadDir) [
            "/tmp"
            "/var/tmp"
          ];
        message = "services.sparkles.loadDir must not be under /tmp or /var/tmp: the service has a private temporary directory.";
      }
      {
        assertion =
          cfg.backup.configFile == null
          || (lib.hasPrefix "/" cfg.backup.configFile && !lib.hasPrefix "/nix/store" cfg.backup.configFile);
        message = "services.sparkles.backup.configFile must be an absolute path outside the Nix store.";
      }
      {
        assertion = lib.all (
          r:
          lib.hasPrefix "/" r
          && r != ""
          && !lib.hasPrefix "${r}/" dataDirSlash
          && !lib.hasPrefix dataDirSlash "${r}/"
          && !lib.any (t: r == t || lib.hasPrefix "${t}/" r) [
            "/tmp"
            "/var/tmp"
            "/home"
            "/root"
            "/run/user"
          ]
        ) fsRoots;
        message = "services.sparkles.backup.fsRoots must be absolute directories outside dataDir, and not under /tmp, /var/tmp or a home directory.";
      }
    ];

    users.users = lib.mkMerge [
      (mkIf (cfg.user == "sparkles") {
        sparkles = {
          isSystemUser = true;
          group = cfg.group;
          home = cfg.dataDir;
          description = "Sparkles RDF server";
        };
      })
      # nginx reaches the Unix socket through the service group
      (mkIf (cfg.nginx.enable && cfg.unixSocket != null) {
        nginx.extraGroups = [ cfg.group ];
      })
    ];
    users.groups = mkIf (cfg.group == "sparkles") { sparkles = { }; };

    environment.systemPackages = lib.optional cfg.installCli cfg.package;

    environment.etc."sparkles/rate-limits.json" = mkIf (rateLimits != null) {
      source = json.generate "sparkles-rate-limits.json" rateLimits;
    };

    environment.etc."sparkles/models.json" = mkIf (cfg.models.settings != null) {
      source = modelsSource;
    };

    environment.etc."sparkles/settings.json".source = settingsSource;

    networking.firewall.allowedTCPPorts = lib.optional cfg.openFirewall cfg.port;

    systemd.tmpfiles.settings."10-sparkles" = lib.listToAttrs (
      map (p: {
        name = p;
        value.d = {
          user = cfg.user;
          group = cfg.group;
          mode = "0750";
        };
      }) writablePaths
    );

    systemd.services.sparkles = {
      description = "Sparkles RDF / SPARQL server";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      environment = {
        RUST_LOG = cfg.logLevel;
      }
      // lib.optionalAttrs cfg.otel.enable (
        {
          OTEL_EXPORTER_OTLP_PROTOCOL = cfg.otel.protocol;
        }
        // lib.optionalAttrs (cfg.otel.endpoint != null) {
          OTEL_EXPORTER_OTLP_ENDPOINT = cfg.otel.endpoint;
        }
        // cfg.otel.environment
      );
      # the server reads these files again on SIGHUP, so a change reloads it
      reloadTriggers = [
        config.environment.etc."sparkles/settings.json".source
      ]
      ++ lib.optional (cfg.models.settings != null) config.environment.etc."sparkles/models.json".source
      ++ lib.optional (rateLimits != null) config.environment.etc."sparkles/rate-limits.json".source;
      serviceConfig = {
        # re-reads the settings, the model configuration, the rate-limit, auth and backup
        # configurations and the TLS certificate
        ExecReload = "${reloadScript} $MAINPID";
        ExecStart = lib.escapeShellArgs ([ (lib.getExe cfg.package) ] ++ args);
        RuntimeDirectory = mkIf (
          cfg.unixSocket != null && lib.hasPrefix "/run/sparkles/" cfg.unixSocket
        ) "sparkles";
        RuntimeDirectoryMode = "0750";
        User = cfg.user;
        Group = cfg.group;
        WorkingDirectory = cfg.dataDir;
        StateDirectory = mkIf (cfg.dataDir == "/var/lib/sparkles") "sparkles";
        StateDirectoryMode = "0750";
        ReadWritePaths = writablePaths;
        ReadOnlyPaths = lib.optional (loadDir != null) loadDir;
        Restart = "on-failure";
        RestartSec = 5;
        # the grace period for requests in flight, then up to 5 s for cancelled ones to
        # stop and the final flush (commits are durable either way)
        TimeoutStopSec = cfg.shutdownGrace + 15;
        LimitNOFILE = 65536;

        # hardening
        NoNewPrivileges = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectSystem = "strict";
        # a load directory under a home stays readable
        ProtectHome = if loadDirInHome then "read-only" else true;
        ProtectHostname = true;
        ProtectClock = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectKernelLogs = true;
        ProtectControlGroups = true;
        ProtectProc = "invisible";
        ProcSubset = "pid";
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        RemoveIPC = true;
        CapabilityBoundingSet = "";
        AmbientCapabilities = "";
        SystemCallArchitectures = "native";
        SystemCallFilter = [
          "@system-service"
          "~@privileged"
          "~@resources"
        ];
        UMask = "0027";
      };
    };

    services.nginx = mkIf cfg.nginx.enable {
      enable = true;
      recommendedProxySettings = lib.mkDefault true;
      virtualHosts.${cfg.nginx.virtualHost} = {
        locations."/" = {
          proxyPass = upstream;
          # the recommended headers follow, except that X-Forwarded-For is overwritten
          recommendedProxySettings = false;
          extraConfig = ''
            proxy_set_header Host $host;
            proxy_set_header X-Real-IP $remote_addr;
            # the server trusts this header from nginx and counts failed logins and rate
            # limits by it, so it names the peer instead of appending to what the client
            # sent (behind another proxy, configure the realip module so that
            # $remote_addr is the client)
            proxy_set_header X-Forwarded-For $remote_addr;
            proxy_set_header X-Forwarded-Proto $scheme;
            proxy_set_header X-Forwarded-Host $host;
            proxy_set_header X-Forwarded-Server $hostname;
            # a client's own Forwarded header never reaches the server
            proxy_set_header Forwarded "";
            client_max_body_size ${cfg.nginx.clientMaxBodySize};
            proxy_read_timeout ${toString cfg.nginx.proxyTimeout}s;
            proxy_send_timeout ${toString cfg.nginx.proxyTimeout}s;
            # stream large results / uploads instead of spooling them to disk
            proxy_request_buffering off;
            proxy_buffering off;
          '';
        };
      };
    };
  };
}
