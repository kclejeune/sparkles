# The `sparkles` binary: Jena-style CLI (load, query, update, dump, compact, backup,
# stats, infer, shacl) and the Fuseki-compatible server (`sparkles serve`).
# With `ui = null` the web UI is not built (no Node.js) and /ui shows a placeholder.
#
# The build is in two layers (crane). `cargoArtifacts` compiles the dependencies from the
# manifests and Cargo.lock alone, with every workspace source replaced by a stub, so it is
# rebuilt only when a Cargo.toml or Cargo.lock changes. The binary and the unit tests
# (`passthru.tests`, the flake's `sparkles-tests` check) start from its target directory
# and compile only the workspace crates. The binary embeds no UI. With `ui`, the package
# wraps it so that the server reads the UI from the UI's store path at run time
# (`SPARKLES_UI_DIR`), so a UI change rebuilds only the wrapper, and `sparkles` and
# `sparkles-cli` share the binary.
{
  lib,
  stdenv,
  stdenvNoCC,
  craneLib,
  installShellFiles,
  makeBinaryWrapper,
  ui ? null,
  # cargo features of sparkles-server on top of its defaults: the local embedding
  # runtime of spec F12 by default, which adds about 5 MB to the binary and runs nothing
  # until a local provider is configured, and on Linux encrypted backup repositories and
  # sealed runtime secrets (`serve --secrets-key`), whose protected key memory needs
  # Linux; [ ] leaves them out
  features ? [ "embed-local" ] ++ lib.optional stdenv.hostPlatform.isLinux "backup-encryption",
}:
let
  featureArgs = lib.optionalString (
    features != [ ]
  ) " --features ${lib.concatStringsSep "," features}";
  version = (lib.importTOML ../Cargo.toml).workspace.package.version;

  # what the dependency layer is computed from: the manifests, Cargo.lock and the Rust
  # files (crane reads which targets each crate has, then stubs them)
  depsSrc = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      (craneLib.fileset.commonCargoSources ../crates)
      (craneLib.fileset.commonCargoSources ../vendor)
    ];
  };

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../rust-toolchain.toml
      ../crates
      ../vendor
      ../THIRD_PARTY_LICENSES.md
      # vendored test data the crates' tests read (Jena's TriX files)
      ../testsuite/trix
    ];
  };

  # the same for both variants, so that they share the dependency layer and the tests
  commonArgs = {
    pname = "sparkles";
    inherit version;
    strictDeps = true;
    cargoVendorDir = craneLib.vendorCargoDeps { cargoLock = ../Cargo.lock; };
  };

  # The server's dependencies, and those of the unit tests of the engine and the facade,
  # which are built with the engine's own features and so are different builds of some
  # crates.
  cargoArtifacts = craneLib.buildDepsOnly (
    commonArgs
    // {
      src = depsSrc;
      buildPhaseCargoCommand = "cargoWithProfile build --locked -p sparkles-server${featureArgs}";
      checkPhaseCargoCommand = "cargoWithProfile test --locked -p sparkles-core -p sparkles --lib --no-run";
    }
  );

  # The unit tests of the engine and the facade (the W3C suites need a Jena checkout and
  # are not run here).
  tests = craneLib.cargoTest (
    commonArgs
    // {
      inherit src cargoArtifacts;
      cargoTestExtraArgs = "-p sparkles-core -p sparkles --lib";
      doInstallCargoArtifacts = false;
    }
  );

  meta = {
    description = "High-performance RDF/SPARQL database with a Jena/Fuseki-compatible CLI and server";
    homepage = "https://github.com/kclejeune/sparkles";
    license = lib.licenses.asl20;
    mainProgram = "sparkles";
    platforms = lib.platforms.unix;
  };

  # The binary, without the UI (build.rs embeds a placeholder page). It is the same
  # derivation for `sparkles` and `sparkles-cli`.
  bin = craneLib.buildPackage (
    commonArgs
    // {
      pname = "sparkles-cli";
      inherit src cargoArtifacts;

      cargoExtraArgs = "--locked -p sparkles-server${featureArgs}";
      # the unit tests are their own derivation (`tests`)
      doCheck = false;

      # shell completions and man pages (`sparkles completions`, `sparkles man`)
      nativeBuildInputs = [ installShellFiles ];

      # the licenses and notices of the linked crates (Apache-2.0 asks for NOTICE files to
      # travel with the binary); `mise run licenses` regenerates the file
      postInstall = ''
        install -Dm644 THIRD_PARTY_LICENSES.md $out/share/doc/sparkles/THIRD_PARTY_LICENSES.md
      ''
      # completions, man pages and the OpenAPI description come from the binary itself, so
      # only a build that can run it installs them
      + lib.optionalString (stdenv.buildPlatform.canExecute stdenv.hostPlatform) ''
        installShellCompletion --cmd sparkles \
          --bash <($out/bin/sparkles completions bash) \
          --zsh <($out/bin/sparkles completions zsh) \
          --fish <($out/bin/sparkles completions fish)
        $out/bin/sparkles man --dir man
        installManPage man/*.1
        $out/bin/sparkles openapi > $out/share/doc/sparkles/openapi.json
      '';

      passthru = { inherit cargoArtifacts tests; };

      inherit meta;
    }
  );
in
if ui == null then
  bin
else
  # The binary with the UI: a wrapper that points `SPARKLES_UI_DIR` at the UI build, and
  # the licenses of the npm packages in it (written by the UI build; the check
  # `ui-licenses` compares them with THIRD_PARTY_LICENSES-UI.md)
  stdenvNoCC.mkDerivation {
    pname = "sparkles";
    inherit version;
    nativeBuildInputs = [ makeBinaryWrapper ];
    buildCommand = ''
      mkdir -p $out/bin
      cp -rs --no-preserve=mode ${bin}/share $out/share
      makeBinaryWrapper ${bin}/bin/sparkles $out/bin/sparkles \
        --set-default SPARKLES_UI_DIR ${ui}
      install -Dm644 ${ui}/licenses.txt $out/share/doc/sparkles/THIRD_PARTY_LICENSES-UI.md
    '';
    passthru = {
      inherit cargoArtifacts tests ui;
      unwrapped = bin;
    };
    inherit meta;
  }
