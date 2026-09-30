# The `sparkles` binary: Jena-style CLI (load, query, update, dump, compact, backup,
# stats, infer, shacl) and the Fuseki-compatible server (`sparkles serve`).
# With `ui = null` the web UI is not built (no Node.js) and /ui shows a placeholder.
{
  lib,
  rustPlatform,
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
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;
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

  meta = {
    description = "High-performance RDF/SPARQL database with a Jena/Fuseki-compatible CLI and server";
    homepage = "https://github.com/kclejeune/sparkles";
    license = lib.licenses.asl20;
    mainProgram = "sparkles";
    platforms = lib.platforms.unix;
  };
}
