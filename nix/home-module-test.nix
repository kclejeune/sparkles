# The Home Manager module without a VM: the files it generates for a test user, checked
# for their content and read back by the CLI. Home Manager is pinned here, fetched at
# evaluation time, rather than declared as a flake input, so that consumers of
# `homeModules.default` never lock or follow it. To update the pin, run
#   nix-prefetch-url --unpack https://github.com/nix-community/home-manager/archive/REV.tar.gz
# and replace `rev` and `sha256`.
{ pkgs, sparkles }:
let
  inherit (pkgs) lib;
  rev = "c58f54c733a7894d84fa2fdff26cc12088c4b78c";
  homeManager = builtins.fetchTarball {
    url = "https://github.com/nix-community/home-manager/archive/${rev}.tar.gz";
    sha256 = "1g0nfxg6dk3g5xh1vgdv0yl551cb1n3s6wqn97sljmbbkw1v9cni";
  };

  evaluate =
    extra:
    import "${homeManager}/modules" {
      inherit pkgs;
      configuration = {
        imports = [
          ./home-module.nix
          extra
        ];
        home = {
          username = "test";
          homeDirectory = "/home/test";
          stateVersion = "26.05";
        };
        manual.manpages.enable = false;
        programs.sparkles = {
          enable = true;
          package = sparkles;
        };
      };
    };

  full = evaluate {
    programs.sparkles = {
      server = "https://sparkles.example.lan";
      memory = {
        dataset = "slurp";
        settings = {
          skip-projects = [ "github.com/acme/secret" ];
          redact = [
            {
              name = "internal-token";
              regex = "itk_[A-Za-z0-9]{32}";
            }
          ];
        };
        hooks.harnesses = [
          "claude-code"
          "codex"
        ];
      };
      backup.settings = {
        version = 1;
        repositories.local = {
          type = "fs";
          path = "/srv/backups/r";
        };
      };
      fmt.settings = {
        line-width = 80;
        lint.unused-prefix = "error";
      };
    };
    programs.claude-code = {
      enable = true;
      package = null;
    };
    programs.codex = {
      enable = true;
      package = null;
    };
  };

  brief = evaluate {
    programs.sparkles.memory.hooks = {
      harnesses = [ "claude-code" ];
      briefOnly = true;
      transcripts = true;
    };
    programs.claude-code = {
      enable = true;
      package = null;
    };
  };

  # A server alone writes no memory.toml, and memory.server follows it.
  serverOnly = evaluate { programs.sparkles.server = "https://sparkles.example.lan"; };

  failures = hm: builtins.filter (a: !a.assertion) hm.config.assertions;

  evalChecks =
    assert failures full == [ ];
    assert failures brief == [ ];
    assert !(serverOnly.config.xdg.configFile ? "sparkles/memory.toml");
    assert serverOnly.config.programs.sparkles.memory.server == "https://sparkles.example.lan";
    assert !(serverOnly.config.home.file ? ".sparklesfmt.toml");
    true;

  expectedMemory = builtins.toJSON {
    server = "https://sparkles.example.lan";
    dataset = "slurp";
    skip-projects = [ "github.com/acme/secret" ];
    redact = [
      {
        name = "internal-token";
        regex = "itk_[A-Za-z0-9]{32}";
      }
    ];
  };
in
assert evalChecks;
pkgs.runCommand "sparkles-home-module"
  {
    nativeBuildInputs = [
      sparkles
      pkgs.jq
      pkgs.python3
    ];
    files = full.config.home-files;
    briefFiles = brief.config.home-files;
    inherit expectedMemory;
  }
  ''
    export HOME="$TMPDIR/home"
    mkdir -p "$HOME"
    export XDG_CONFIG_HOME="$files/.config"
    fail() { echo "home-module: $*" >&2; exit 1; }
    # the hooks in `want` and `got` are equal and not empty
    same() { jq -e .SessionStart want > /dev/null && diff -u want got; }

    # memory.toml is server and dataset with memory.settings merged over them
    python3 -c '
    import json, sys, tomllib
    got = tomllib.load(open(sys.argv[1], "rb"))
    want = json.loads(sys.argv[2])
    if got != want:
        sys.exit(f"memory.toml: {got} != {want}")
    ' "$files/.config/sparkles/memory.toml" "$expectedMemory"

    # The CLI reads memory.toml: setup reports the server it names
    sparkles memory setup codex --json > codex.json || fail "memory setup rejected memory.toml"
    jq -e '.mcp | contains("https://sparkles.example.lan")' codex.json > /dev/null \
      || fail "memory setup did not read the server of memory.toml"

    # The hooks and the skill match what the CLI's own setup prints
    sparkles memory setup claude-code --json > claude.json
    jq -S .hooks.hooks claude.json > want
    jq -S .hooks "$files/.claude/settings.json" > got
    same || fail "Claude Code hooks differ from sparkles memory setup claude-code"
    jq -S .hooks.hooks codex.json > want
    jq -S .hooks "$files/.codex/hooks.json" > got
    same || fail "Codex hooks differ from sparkles memory setup codex"
    jq -jr .skill.text claude.json > want
    diff -u want "$files/.claude/skills/sparkles-memory-extract/SKILL.md" \
      || fail "the Claude Code skill differs"
    grep -q 'sparkles-memory-extract' "$files"/.codex/skills/sparkles-memory-extract/SKILL.md \
      || fail "the Codex skill is missing"

    sparkles memory setup claude-code --brief --transcripts --json | jq -S .hooks.hooks > want
    jq -S .hooks "$briefFiles/.claude/settings.json" > got
    same || fail "brief-only hooks differ"
    [ ! -e "$briefFiles/.claude/skills/sparkles-memory-extract" ] \
      || fail "brief-only hooks installed the skill"
    [ ! -e "$briefFiles/.config/sparkles/memory.toml" ] || fail "memory.toml without a dataset"

    # The CLI reads backup.toml
    sparkles repo list --json > repos.json || fail "repo list rejected backup.toml"
    grep -q '"local"' repos.json || fail "repo list does not show the repository of backup.toml"

    # The formatter and the linter read ~/.sparklesfmt.toml
    cp "$files/.sparklesfmt.toml" "$HOME/.sparklesfmt.toml"
    cd "$HOME"
    printf 'PREFIX ex: <http://example.org/>\nSELECT * WHERE { ?s ?p ?o }\n' > q.rq
    sparkles fmt --stdin-filepath q.rq < q.rq > /dev/null || fail "fmt rejected .sparklesfmt.toml"
    if sparkles lint q.rq > lint.txt 2>&1; then
      cat lint.txt
      fail "lint did not raise unused-prefix to an error"
    fi
    grep -q 'unused-prefix' lint.txt || { cat lint.txt; fail "lint did not report unused-prefix"; }

    touch $out
  ''
