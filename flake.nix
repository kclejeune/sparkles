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

        # Adds `sparkles`, `sparkles-cli` and `sparkles-ui` to a package set.
        overlays.default =
          final: prev:
          let
            pkgs = final.extend inputs.rust-overlay.overlays.default;
            toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
            rustPlatform = pkgs.makeRustPlatform {
              cargo = toolchain;
              rustc = toolchain;
            };
            ui = final.callPackage ./nix/ui.nix { };
          in
          {
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
        in
        {
          _module.args.pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [ inputs.rust-overlay.overlays.default ];
          };

          packages = {
            # the web UI (static SvelteKit build)
            sparkles-ui = pkgs.callPackage ./nix/ui.nix { };
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
          }
          // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            nixos-module = pkgs.testers.runNixOSTest (import ./nix/test.nix { inherit self; });
          };

          formatter = pkgs.nixfmt;
        };
    };
}
