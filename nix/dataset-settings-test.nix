# Module evaluation and startup-script checks without building the server or a VM.
{ pkgs }:
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
            package = pkgs.hello;
            dataDir = "/data/server";
          };
        }
        extra
      ];
    }).config;
  failures = config: builtins.filter (a: !a.assertion) config.assertions;
  assistant = {
    enabled = true;
    ingest = true;
    send = "documents";
  };
  memory = {
    agentGraphs = [ "urn:x-sparkles:import/*" ];
    imports = {
      base = "urn:x-sparkles:import/";
      extract = "server";
    };
  };
  configured = evaluate {
    services.sparkles = {
      datasets = {
        demo = { };
        archive.path = "/data/custom archive's database";
      };
      datasetSettings = {
        demo = { inherit assistant memory; };
        archive = { inherit assistant; };
        slurp = { inherit assistant; };
        future = { inherit assistant; };
        unmanaged = { };
      };
    };
  };
  plain = evaluate { };
  invalidNames =
    map (name: evaluate { services.sparkles.datasetSettings.${name}.assistant = assistant; })
      [
        "ui"
        "."
        ".."
        "bad/name"
      ];
  memoryDataset = evaluate {
    services.sparkles = {
      datasets.scratch.type = "mem";
      datasetSettings.scratch = { inherit assistant; };
    };
  };
  fixture = pkgs.writeText "sparkles-dataset-settings-test.json" (
    builtins.toJSON {
      inherit assistant memory;
      script = configured.systemd.services.sparkles.preStart;
      triggers = map toString configured.systemd.services.sparkles.restartTriggers;
      paths = builtins.attrNames configured.systemd.tmpfiles.settings."10-sparkles";
      user = configured.systemd.services.sparkles.serviceConfig.User;
    }
  );
in
assert failures configured == [ ];
assert plain.systemd.services.sparkles.preStart == "";
assert plain.systemd.services.sparkles.restartTriggers == [ ];
assert builtins.all (config: failures config != [ ]) invalidNames;
assert failures memoryDataset != [ ];
pkgs.runCommand "sparkles-dataset-settings-test"
  {
    nativeBuildInputs = [
      pkgs.python3
      pkgs.bash
      pkgs.shellcheck
    ];
  }
  ''
    python3 - ${fixture} <<'PY'
    import json
    import pathlib
    import subprocess
    import sys
    import tempfile

    fixture = json.loads(pathlib.Path(sys.argv[1]).read_text())
    assert fixture["user"] == "sparkles"
    assert len(fixture["triggers"]) == 5
    assert all("/databases/" not in path for path in fixture["paths"])

    with tempfile.TemporaryDirectory() as root:
        root = pathlib.Path(root)
        declared = root / "server/declarative/demo"
        custom = root / "custom archive's database"
        runtime = root / "server/databases/slurp"
        future = root / "server/databases/future"
        declared.mkdir(parents=True)
        custom.mkdir()
        runtime.mkdir(parents=True)
        # An uninitialized runtime directory is skipped too.
        (runtime / "assistant.json").write_text('{"enabled":false}')
        script = fixture["script"].replace("/data/", str(root) + "/")
        script_path = root / "pre-start.sh"
        script_path.write_text("#!/usr/bin/env bash\n" + script)
        subprocess.run(["shellcheck", str(script_path)], check=True)

        def start():
            subprocess.run(["bash", "-e", str(script_path)], check=True)

        def check_file(directory, name, expected):
            path = directory / name
            assert json.loads(path.read_text()) == expected
            assert path.stat().st_mode & 0o777 == 0o600

        start()
        check_file(declared, "assistant.json", fixture["assistant"])
        check_file(declared, "memory.json", fixture["memory"])
        check_file(custom, "assistant.json", fixture["assistant"])
        assert json.loads((runtime / "assistant.json").read_text()) == {"enabled": False}
        assert not future.exists()
        assert not (root / "server/databases/unmanaged").exists()

        (runtime / "CURRENT").write_text("gen-00000001")
        start()
        check_file(runtime, "assistant.json", fixture["assistant"])
        # Unspecified memory settings stay unmanaged.
        assert not (runtime / "memory.json").exists()
        (declared / "assistant.json").write_text('{"enabled":false}')
        start()
        check_file(declared, "assistant.json", fixture["assistant"])
        assert not future.exists()
    PY
    touch "$out"
  ''
