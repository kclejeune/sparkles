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

  # behind the bundled nginx, the client address comes from X-Forwarded-For
  rateLimits =
    if cfg.rateLimits == null then
      null
    else
      cfg.rateLimits
      // lib.optionalAttrs cfg.nginx.enable {
        trustedProxies = lib.unique (
          (cfg.rateLimits.trustedProxies or [ ])
          ++ [
            "127.0.0.1"
            "::1"
          ]
        );
      };
  rateLimitsFile = "/etc/sparkles/rate-limits.json";

  datasetPath = name: ds: if ds.path != null then ds.path else "${cfg.dataDir}/declarative/${name}";

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
  ]
  ++ lib.optionals (cfg.auth.configFile != null) [
    "--auth-config"
    cfg.auth.configFile
  ]
  ++ lib.optionals (cfg.unixSocket != null) [
    "--unix-socket"
    cfg.unixSocket
  ]
  ++ lib.optional cfg.allowOpenNetwork "--allow-open-network"
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
  ++ lib.optional cfg.otel.enable "--otel"
  ++ lib.optional cfg.otel.logs "--otel-logs"
  ++ lib.optional cfg.otel.queryText "--otel-query-text"
  ++ lib.optional cfg.otel.planSpans "--otel-plan-spans"
  ++ lib.optionals (rateLimits != null) [
    "--rate-limit-config"
    rateLimitsFile
  ]
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
  );

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
      "http://${host}:${toString cfg.port}";
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
        the admin API (`databases/`), declarative persistent datasets (`declarative/`)
        and backups (`backups/`).
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

    queryTimeout = mkOption {
      type = types.ints.positive;
      default = 60;
      description = "Default query timeout in seconds (clients may ask for another with `timeout=`, up to `--max-timeout`, 1800 s by default).";
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

    logLevel = mkOption {
      type = types.str;
      default = "sparkles=info,sparkles_server=info,tower_http=warn";
      description = "`RUST_LOG` filter for the service.";
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
        (SIGHUP) instead of restarting it. With `nginx.enable`, the loopback addresses
        are added to `trustedProxies`. `null` (the default): no limits.
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
        assertion = lib.all (name: builtins.match "[A-Za-z0-9_.-]+" name != null && name != "ui") (
          lib.attrNames cfg.datasets
        );
        message = "services.sparkles.datasets: names must match [A-Za-z0-9_.-]+ and must not be `ui`.";
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
      reloadTriggers = lib.optional (
        rateLimits != null
      ) config.environment.etc."sparkles/rate-limits.json".source;
      serviceConfig = {
        # re-reads the rate-limit and auth configurations (without either, SIGHUP would
        # stop the server)
        ExecReload = mkIf (
          rateLimits != null || cfg.auth.configFile != null
        ) "${pkgs.coreutils}/bin/kill -HUP $MAINPID";
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
        Restart = "on-failure";
        RestartSec = 5;
        # graceful shutdown flushes nothing extra (commits are durable), but give
        # in-flight requests a moment
        TimeoutStopSec = 30;
        LimitNOFILE = 65536;

        # hardening
        NoNewPrivileges = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectSystem = "strict";
        ProtectHome = true;
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
          recommendedProxySettings = true;
          extraConfig = ''
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
