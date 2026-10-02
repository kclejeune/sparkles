# The Python bindings (crates/sparkles-py): the `sparkles` package, built into an abi3
# wheel by maturin from the crate's own Cargo.lock and installed for nixpkgs' Python. The
# check phase runs the pytest suite, with mypy's stubtest and the rdflib tests, on the
# installed package (`checks.python-bindings`).
{
  lib,
  python3,
  rustPlatform,
}:
let
  crate = (lib.importTOML ../crates/sparkles-py/Cargo.toml).package;
in
python3.pkgs.buildPythonPackage {
  pname = "sparkles-rdf";
  inherit (crate) version;
  pyproject = true;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      # the root workspace, which the library crates inherit their dependencies from
      ../Cargo.toml
      ../LICENSE
      ../crates
      ../vendor
    ];
  };

  buildAndTestSubdir = "crates/sparkles-py";
  cargoRoot = "crates/sparkles-py";
  cargoDeps = rustPlatform.importCargoLock { lockFile = ../crates/sparkles-py/Cargo.lock; };

  nativeBuildInputs = [
    rustPlatform.cargoSetupHook
    rustPlatform.maturinBuildHook
  ];

  nativeCheckInputs = [
    python3.pkgs.pytestCheckHook
    python3.pkgs.mypy
    python3.pkgs.rdflib
  ];
  enabledTestPaths = [ "crates/sparkles-py/tests" ];
  pythonImportsCheck = [ "sparkles" ];

  meta = {
    description = "Python bindings for the Sparkles RDF/SPARQL database";
    homepage = "https://github.com/kclejeune/sparkles";
    license = lib.licenses.asl20;
    platforms = lib.platforms.unix;
  };
}
