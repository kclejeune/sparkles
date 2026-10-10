# Model settings, runtime credential references and module validation without a VM.
{ pkgs, sparkles }:
let
  evaluate =
    extra:
    (import "${pkgs.path}/nixos/lib/eval-config.nix" {
      system = pkgs.stdenv.hostPlatform.system;
      modules = [
        ./module.nix
        {
          nixpkgs.pkgs = pkgs;
          system.stateVersion = "26.05";
          boot.isContainer = true;
          services.sparkles = {
            enable = true;
            package = sparkles;
          };
        }
        extra
      ];
    }).config;
  failures = config: builtins.filter (a: !a.assertion) config.assertions;
  haiku = {
    provider = "claude";
    model = "claude-haiku-5-5";
  };
  sonnet = {
    provider = "claude";
    model = "claude-sonnet-5-5";
    maxOutputTokens = 8192;
  };
  settings = {
    providers = {
      claude = {
        kind = "anthropic";
        endpoint = "https://api.anthropic.com";
        apiKey.secret = "anthropic";
        contextTokens = 100000;
        allowedModels = [
          haiku.model
          sonnet.model
        ];
        models.${sonnet.model}.pricing = {
          inputPerMTok = 2.0;
          outputPerMTok = 10.0;
        };
      };
      openai = {
        kind = "openai";
        endpoint = "https://api.openai.com/v1";
        apiKey.secret = "openai";
      };
      local = {
        kind = "ollama";
        endpoint = "http://127.0.0.1:11434";
      };
      # an internal CA named by path, never inlined
      internal = {
        kind = "openai";
        endpoint = "https://llm.internal.example/v1";
        tls.caCert.file = "/etc/ssl/certs/internal-ca.pem";
      };
    };
    roles = {
      draft = [
        haiku
        sonnet
      ];
      repair = [ sonnet ];
      summarize = [ haiku ];
      extract = [ sonnet ];
      explain = [ haiku ];
      optimize = [ sonnet ];
    };
    routing.complexityThreshold = 6;
  };
  configured = evaluate {
    services.sparkles.models = {
      inherit settings;
      secrets = {
        anthropic.file = "/run/secrets/Claude key's file";
        openai.environment = "OPENAI_API_KEY";
      };
    };
  };
  wrapped = evaluate { services.sparkles.models.settings.models = settings; };
  external = evaluate {
    services.sparkles.models = {
      configFile = "/etc/custom model's config.json";
      secrets.anthropic.file = "/run/secrets/anthropic";
    };
  };
  plain = evaluate { };
  invalid = map (models: evaluate { services.sparkles.models = models; }) [
    {
      inherit settings;
      configFile = "/etc/conflict.json";
    }
    { configFile = "relative.json"; }
    { secrets.missing = { }; }
    {
      secrets.both = {
        file = "/run/key";
        environment = "KEY";
      };
    }
    { secrets."".file = "/run/key"; }
    { secrets."bad=name".file = "/run/key"; }
    { secrets.relative.file = "key"; }
    { secrets.store.file = "/nix/store/secret"; }
    { secrets.empty.environment = ""; }
    { secrets.bad.environment = "BAD-NAME"; }
  ];
  fixture = pkgs.writeText "sparkles-models-test.json" (
    builtins.toJSON {
      inherit settings;
      source = configured.environment.etc."sparkles/models.json".source;
      wrappedSource = wrapped.environment.etc."sparkles/models.json".source;
      command = configured.systemd.services.sparkles.serviceConfig.ExecStart;
      externalCommand = external.systemd.services.sparkles.serviceConfig.ExecStart;
      settingsSource = configured.environment.etc."sparkles/settings.json".source;
      triggers = map toString configured.systemd.services.sparkles.reloadTriggers;
      externalTriggers = map toString external.systemd.services.sparkles.reloadTriggers;
      externalSettings = external.environment.etc."sparkles/settings.json".source;
      reload = configured.systemd.services.sparkles.serviceConfig.ExecReload;
      plainCommand = plain.systemd.services.sparkles.serviceConfig.ExecStart;
    }
  );
in
assert failures configured == [ ];
assert failures wrapped == [ ];
assert failures external == [ ];
assert failures plain == [ ];
assert builtins.all (config: failures config != [ ]) invalid;
assert !(plain.environment.etc ? "sparkles/models.json");
assert !(external.environment.etc ? "sparkles/models.json");
# a change of the model settings reloads the server instead of restarting it
assert configured.systemd.services.sparkles.restartTriggers == [ ];
assert external.systemd.services.sparkles.restartTriggers == [ ];
assert plain.systemd.services.sparkles.restartTriggers == [ ];
pkgs.runCommand "sparkles-models-test" { nativeBuildInputs = [ pkgs.python3 ]; } ''
  python3 - ${fixture} <<'PY'
  import json
  import pathlib
  import shlex
  import sys

  fixture = json.loads(pathlib.Path(sys.argv[1]).read_text())
  assert json.loads(pathlib.Path(fixture["source"]).read_text()) == fixture["settings"]
  assert json.loads(pathlib.Path(fixture["wrappedSource"]).read_text()) == {"models": fixture["settings"]}
  assert fixture["triggers"] == [fixture["settingsSource"], fixture["source"]], fixture["triggers"]
  assert fixture["externalTriggers"] == [fixture["externalSettings"]], fixture["externalTriggers"]
  assert fixture["reload"].endswith(" $MAINPID"), fixture["reload"]

  def values(command, flag):
      args = shlex.split(command)
      return [args[i + 1] for i, arg in enumerate(args) if arg == flag]

  assert values(fixture["command"], "--model-config") == ["/etc/sparkles/models.json"]
  assert values(fixture["command"], "--model-secret") == [
      "anthropic=file:/run/secrets/Claude key's file", "openai=env:OPENAI_API_KEY"
  ]
  assert values(fixture["externalCommand"], "--model-config") == ["/etc/custom model's config.json"]
  assert values(fixture["externalCommand"], "--model-secret") == ["anthropic=file:/run/secrets/anthropic"]
  assert values(fixture["plainCommand"], "--model-config") == []
  assert values(fixture["plainCommand"], "--model-secret") == []
  PY
  touch "$out"
''
