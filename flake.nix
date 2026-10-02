{
  description = "Sparkles: high-performance RDF / SPARQL database (Jena/Fuseki compatible, QLever-style index)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{ self, flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      flake = {
        # `services.sparkles`; the package defaults to this flake's build for the host.
        nixosModules.default =
          { lib, pkgs, ... }:
          {
            imports = [ ./nix/module.nix ];
            services.sparkles.package = lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.sparkles;
          };
        nixosModules.sparkles = self.nixosModules.default;

        # Adds `sparkles`, `sparkles-cli`, `sparkles-ui` and `sparkles-fmt-wasm` to a package set.
        overlays.default =
          final: prev:
          let
            pkgs = final.extend inputs.rust-overlay.overlays.default;
            toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
            rustPlatform = pkgs.makeRustPlatform {
              cargo = toolchain;
              rustc = toolchain;
            };
            fmtWasm = final.callPackage ./nix/fmt-wasm.nix { inherit rustPlatform; };
            ui = final.callPackage ./nix/ui.nix { inherit fmtWasm; };
          in
          {
            sparkles-fmt-wasm = fmtWasm;
            sparkles-ui = ui;
            sparkles = final.callPackage ./nix/package.nix { inherit rustPlatform ui; };
            sparkles-cli = final.callPackage ./nix/package.nix {
              inherit rustPlatform;
              ui = null;
            };
          };
      };

      perSystem =
        {
          system,
          pkgs,
          lib,
          self',
          ...
        }:
        let
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = toolchain;
            rustc = toolchain;
          };
          # Chromium for the Playwright UI tests (the dev shell and the `ui-e2e` check)
          playwrightBrowsers = pkgs.playwright-driver.browsers.override {
            withFirefox = false;
            withWebkit = false;
            withFfmpeg = false;
          };
        in
        {
          _module.args.pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [ inputs.rust-overlay.overlays.default ];
          };

          packages = {
            # the formatter for the browser (WebAssembly), built into the UI
            sparkles-fmt-wasm = pkgs.callPackage ./nix/fmt-wasm.nix { inherit rustPlatform; };
            # the web UI (static SvelteKit build)
            sparkles-ui = pkgs.callPackage ./nix/ui.nix { fmtWasm = self'.packages.sparkles-fmt-wasm; };
            # `sparkles` binary: CLI + server with the UI embedded
            sparkles = pkgs.callPackage ./nix/package.nix {
              inherit rustPlatform;
              ui = self'.packages.sparkles-ui;
            };
            # same binary without the UI build (no Node.js needed; /ui shows a placeholder)
            sparkles-cli = pkgs.callPackage ./nix/package.nix {
              inherit rustPlatform;
              ui = null;
            };
            default = self'.packages.sparkles;
          };

          apps.default = {
            type = "app";
            program = lib.getExe self'.packages.sparkles;
          };

          devShells.default = pkgs.mkShell {
            # Chromium for the Playwright UI tests (`mise run ui:e2e`). @playwright/test in
            # ui/package.json is pinned to this nixpkgs' playwright-driver version so that the
            # browser revisions match. Elsewhere `mise run ui:e2e` downloads its own.
            env = lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
              PLAYWRIGHT_BROWSERS_PATH = playwrightBrowsers;
              PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS = "true";
            };
            packages = [
              (toolchain.override {
                extensions = [
                  "rust-src"
                  "rust-analyzer"
                ];
              })
              pkgs.nodejs_24
              pkgs.pnpm_10
              pkgs.hyperfine
              pkgs.python3
              pkgs.mise
            ];
          };

          checks = {
            inherit (self'.packages) sparkles sparkles-cli;
            # THIRD_PARTY_LICENSES-UI.md is the notices file the UI build writes (the build
            # itself fails on a non-permissive license)
            ui-licenses = pkgs.runCommand "sparkles-ui-licenses" { } ''
              if ! cmp -s ${self'.packages.sparkles-ui}/licenses.txt ${./THIRD_PARTY_LICENSES-UI.md}; then
                echo "THIRD_PARTY_LICENSES-UI.md is out of date: run 'mise run licenses'" >&2
                exit 1
              fi
              touch $out
            '';
          }
          // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            # Jena's own HTTP clients against the server (`mise run test:jena-clients`)
            jena-clients =
              let
                src = lib.fileset.toSource {
                  root = ./.;
                  fileset = lib.fileset.unions [
                    ./scripts/test-jena-clients.sh
                    ./testsuite/jena-clients
                  ];
                };
              in
              pkgs.runCommand "sparkles-jena-clients"
                {
                  nativeBuildInputs = [
                    pkgs.bash
                    pkgs.curl
                  ];
                }
                ''
                  export HOME="$TMPDIR"
                  SPARKLES_BIN=${lib.getExe self'.packages.sparkles-cli} \
                  JENA_HOME=${pkgs.apache-jena} \
                  JAVA=${pkgs.jdk}/bin/java \
                    bash ${src}/scripts/test-jena-clients.sh
                  touch $out
                '';
            nixos-module = pkgs.testers.runNixOSTest (import ./nix/test.nix { inherit self; });
            # the Playwright UI tests against the release binary, in nixpkgs' Chromium
            ui-e2e = pkgs.callPackage ./nix/ui-e2e.nix {
              inherit (self'.packages) sparkles;
              ui = self'.packages.sparkles-ui;
              browsers = playwrightBrowsers;
            };
          };

          formatter = pkgs.nixfmt;
        };
    };
}
