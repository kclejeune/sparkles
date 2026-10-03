# The `sparkles` binary: Jena-style CLI (load, query, update, dump, compact, backup,
# stats, infer, shacl) and the Fuseki-compatible server (`sparkles serve`).
# With `ui = null` the web UI is not built (no Node.js) and /ui shows a placeholder.
{
  lib,
  stdenv,
  rustPlatform,
  installShellFiles,
  ui ? null,
}:
let
  version = (lib.importTOML ../Cargo.toml).workspace.package.version;
in
rustPlatform.buildRustPackage {
  pname = if ui == null then "sparkles-cli" else "sparkles";
  inherit version;

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

  cargoLock.lockFile = ../Cargo.lock;

  # shell completions and man pages (`sparkles completions`, `sparkles man`)
  nativeBuildInputs = [ installShellFiles ];
  cargoBuildFlags = [
    "-p"
    "sparkles-server"
  ];

  # The UI is embedded at compile time from ui/build (build.rs writes a placeholder
  # page when it is missing).
  preBuild = lib.optionalString (ui != null) ''
    mkdir -p ui
    cp -r ${ui} ui/build
    chmod -R u+w ui/build
  '';

  # Engine unit tests (the W3C suites need a Jena checkout and are not run here).
  cargoTestFlags = [
    "-p"
    "sparkles"
    "--lib"
  ];

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

  meta = {
    description = "High-performance RDF/SPARQL database with a Jena/Fuseki-compatible CLI and server";
    homepage = "https://github.com/kclejeune/sparkles";
    license = lib.licenses.asl20;
    mainProgram = "sparkles";
    platforms = lib.platforms.unix;
  };
}
