{
  description = "Sparkles: high-performance RDF / SPARQL database (Jena/Fuseki compatible, QLever-style index)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
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

        # `programs.sparkles` for Home Manager: the client's config files and agent hooks.
        # `homeModules` is the name Home Manager's own flake-parts module uses.
        homeModules.default =
          { lib, pkgs, ... }:
          {
            imports = [ ./nix/home-module.nix ];
            programs.sparkles.package = lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.sparkles;
          };
        homeModules.sparkles = self.homeModules.default;

        # Adds `sparkles`, `sparkles-cli`, `sparkles-ui`, `sparkles-fmt-wasm` and the
        # `sparkles-model-snapshots` helper to a package set.
        overlays.default =
          final: prev:
          let
            pkgs = final.extend inputs.rust-overlay.overlays.default;
            craneLib = (inputs.crane.mkLib pkgs).overrideToolchain (
              p: p.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml
            );
            fmtWasm = final.callPackage ./nix/fmt-wasm.nix { inherit craneLib; };
            ui = final.callPackage ./nix/ui.nix { inherit fmtWasm; };
          in
          {
            sparkles-fmt-wasm = fmtWasm;
            sparkles-ui = ui;
            sparkles = final.callPackage ./nix/package.nix { inherit craneLib ui; };
            sparkles-model-snapshots = final.callPackage ./nix/model-snapshots.nix { };
            sparkles-cli = final.callPackage ./nix/package.nix {
              inherit craneLib;
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
          # the Rust packages (crane, which builds the dependencies as a layer of their own)
          craneLib = (inputs.crane.mkLib pkgs).overrideToolchain toolchain;
          # the Python bindings (maturin, through nixpkgs' hooks)
          rustPlatform = pkgs.makeRustPlatform {
            cargo = toolchain;
            rustc = toolchain;
          };
          # Chromium for the Playwright UI tests (the dev shell and the `ui-e2e` check)
          # the JVM bindings: the native library, the jar and its check
          jvm = pkgs.callPackage ./nix/jvm.nix { inherit rustPlatform; };
          node = pkgs.callPackage ./nix/node.nix { inherit rustPlatform; };
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
            sparkles-fmt-wasm = pkgs.callPackage ./nix/fmt-wasm.nix { inherit craneLib; };
            # the web UI (static SvelteKit build)
            sparkles-ui = pkgs.callPackage ./nix/ui.nix { fmtWasm = self'.packages.sparkles-fmt-wasm; };
            # `sparkles` binary: CLI + server with the UI embedded
            sparkles = pkgs.callPackage ./nix/package.nix {
              inherit craneLib;
              ui = self'.packages.sparkles-ui;
            };
            # same binary without the UI build (no Node.js needed; /ui shows a placeholder)
            sparkles-cli = pkgs.callPackage ./nix/package.nix {
              inherit craneLib;
              ui = null;
            };
            # the server built with PDF OCR (`pdf-ocr`) and local embeddings; PDFium and ONNX Runtime come from
            # the image or the host at run time
            sparkles-ocr = pkgs.callPackage ./nix/package.nix {
              inherit craneLib;
              ui = self'.packages.sparkles-ui;
              features = [
                "embed-local"
                "pdf-ocr"
              ];
            };
            # the Python bindings (crates/sparkles-py) for nixpkgs' python3
            sparkles-py = pkgs.callPackage ./nix/python.nix { inherit rustPlatform; };
            # the JVM bindings' native library (lib/) with its generated Kotlin (share/uniffi)
            sparkles-ffi = jvm.ffi;
            # the sparkles-jena jar (share/java) with the host's native library inside
            sparkles-jena = jvm.jar;
            # the Node-API addon and installable JavaScript package archives
            sparkles-node-native = node.native;
            sparkles-node = node.packages;
            default = self'.packages.sparkles;
          }
          // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            # OCI images of the server (nix/image.nix): `nix build .#image && ./result | docker load`
            image = pkgs.callPackage ./nix/image.nix { sparkles = self'.packages.sparkles; };
            image-ocr = pkgs.callPackage ./nix/image.nix {
              sparkles = self'.packages.sparkles-ocr;
              ocr = true;
            };
          };

          # `modelSnapshots { manifests = [ ./model.sparkles-manifest.json ]; }` builds a
          # read-only model store for `--models-dir` from pinned snapshot manifests (F12)
          legacyPackages.modelSnapshots = pkgs.callPackage ./nix/model-snapshots.nix { };

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
              # the scripts, and the Python bindings' tests (`mise run py:test`)
              (pkgs.python3.withPackages (ps: [
                ps.pytest
                ps.mypy
                ps.rdflib
              ]))
              pkgs.maturin
              # the JVM bindings (`mise run jvm:test`): a Java 17 toolchain and Gradle 9
              pkgs.jdk17
              pkgs.gradle_9
              pkgs.mise
            ];
          };

          checks = {
            inherit (self'.packages) sparkles sparkles-cli;
            # the engine's unit tests, on the packages' dependency layer
            sparkles-tests = self'.packages.sparkles-cli.passthru.tests;
            # rustfmt, including the language-binding crates' separate workspaces.
            fmt = craneLib.cargoFmt {
              pname = "sparkles";
              version = (lib.importTOML ./Cargo.toml).workspace.package.version;
              src = lib.fileset.toSource {
                root = ./.;
                fileset = lib.fileset.unions [
                  ./Cargo.toml
                  ./Cargo.lock
                  (craneLib.fileset.commonCargoSources ./crates)
                  (craneLib.fileset.commonCargoSources ./vendor)
                ];
              };
              cargoExtraArgs = "--all";
              postBuild = ''
                cargo fmt --manifest-path crates/sparkles-py/Cargo.toml -- --check
                cargo fmt --manifest-path crates/sparkles-ffi/Cargo.toml -- --check
                cargo fmt --manifest-path crates/sparkles-node/Cargo.toml -- --check
              '';
            };
            # the wheel, installed, with the pytest suite as its check phase
            python-bindings = self'.packages.sparkles-py;
            node-bindings = node.check;
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
            # the module without a VM; the settings check of the build needs the CLI
            nixos-settings = import ./nix/settings-test.nix {
              inherit pkgs;
              sparkles = self'.packages.sparkles-cli;
            };
            # the Home Manager module's files, read back by the CLI
            home-module = import ./nix/home-module-test.nix {
              inherit pkgs;
              sparkles = self'.packages.sparkles-cli;
            };
            nixos-models = import ./nix/models-test.nix {
              inherit pkgs;
              sparkles = self'.packages.sparkles-cli;
            };
            # the jar built offline, with Jena's contract tests, the binding's own tests and
            # the Java sample's tests as its check phase
            jvm-bindings = jvm.check;
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
