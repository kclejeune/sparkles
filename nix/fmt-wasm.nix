# The formatter for the browser (crates/sparkles-fmt-wasm): the WebAssembly module and its
# JavaScript bindings, as scripts/build-fmt-wasm.sh builds them for `mise run ui:wasm`. The
# UI package builds them in when given them (`fmtWasm`); without them the UI formats
# through POST /$/format.
#
# Like nix/package.nix, the build is in two layers (crane): the dependencies, compiled for
# wasm32-unknown-unknown with the `fmt-wasm` profile from the manifests and Cargo.lock
# alone, then the script, which compiles the workspace crates and runs wasm-bindgen. The
# script's source has every workspace crate stubbed but the formatter, its WebAssembly
# crate and the vendored spargebra, so an edit to another crate changes neither layer.
{
  lib,
  craneLib,
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

  commonArgs = {
    pname = "sparkles-fmt-wasm";
    inherit version;
    strictDeps = true;
    cargoVendorDir = craneLib.vendorCargoDeps { cargoLock = ../Cargo.lock; };
    # the script's target and profile (the profile is in the workspace's Cargo.toml)
    CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
    CARGO_PROFILE = "fmt-wasm";
    doCheck = false;
  };

  # the manifests, Cargo.lock and the targets of every crate, which crane stubs
  manifests = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      (craneLib.fileset.commonCargoSources ../crates)
      (craneLib.fileset.commonCargoSources ../vendor)
    ];
  };

  cargoArtifacts = craneLib.buildDepsOnly (
    commonArgs
    // {
      src = manifests;
      buildPhaseCargoCommand = "cargoWithProfile build --locked -p sparkles-fmt-wasm";
    }
  );

  # what the script compiles, besides the stubs
  real = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../rust-toolchain.toml
      ../crates/sparkles-fmt
      ../crates/sparkles-fmt-wasm
      ../vendor
      ../scripts/build-fmt-wasm.sh
    ];
  };

  # The workspace with the other crates stubbed, and the real formatter crates in place.
  src = craneLib.mkDummySrc {
    src = manifests;
    cargoLock = ../Cargo.lock;
    extraDummyScript = ''
      chmod -R u+w $out
      rm -rf $out/Cargo.toml $out/crates/sparkles-fmt $out/crates/sparkles-fmt-wasm $out/vendor
      cp -r --no-preserve=mode ${real}/crates/sparkles-fmt ${real}/crates/sparkles-fmt-wasm $out/crates/
      cp -r --no-preserve=mode ${real}/vendor $out/vendor
      mkdir -p $out/scripts
      cp --no-preserve=mode ${real}/Cargo.toml ${real}/rust-toolchain.toml $out/
      cp --no-preserve=mode ${real}/scripts/build-fmt-wasm.sh $out/scripts/
    '';
  };
in
assert lib.assertMsg (wasm-bindgen-cli.version == locked)
  "nix/fmt-wasm.nix builds wasm-bindgen-cli ${wasm-bindgen-cli.version} but Cargo.lock has wasm-bindgen ${locked}: update its version and hashes";
craneLib.mkCargoDerivation (
  commonArgs
  // {
    inherit cargoArtifacts src;

    nativeBuildInputs = [ wasm-bindgen-cli ];

    # the script writes the module and its bindings to $out
    buildPhaseCargoCommand = ''bash scripts/build-fmt-wasm.sh "$out"'';
    installPhaseCommand = "";
    doInstallCargoArtifacts = false;

    passthru = { inherit cargoArtifacts; };

    meta = {
      description = "The Sparkles formatter for the browser (WebAssembly)";
      license = lib.licenses.asl20;
    };
  }
)
