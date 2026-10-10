# Home Manager module: `programs.sparkles`, the client side of Sparkles for one user.
#
# What the CLI reads per user, and what this module does with it:
#
# - `$XDG_CONFIG_HOME/sparkles/memory.toml` is the configuration of `sparkles memory`
#   (server, dataset, skip-projects, transcripts, redact, claude-dir, codex-dir). The
#   module writes it from `memory.server`, `memory.dataset` and `memory.settings`.
# - `$XDG_CONFIG_HOME/sparkles/backup.toml` names backup repositories, credentials
#   sources and policies for `sparkles repo` and `sparkles backup`. The module writes it
#   from `backup.settings`. `SPARKLES_BACKUP_CONFIG` or `--backup-config` replace it.
# - `$XDG_CONFIG_HOME/sparkles/credentials.toml` holds the tokens that
#   `sparkles auth login` saves, with the default server of the login. The CLI rewrites
#   it, and it is secret, so the module never writes it. `SPARKLES_TOKEN` replaces the
#   saved token, and `SPARKLES_SERVER` sets the server of most remote commands.
# - `.sparklesfmt.toml` is found by walking up from each file, so one in the home
#   directory applies to every file below it that has no nearer one. The module writes
#   it from `fmt.settings`, with the lint severities under `[lint]`.
# - `SPARKLES_MEMORY_DATASET` replaces the dataset of memory.toml for one shell.
# - The package installs bash, zsh and fish completions and the man pages, which Home
#   Manager's shell modules and `programs.man` pick up from the profile.
# - `sparkles memory setup` prints Claude Code and Codex hooks and a skill. The module
#   declares the same through `programs.claude-code` and `programs.codex`.
{
  config,
  lib,
  options,
  pkgs,
  ...
}:
let
  inherit (lib) mkOption types;
  cfg = config.programs.sparkles;
  toml = pkgs.formats.toml { };

  memoryConfig = lib.recursiveUpdate (lib.filterAttrs (_: v: v != null) {
    inherit (cfg.memory) server dataset;
  }) cfg.memory.settings;
  writeMemory = cfg.memory.dataset != null || cfg.memory.settings != { };

  hooks = cfg.memory.hooks;
  hasHarness = h: lib.elem h hooks.harnesses;

  # The hooks of `sparkles memory setup HARNESS [--brief] [--transcripts]`, which the
  # flake's `home-module` check compares with the CLI's own output.
  briefHook = harness: [
    {
      matcher = "startup|resume|clear|compact";
      hooks = [
        {
          type = "command";
          timeout = 10;
          command = "sparkles memory brief --hook ${harness} --if-reachable";
        }
      ];
    }
  ];
  endHook = harness: [
    {
      hooks = [
        {
          type = "command";
          timeout = 10;
          command =
            "sparkles memory sync --from-hook ${harness} --if-reachable --quiet --detach"
            + lib.optionalString hooks.transcripts " --transcripts";
        }
      ];
    }
  ];
  hookEvents = {
    claude-code = {
      SessionStart = briefHook "claude-code";
    }
    // lib.optionalAttrs (!hooks.briefOnly) {
      PostToolUse = [
        {
          matcher = "Write|Edit|MultiEdit";
          hooks = [
            {
              type = "command";
              async = true;
              command = "sparkles memory sync --from-hook claude-code --if-reachable --quiet";
            }
          ];
        }
      ];
      SessionEnd = endHook "claude-code";
    };
    codex = {
      SessionStart = briefHook "codex";
    }
    // lib.optionalAttrs (!hooks.briefOnly) {
      Stop = [
        {
          hooks = [
            {
              type = "command";
              timeout = 10;
              command = "sparkles memory sync --from-hook codex --instructions-only --if-reachable --quiet --detach";
            }
          ];
        }
      ];
      SessionEnd = endHook "codex";
    };
  };

  # The extraction skill, as the installed CLI prints it.
  skill = pkgs.runCommand "sparkles-memory-extract-skill" { nativeBuildInputs = [ pkgs.jq ]; } ''
    export HOME="$TMPDIR" XDG_CONFIG_HOME="$TMPDIR/config"
    ${lib.getExe cfg.package} memory setup claude-code --json | jq -jr .skill.text > $out
  '';
  withSkill = !hooks.briefOnly;

  harnessModule = {
    claude-code = "programs.claude-code";
    codex = "programs.codex";
  };
