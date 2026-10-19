# Package the published release without inspecting or building Fluree's source.
{
  lib,
  stdenv,
  fetchurl,
  autoPatchelfHook,
  versionCheckHook,
}:
let
  releases = {
    x86_64-linux = {
      target = "x86_64-unknown-linux-gnu";
      hash = "sha256-Op8grgGa+sgww/IjQhICAM58uKEvIGvqiHf9scnYWQc=";
    };
    aarch64-linux = {
      target = "aarch64-unknown-linux-gnu";
      hash = "sha256-nWgz2Y8qlDhdWMeOKjzu71N5n8dU1cNmi/C1VqKlYeI=";
    };
    aarch64-darwin = {
      target = "aarch64-apple-darwin";
      hash = "sha256-/lQsPS4rIdcRH9KeOecPgtfjh0q2nzRo1tBCmUuWPt4=";
    };
  };
  release = releases.${stdenv.hostPlatform.system};
in
stdenv.mkDerivation (finalAttrs: {
  pname = "fluree";
  version = "4.2.2";

  src = fetchurl {
    url = "https://github.com/fluree/db/releases/download/v${finalAttrs.version}/fluree-db-cli-${release.target}.tar.xz";
    inherit (release) hash;
  };

  nativeBuildInputs = lib.optionals stdenv.hostPlatform.isLinux [ autoPatchelfHook ];
  buildInputs = lib.optionals stdenv.hostPlatform.isLinux [ stdenv.cc.cc.lib ];
  dontBuild = true;
  dontStrip = true;

  installPhase = ''
    runHook preInstall
    install -Dm755 fluree $out/bin/fluree
    install -Dm644 LICENSE $out/share/licenses/fluree/LICENSE
    install -Dm644 README.md $out/share/doc/fluree/README.md
    runHook postInstall
  '';

  doInstallCheck = true;
  nativeInstallCheckInputs = [ versionCheckHook ];
  versionCheckProgram = "${placeholder "out"}/bin/fluree";

  meta = {
    description = "Fluree database CLI and embedded server from the pinned binary release";
    homepage = "https://www.labs.flur.ee/docs/db/";
    changelog = "https://github.com/fluree/db/releases/tag/v${finalAttrs.version}";
    license = lib.licenses.bsl11;
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
    platforms = builtins.attrNames releases;
    mainProgram = "fluree";
  };
})
