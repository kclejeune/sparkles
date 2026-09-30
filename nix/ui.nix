# Static SvelteKit build of the web UI (served by the server under /ui/).
{
  lib,
  stdenvNoCC,
  nodejs_24,
  pnpm_10,
  fetchPnpmDeps,
  pnpmConfigHook,
}:
stdenvNoCC.mkDerivation (finalAttrs: {
  pname = "sparkles-ui";
  version = (lib.importTOML ../Cargo.toml).workspace.package.version;

  src = lib.fileset.toSource {
    root = ../ui;
    fileset = lib.fileset.difference ../ui (
      lib.fileset.unions [
        (lib.fileset.maybeMissing ../ui/node_modules)
        (lib.fileset.maybeMissing ../ui/build)
        (lib.fileset.maybeMissing ../ui/.svelte-kit)
      ]
    );
  };

  nativeBuildInputs = [
    nodejs_24
    pnpm_10
    pnpmConfigHook
  ];

  pnpmDeps = fetchPnpmDeps {
    inherit (finalAttrs) pname version src;
    pnpm = pnpm_10;
    fetcherVersion = 4;
    hash = "sha256-cJsR2mKj9v7+9ELcbpWu/UV+p1Mpxl7QC/HrQzxUv5Y=";
  };

  buildPhase = ''
    runHook preBuild
    pnpm build
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    cp -r build $out
    runHook postInstall
  '';

  meta = {
    description = "Web UI for the Sparkles RDF database";
    license = lib.licenses.asl20;
  };
})
