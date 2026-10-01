# The formatter for the browser (crates/sparkles-fmt-wasm): the WebAssembly module and its
# JavaScript bindings, as scripts/build-fmt-wasm.sh builds them for `mise run ui:wasm`. The
# UI package builds them in when given them (`fmtWasm`); without them the UI formats
# through POST /$/format.
{
  lib,
  rustPlatform,
  buildWasmBindgenCli,
  fetchCrate,
}:
let
  version = (lib.importTOML ../Cargo.toml).workspace.package.version;
  # the wasm-bindgen CLI must be the version of the wasm-bindgen crate in Cargo.lock
  locked =
    (lib.findFirst (p: p.name == "wasm-bindgen") null (lib.importTOML ../Cargo.lock).package).version;
  wasm-bindgen-cli = buildWasmBindgenCli rec {
    src = fetchCrate {
      pname = "wasm-bindgen-cli";
      version = "0.2.129";
      hash = "sha256-pcecKQd7E8Opw6bkFoE569epUi7gh5qpQF1e5PJY6V8=";
    };
    cargoDeps = rustPlatform.fetchCargoVendor {
      inherit src;
      inherit (src) pname version;
      hash = "sha256-vmUrWVU7kPJJxO5qIVeAkwQyWDELO1Z4Z5gitz2kco8=";
    };
  };
in
assert lib.assertMsg (wasm-bindgen-cli.version == locked)
  "nix/fmt-wasm.nix builds wasm-bindgen-cli ${wasm-bindgen-cli.version} but Cargo.lock has wasm-bindgen ${locked}: update its version and hashes";
rustPlatform.buildRustPackage {
  pname = "sparkles-fmt-wasm";
  inherit version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../rust-toolchain.toml
      ../crates
      ../vendor
      ../scripts/build-fmt-wasm.sh
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;
  nativeBuildInputs = [ wasm-bindgen-cli ];

  # the script's own build (for wasm32-unknown-unknown, which rust-toolchain.toml adds to
  # the toolchain), not the host build of buildRustPackage
  buildPhase = ''
    runHook preBuild
    bash scripts/build-fmt-wasm.sh "$out"
    runHook postBuild
  '';
  doCheck = false;
  installPhase = ''
    runHook preInstall
    runHook postInstall
  '';

  meta = {
    description = "The Sparkles formatter for the browser (WebAssembly)";
    license = lib.licenses.asl20;
  };
}
