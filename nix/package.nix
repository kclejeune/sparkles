# The `sparkles` binary: Jena-style CLI (load, query, update, dump, compact, backup,
# stats, infer, shacl) and the Fuseki-compatible server (`sparkles serve`).
# With `ui = null` the web UI is not built (no Node.js) and /ui shows a placeholder.
#
# The build is in two layers (crane). `cargoArtifacts` compiles the dependencies from the
# manifests and Cargo.lock alone, with every workspace source replaced by a stub, so it is
# rebuilt only when a Cargo.toml or Cargo.lock changes. The package and the unit tests
# (`passthru.tests`, the flake's `sparkles-tests` check) start from its target directory
# and compile only the workspace crates. The UI is copied in by the package alone, so a UI
# change rebuilds neither the dependencies nor the tests, and `sparkles` and
# `sparkles-cli` share both.
{
  lib,
  stdenv,
  craneLib,
  installShellFiles,
  ui ? null,
}:
let
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

  # The server's dependencies, and those of the engine's unit tests, which are built with
  # the engine's own features and so are different builds of some crates.
  cargoArtifacts = craneLib.buildDepsOnly (
    commonArgs
    // {
      src = depsSrc;
      buildPhaseCargoCommand = "cargoWithProfile build --locked -p sparkles-server";
      checkPhaseCargoCommand = "cargoWithProfile test --locked -p sparkles --lib --no-run";
    }
  );

  # Engine unit tests (the W3C suites need a Jena checkout and are not run here).
  tests = craneLib.cargoTest (
    commonArgs
    // {
      inherit src cargoArtifacts;
      cargoTestExtraArgs = "-p sparkles --lib";
      doInstallCargoArtifacts = false;
    }
  );
in
craneLib.buildPackage (
  commonArgs
  // {
    pname = if ui == null then "sparkles-cli" else "sparkles";
    inherit src cargoArtifacts;

    cargoExtraArgs = "--locked -p sparkles-server";
    # the unit tests are their own derivation (`tests`)
    doCheck = false;

    # shell completions and man pages (`sparkles completions`, `sparkles man`)
    nativeBuildInputs = [ installShellFiles ];

    # The UI is embedded at compile time from ui/build (build.rs writes a placeholder
    # page when it is missing).
    preBuild = lib.optionalString (ui != null) ''
      mkdir -p ui
      cp -r ${ui} ui/build
      chmod -R u+w ui/build
    '';

    # the licenses and notices of the linked crates (Apache-2.0 asks for NOTICE files to
    # travel with the binary), and of the npm packages in the embedded UI (written by the UI
    # build; the check `ui-licenses` compares them with THIRD_PARTY_LICENSES-UI.md);
    # `mise run licenses` regenerates both files
    postInstall = ''
      install -Dm644 THIRD_PARTY_LICENSES.md $out/share/doc/sparkles/THIRD_PARTY_LICENSES.md
    ''
    + lib.optionalString (ui != null) ''
      install -Dm644 ${ui}/licenses.txt $out/share/doc/sparkles/THIRD_PARTY_LICENSES-UI.md
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

    meta = {
      description = "High-performance RDF/SPARQL database with a Jena/Fuseki-compatible CLI and server";
      homepage = "https://github.com/kclejeune/sparkles";
      license = lib.licenses.asl20;
      mainProgram = "sparkles";
      platforms = lib.platforms.unix;
    };
  }
)