in
{
  _class = "homeManager";

  options.programs.sparkles = {
    enable = lib.mkEnableOption "the Sparkles command-line client";

    package = lib.mkPackageOption pkgs "sparkles" {
      extraDescription = ''
        The flake's `homeModules.default` sets this to the flake's own build for the
        host. Its `sparkles-cli` package is the same binary without the web UI.
      '';
    };

    server = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "https://sparkles.example.lan";
      description = ''
        The default server of the client configuration files that take one. The CLI
        has no general default-server file, so this feeds {option}`memory.server` and,
        with the `codex` harness, the URL of the Codex MCP entry. Other remote commands
        take `--server`, `SPARKLES_SERVER` or the default server that
        `sparkles auth login` saves.
      '';
    };

    memory = {
      server = mkOption {
        type = types.nullOr types.str;
        default = cfg.server;
        defaultText = lib.literalExpression "config.programs.sparkles.server";
        description = "The server of `sparkles memory`, written as `server` in memory.toml.";
      };

      dataset = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "slurp";
        description = ''
          The dataset that `sparkles memory` imports into and reads from, written as
          `dataset` in memory.toml. memory.toml is written only when this or
          {option}`memory.settings` is set.
        '';
      };

      settings = mkOption {
        inherit (toml) type;
        default = { };
        example = lib.literalExpression ''
          {
            skip-projects = [ "github.com/acme/secret" ];
            transcripts = [ "github.com/acme/shop" ];
            redact = [ { name = "internal-token"; regex = "itk_[A-Za-z0-9]{32}"; } ];
          }
        '';
        description = ''
          Further keys of {file}`$XDG_CONFIG_HOME/sparkles/memory.toml`, merged over
          `server` and `dataset`. The keys are described in the "Agent memory"
          section of docs/USAGE.md. New keys need no change to this module.
        '';
      };

      hooks = {
        harnesses = mkOption {
          type = types.listOf (
            types.enum [
              "claude-code"
              "codex"
            ]
          );
          default = [ ];
          example = [ "claude-code" ];
          description = ''
            Coding agents that get the memory hooks and the extraction skill of
            `sparkles memory setup`, through Home Manager's `programs.claude-code` and
            `programs.codex` modules. The hooks sync memory files as the agent writes
            them, sync the project when a session ends and print the brief when a
            session starts. They exit 0 when the server cannot be reached. With
            `codex` and a {option}`memory.server`, the Sparkles MCP server is added to
            Codex's configuration as well.
          '';
        };

        briefOnly = mkOption {
          type = types.bool;
          default = false;
          description = "Install only the session start hook that prints the brief, without the skill.";
        };

        transcripts = mkOption {
          type = types.bool;
          default = false;
          description = ''
            Import the session's transcript when a session ends. The dataset and the
            project must allow transcripts as well.
          '';
        };
      };
    };

    backup.settings = mkOption {
      inherit (toml) type;
      default = { };
      example = lib.literalExpression ''
        {
          version = 1;
          repositories.local = { type = "fs"; path = "/srv/backups/r"; };
          repositories.lab = {
            type = "s3";
            bucket = "lab";
            credentials = { source = "named"; name = "lab"; };
          };
          credentials.lab = {
            source = "env";
            access_key_id_var = "LAB_ACCESS_KEY";
            secret_access_key_var = "LAB_SECRET_KEY";
          };
        }
      '';
      description = ''
        The backup config file {file}`$XDG_CONFIG_HOME/sparkles/backup.toml` of
        `sparkles repo` and `sparkles backup`. It names repositories, policies and where
        credentials and keys come from, such as environment variables or files. It holds
        references to secrets and never the secrets themselves, because the file is
        copied into the world-readable Nix store. A config managed here is read-only, so
        `sparkles repo add` and `repo remove` cannot edit it. The CLI warns that the file
        is readable by others, which is expected for a file in the store.
      '';
    };

    fmt.settings = mkOption {
      inherit (toml) type;
      default = { };
      example = lib.literalExpression ''
        {
          line-width = 80;
          lint.unused-prefix = "error";
        }
      '';
      description = ''
        The options of `sparkles fmt`, `sparkles lint` and `sparkles lsp`, written to
        {file}`~/.sparklesfmt.toml`. The CLI finds the nearest `.sparklesfmt.toml` or
        `sparklesfmt.toml` above each file, so this one applies to files under the home
        directory that have no nearer config. Files are never merged, so a project's own
        config replaces this one completely. The `lint` table sets rule severities.
      '';
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        # The package brings its completions and man pages along. Home Manager's bash,
        # zsh and fish modules load completions from the profile when their completion
        # support is on, and `programs.man` indexes the profile's man pages.
        home.packages = [ cfg.package ];

        xdg.configFile."sparkles/memory.toml" = lib.mkIf writeMemory {
          source = toml.generate "sparkles-memory.toml" memoryConfig;
        };
        xdg.configFile."sparkles/backup.toml" = lib.mkIf (cfg.backup.settings != { }) {
          source = toml.generate "sparkles-backup.toml" cfg.backup.settings;
        };
        home.file.".sparklesfmt.toml" = lib.mkIf (cfg.fmt.settings != { }) {
          source = toml.generate "sparklesfmt.toml" cfg.fmt.settings;
        };

        assertions = map (h: {
          assertion = !(hasHarness h) || lib.hasAttrByPath (lib.splitString "." harnessModule.${h}) options;
          message = "programs.sparkles.memory.hooks.harnesses contains \"${h}\", but this Home Manager has no ${harnessModule.${h}} module.";
        }) (lib.attrNames harnessModule);

        warnings = lib.concatMap (
          h:
          lib.optional
            (
              hasHarness h
              && !(lib.attrByPath (lib.splitString "." harnessModule.${h} ++ [ "enable" ]) false config)
            )
            "programs.sparkles.memory.hooks.harnesses contains \"${h}\", but ${harnessModule.${h}}.enable is false, so its hooks are not written."
        ) (lib.attrNames harnessModule);
      }

      (lib.optionalAttrs (options ? programs.claude-code) {
        programs.claude-code = lib.mkIf (hasHarness "claude-code") {
          settings.hooks = hookEvents.claude-code;
          skills = lib.mkIf withSkill { sparkles-memory-extract = skill; };
        };
      })

      (lib.optionalAttrs (options ? programs.codex) {
        programs.codex = lib.mkIf (hasHarness "codex") {
          hooks = hookEvents.codex;
          skills = lib.mkIf withSkill { sparkles-memory-extract = skill; };
          settings.mcp_servers.sparkles = lib.mkIf (withSkill && cfg.memory.server != null) {
            command = "sparkles";
            args = [
              "mcp"
              "--url"
              cfg.memory.server
            ];
          };
        };
      })
    ]
  );
}
