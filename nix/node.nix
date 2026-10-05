# Native Node-API addon, distributable TypeScript packages and installed-package smoke.
{
  lib,
  rustPlatform,
  nodejs_24,
  pnpm_10,
  fetchPnpmDeps,
  pnpmConfigHook,
  stdenvNoCC,
  runCommand,
}:
let
  version = (lib.importTOML ../crates/sparkles-node/Cargo.toml).package.version;
  src = lib.fileset.toSource {
    root = ../.;
    fileset =
      lib.fileset.difference
        (lib.fileset.unions [
          ../Cargo.toml
          ../LICENSE
          ../crates
          ../vendor
          ../js
          ../scripts/node-native.sh
        ])
        (
          lib.fileset.unions [
            ../crates/sparkles-ffi
            (lib.fileset.maybeMissing ../crates/sparkles-node/target)
            ../crates/sparkles-py
            (lib.fileset.maybeMissing ../js/node_modules)
            (lib.fileset.maybeMissing ../js/engine/native)
            (lib.fileset.fileFilter (file: file.hasExt "node") ../js/engine/npm)
            (lib.fileset.maybeMissing ../js/common/dist)
            (lib.fileset.maybeMissing ../js/client/dist)
            (lib.fileset.maybeMissing ../js/engine/dist)
          ]
        );
  };
  native = rustPlatform.buildRustPackage {
    pname = "sparkles-node-native";
    inherit version src;
    cargoRoot = "crates/sparkles-node";
    buildAndTestSubdir = "crates/sparkles-node";
    cargoLock.lockFile = ../crates/sparkles-node/Cargo.lock;
    doCheck = false;
    installPhase = ''
      mkdir -p $out/lib
      find target -type f \( -name 'libsparkles_node.so' -o -name 'libsparkles_node.dylib' \) -exec cp {} $out/lib/ \;
    '';
    meta = {
      description = "Sparkles Node-API addon";
      license = lib.licenses.asl20;
      platforms = lib.platforms.unix;
    };
  };
  packages = stdenvNoCC.mkDerivation (final: {
    pname = "sparkles-js-packages";
    inherit version src;
    nativeBuildInputs = [
      nodejs_24
      pnpm_10
      pnpmConfigHook
    ];
    pnpmRoot = "js";
    pnpmDeps = fetchPnpmDeps {
      inherit src;
      sourceRoot = "${src.name}/js";
      pname = final.pname;
      inherit version;
      pnpm = pnpm_10;
      fetcherVersion = 4;
      hash = "sha256-5pwSilEuKomgAsVDlU7jOPPiI7tw5cPMs9EyM61kZ0I=";
    };
    buildPhase = ''
      cd js
      pnpm build
      mkdir -p engine/native
      case "${nodejs_24.system}" in *darwin) platform=darwin; library=libsparkles_node.dylib ;; *) platform=linux; library=libsparkles_node.so ;; esac
      arch=$(node -p process.arch)
      suffix=
      if [ "$platform" = linux ]; then suffix=-gnu; fi
      cp ${native}/lib/$library engine/native/sparkles.$platform-$arch$suffix.node
      node scripts/platforms.mjs
    '';
    installPhase = ''
      mkdir -p $out
      export npm_config_cache=$TMPDIR/npm-cache
      node scripts/pack-runtime.mjs $out
      cp scripts/packed-smoke.mjs scripts/packed-types.mts $out/

    '';
    meta = {
      description = "Sparkles JavaScript package archives";
      license = lib.licenses.asl20;
      platforms = lib.platforms.unix;
    };
  });
  check = runCommand "sparkles-node-installed-check" { nativeBuildInputs = [ nodejs_24 ]; } ''
    export npm_config_cache=$TMPDIR/npm-cache
    mkdir project
    cd project
    npm init -y >/dev/null
    npm install --offline --ignore-scripts --omit=optional ${packages}/*.tgz >/dev/null
    cp ${packages}/packed-smoke.mjs ${packages}/packed-types.mts .
    node packed-smoke.mjs
    node node_modules/typescript/bin/tsc --noEmit --strict --target es2023 --module nodenext packed-types.mts
    touch $out
  '';
in
{
  inherit native packages check;
}
