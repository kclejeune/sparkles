# The UI's Playwright end-to-end tests (ui/tests/e2e) as a hermetic check: the tests run
# against `sparkles` (the release build, with the UI embedded, so ui/build is not needed)
# in headless Chromium from nixpkgs' playwright-driver, to which @playwright/test is
# pinned. The server listens on 127.0.0.1 only, which the build sandbox provides. Linux only.
{
  lib,
  stdenvNoCC,
  nodejs_24,
  pnpm_10,
  pnpmConfigHook,
  makeFontsConf,
  dejavu_fonts,
  sparkles,
  # the UI package, whose pnpm dependencies (the same lockfile and hash) are reused
  ui,
  # playwright-driver.browsers, as in the dev shell
  browsers,
}:
stdenvNoCC.mkDerivation {
  pname = "sparkles-ui-e2e";
  inherit (ui) version pnpmDeps;

  src = lib.fileset.toSource {
    root = ../ui;
    fileset = lib.fileset.difference ../ui (
      lib.fileset.unions [
        (lib.fileset.maybeMissing ../ui/node_modules)
        (lib.fileset.maybeMissing ../ui/build)
        (lib.fileset.maybeMissing ../ui/.svelte-kit)
        ../ui/README.md
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

  env = {
    SPARKLES_BIN = lib.getExe sparkles;
    PLAYWRIGHT_BROWSERS_PATH = browsers;
    PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS = "true";
    # the sandbox has no /etc/fonts, and headless Chromium aborts without any font
    FONTCONFIG_FILE = makeFontsConf { fontDirectories = [ dejavu_fonts ]; };
    # forbids test.only, retries a failed test once (reported as flaky)
    CI = "true";
  };

  # Chromium and pnpm write to $HOME
  preConfigure = ''
    export HOME=$(mktemp -d)
  '';

  buildPhase = ''
    runHook preBuild
    pnpm e2e --reporter=list
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    touch $out
    runHook postInstall
  '';

  meta = {
    description = "Playwright end-to-end tests of the Sparkles web UI";
    platforms = lib.platforms.linux;
  };
}
