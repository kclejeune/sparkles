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
        # documentation edits do not rebuild the UI (and the server that embeds it)
        ../ui/README.md
        # the Playwright tests and their output are not part of the build
        (lib.fileset.maybeMissing ../ui/tests)
        (lib.fileset.maybeMissing ../ui/playwright.config.ts)
        (lib.fileset.maybeMissing ../ui/test-results)
        (lib.fileset.maybeMissing ../ui/playwright-report)
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
    hash = "sha256-SeBw8YHV6QGSzMfKisvJi2x1gGDL5yWbVAu6o5QBi98=";
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
