# The JVM bindings (spec docs/specs/P04-jvm-bindings.md).
#
# `ffi` is the native library (crates/sparkles-ffi), built from the crate's own Cargo.lock,
# with the Kotlin bindings that its uniffi-bindgen generates in share/uniffi. `jar` is the
# sparkles-jena jar, built by Gradle from jvm/ with the host's library inside. Its Maven
# dependencies come from nix/jvm-deps.json, which `mise run jvm:nix-deps` refreshes.
# `check` runs the library's tests (Jena's contract tests among them) and the Java sample's.
{
  lib,
  stdenv,
  rustPlatform,
  gradle_9,
  jdk17_headless,
}:
let
  crate = (lib.importTOML ../crates/sparkles-ffi/Cargo.toml).package;
  # Gradle runs on Java 17, which is the toolchain the build asks for
  gradle = gradle_9.override { java = jdk17_headless; };
  libName = "libsparkles_ffi${stdenv.hostPlatform.extensions.sharedLibrary}";

  ffi = rustPlatform.buildRustPackage {
    pname = "sparkles-ffi";
    inherit (crate) version;

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

    cargoRoot = "crates/sparkles-ffi";
    buildAndTestSubdir = "crates/sparkles-ffi";
    cargoLock.lockFile = ../crates/sparkles-ffi/Cargo.lock;
    # one build makes the library and the generator; the feature only adds the generator
    buildFeatures = [ "bindgen" ];
    # the crate's tests run in `mise run jvm:rust-test`; the jar's check exercises the library
    doCheck = false;

    postInstall = ''
      $out/bin/uniffi-bindgen generate --library $out/lib/${libName} --language kotlin \
        --no-format --metadata-no-deps --config crates/sparkles-ffi/bindgen.toml \
        --out-dir $out/share/uniffi
    '';

    meta = {
      description = "The native library of the Sparkles JVM bindings";
      homepage = "https://github.com/kclejeune/sparkles";
      license = lib.licenses.asl20;
      platforms = lib.platforms.unix;
    };
  };

  jar = stdenv.mkDerivation (finalAttrs: {
    pname = "sparkles-jena";
    version = lib.removePrefix "version=" (
      lib.head (
        lib.filter (l: lib.hasPrefix "version=" l) (
          lib.splitString "\n" (builtins.readFile ../jvm/gradle.properties)
        )
      )
    );

    # jvm/ with the project's license beside it, which the jar carries
    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ../LICENSE
        (lib.fileset.difference ../jvm (
          lib.fileset.unions [
            (lib.fileset.maybeMissing ../jvm/build)
            (lib.fileset.maybeMissing ../jvm/.gradle)
            (lib.fileset.maybeMissing ../jvm/.kotlin)
            (lib.fileset.maybeMissing ../jvm/sparkles-jena/build)
            (lib.fileset.maybeMissing ../jvm/sample-java/build)
          ]
        ))
      ];
    };
    sourceRoot = "${finalAttrs.src.name}/jvm";

    nativeBuildInputs = [ gradle ];

    mitmCache = gradle.fetchDeps {
      pkg = finalAttrs.finalPackage;
      data = ./jvm-deps.json;
    };
    # the MITM cache's proxy listens on a local port
    __darwinAllowLocalNetworking = true;

    gradleFlags = [
      "-Psparkles.nativeLib=${ffi}/lib/${libName}"
      "-Psparkles.bindings=${ffi}/share/uniffi"
      "-Dfile.encoding=utf-8"
      # nixpkgs' nixDownloadDeps task reads the projects' configurations when it runs
      "--no-configuration-cache"
    ];
    gradleBuildTask = ":sparkles-jena:assemble";
    # everything the build and the tests resolve, for the update script
    gradleUpdateTask = "nixDownloadDeps :sparkles-jena:assemble :sparkles-jena:testClasses :sample-java:testClasses";
    gradleCheckTask = ":sparkles-jena:test :sample-java:test";
    doCheck = false;

    installPhase = ''
      runHook preInstall
      mkdir -p $out/share/java
      cp sparkles-jena/build/libs/sparkles-jena-${finalAttrs.version}.jar $out/share/java/
      cp sparkles-jena/build/libs/sparkles-jena-${finalAttrs.version}-sources.jar $out/share/java/
      runHook postInstall
    '';

    passthru = { inherit ffi; };

    meta = {
      description = "Apache Jena's DatasetGraph backed by Sparkles (the JVM bindings), with the host's native library";
      homepage = "https://github.com/kclejeune/sparkles";
      license = lib.licenses.asl20;
      platforms = lib.platforms.unix;
      sourceProvenance = with lib.sourceTypes; [
        fromSource
        # the Maven dependencies in the MITM cache
        binaryBytecode
      ];
    };
  });
in
{
  inherit ffi jar;
  # the jar's build with its tests and the Java sample's tests as its check phase
  check = jar.overrideAttrs { doCheck = true; };
}
