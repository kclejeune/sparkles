# The module's declared settings without a VM: the generated settings file, its flags
# and reload triggers, the name assertions, and the build-time check that refuses a
# wrong value.
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
            dataDir = "/data/server";
          };
        }
        extra
      ];
    }).config;
  failures = config: builtins.filter (a: !a.assertion) config.assertions;
  service = config: config.systemd.services.sparkles;
  settingsSource = config: config.environment.etc."sparkles/settings.json".source;

  models = {
    providers.claude = {
      kind = "anthropic";
      endpoint = "https://api.anthropic.com";
      apiKey.secret = "anthropic";
    };
    roles.draft = [
      {
        provider = "claude";
        model = "claude-haiku-5-5";
      }
    ];
  };
  settings = {
    defaults = {
      assistant = {
        enabled = true;
        send = "schema";
      };
      locked = [ "assistant.send" ];
    };
    datasets = {
      slurp = {
        assistant = {
          ingest = true;
          send = "documents";
          sendByProvider.claude = "rows";
        };
        memory = {
          agentGraphs = [ "urn:x-sparkles:import/*" ];
          imports = {
            base = "urn:x-sparkles:import/";
            extract = "server";
          };
        };
      };
      scratch.assistant.historyDays = 7;
    };
    server.locked = [ "models.routing" ];
  };

  configured = evaluate {
    services.sparkles = {
      datasets = {
        demo = { };
        scratch.type = "mem";
      };
      inherit settings;
      models = {
        settings = models;
        secrets.anthropic.file = "/run/secrets/anthropic";
      };
    };
  };
  # a second module's lock adds to the first one's
  merged = evaluate {
    imports = [
      { services.sparkles.settings.defaults.locked = [ "assistant.send" ]; }
      { services.sparkles.settings.defaults.locked = [ "assistant.enabled" ]; }
    ];
  };
  plain = evaluate { };
  invalidNames = map (name: evaluate { services.sparkles.settings.datasets.${name} = { }; }) [
    "ui"
    "."
    ".."
    "bad/name"
  ];

  # a wrong value fails the build of the settings file, and so of the system
  refused =
    name: extra:
    let
      check = pkgs.testers.testBuildFailure (settingsSource (evaluate extra));
    in
    pkgs.runCommand "sparkles-settings-refused-${name}" { } ''
      grep -q 'settings file' ${check}/testBuildFailure.log
      cp ${check}/testBuildFailure.log $out
    '';
  refusals = {
    send = refused "send" { services.sparkles.settings.defaults.assistant.send = "everything"; };
    kind = refused "kind" { services.sparkles.settings.datasets.demo.nosuchkind = { }; };
    endpoint = refused "endpoint" {
      services.sparkles.settings.datasets.demo.assistant.endpoint = "https://example.org";
    };
    lock = refused "lock" { services.sparkles.settings.defaults.locked = [ "" ]; };
    # with model settings, a provider the model configuration lacks is refused too
    provider = refused "provider" {
      services.sparkles = {
        models.settings = models;
        settings.defaults.assistant.sendByProvider.nosuch = "schema";
      };
    };
    # a server lock whose path names no field of the models kind
    serverLock = refused "server-lock" {
      services.sparkles = {
        models.settings = models;
        settings.server.locked = [ "models.providers.claude.endpont" ];
      };
    };
  };
  # without model settings, the check does not know the providers and accepts it
  unchecked = evaluate {
    services.sparkles.settings.defaults.assistant.sendByProvider.nosuch = "schema";
  };

  fixture = pkgs.writeText "sparkles-settings-test.json" (
    builtins.toJSON {
      inherit settings;
      source = settingsSource configured;
      models = configured.environment.etc."sparkles/models.json".source;
      merged = settingsSource merged;
      plain = settingsSource plain;
      unchecked = settingsSource unchecked;
      command = (service configured).serviceConfig.ExecStart;
      plainCommand = (service plain).serviceConfig.ExecStart;
      reload = (service configured).serviceConfig.ExecReload;
      plainReload = (service plain).serviceConfig.ExecReload;
      triggers = map toString (service configured).reloadTriggers;
      plainTriggers = map toString (service plain).reloadTriggers;
      paths = builtins.attrNames configured.systemd.tmpfiles.settings."10-sparkles";
      refusals = builtins.attrValues refusals;
    }
  );
in
assert failures configured == [ ];
assert failures merged == [ ];
assert failures plain == [ ];
assert builtins.all (config: failures config != [ ]) invalidNames;
assert (service configured).restartTriggers == [ ];
assert (service plain).restartTriggers == [ ];
assert (service configured).preStart == "";
pkgs.runCommand "sparkles-settings-test"
  {
    nativeBuildInputs = [
      pkgs.python3
      pkgs.shellcheck
    ];
  }
  ''
    python3 - ${fixture} <<'PY'
    import json
    import pathlib
    import shlex
    import subprocess
    import sys

    fixture = json.loads(pathlib.Path(sys.argv[1]).read_text())

    def read(path):
        return json.loads(pathlib.Path(path).read_text())

    assert read(fixture["source"]) == fixture["settings"]
    assert read(fixture["plain"]) == {}
    assert read(fixture["unchecked"]) == {
        "defaults": {"assistant": {"sendByProvider": {"nosuch": "schema"}}}
    }
    assert sorted(read(fixture["merged"])["defaults"]["locked"]) == [
        "assistant.enabled", "assistant.send"
    ]

    def values(command, flag):
        args = shlex.split(command)
        return [args[i + 1] for i, arg in enumerate(args) if arg == flag]

    # the flag names the /etc path, so a change of the settings leaves ExecStart alone
    for command in (fixture["command"], fixture["plainCommand"]):
        assert values(command, "--settings") == ["/etc/sparkles/settings.json"], command
    assert values(fixture["command"], "--model-config") == ["/etc/sparkles/models.json"]
    assert fixture["triggers"] == [fixture["source"], fixture["models"]], fixture["triggers"]
    assert fixture["plainTriggers"] == [fixture["plain"]], fixture["plainTriggers"]
    # the module no longer writes into dataset directories
    assert all("/databases/" not in path for path in fixture["paths"]), fixture["paths"]

    # SIGHUP is always handled: the reload waits for the server to catch it
    for reload in (fixture["reload"], fixture["plainReload"]):
        script, arg = reload.split()
        assert arg == "$MAINPID", reload
        subprocess.run(["shellcheck", script], check=True)
    PY
    for log in ${toString (builtins.attrValues refusals)}; do
      echo "== $log"; cat "$log"
    done
    touch "$out"
  ''
