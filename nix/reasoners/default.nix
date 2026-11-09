# The reference reasoners that docs/benchmarks/el-classification compares the classifier of
# sparkles-owl with: ROBOT, ELK, HermiT, Whelk and Konclude, and Nemo, the reference OWL 2
# RL materializer of docs/benchmarks/reasoning. Each comes from a checksum-pinned release,
# the Java tools from Maven Central or the project's GitHub releases, Konclude from its
# Linux x86_64 release binary and Nemo from its release binaries. They are benchmark-only
# packages and never ship with Sparkles.
#
# The Java packages carry a wrapper that names the JDK below, so a systemd unit with a
# minimal PATH still runs them. `elk-classify`, `hermit-classify` and `whelk-classify` run
# scripts/elk-compare/ElkClassify.java, which each package compiles against its own jars,
# with the JVM options of JAVA_OPTS. `robot` runs ROBOT's release jar with the options of
# ROBOT_JAVA_ARGS, as ROBOT's own launcher does. Each package also keeps its jars in
# share/java/<name>.
{
  lib,
  stdenv,
  fetchurl,
  runCommand,
  writeShellScript,
  autoPatchelfHook,
  jdk17_headless,
  glib,
  pcre,
  unzip,
  xz,
  zlib,
}:
let
  jdk = jdk17_headless;
  central = "https://repo1.maven.org/maven2";

  # The jars of a pins file, one `path<TAB>sha256` line each under Maven Central.
  pinned =
    file:
    map
      (
        line:
        let
          fields = lib.splitString "\t" line;
          path = builtins.elemAt fields 0;
        in
        fetchurl {
          url = "${central}/${path}";
          sha256 = builtins.elemAt fields 1;
          name = baseNameOf path;
        }
      )
      (
        builtins.filter (line: line != "" && !lib.hasPrefix "#" line) (
          lib.splitString "\n" (builtins.readFile file)
        )
      );

  # The licence texts installed with the packages, from the projects' release tags. Most
  # of the dependency jars carry no notice of their own, so each Java package also lists
  # its jars by Maven Central path in share/licenses/<name>/jars.tsv, whose POMs name
  # their licences.
  licence =
    name: url: hash:
    fetchurl { inherit name url hash; };
  licences = {
    elk =
      licence "elk-LICENSE.txt"
        "https://raw.githubusercontent.com/liveontologies/elk-reasoner/v0.6.0/LICENSE.txt"
        "sha256-xx0jnfkXJvxRnG63LTGOxlggYnIysveWIZ6H3PNdCrQ=";
    whelk =
      licence "whelk-LICENSE.txt" "https://raw.githubusercontent.com/INCATools/whelk/v1.2.1/LICENSE.txt"
        "sha256-0aBOv8RuV7h3Qw5j6WfUx/RC6Pzr10Jr2pCb9YDZhP0=";
    robot =
      licence "robot-LICENSE.txt" "https://raw.githubusercontent.com/ontodev/robot/v1.9.11/LICENSE.txt"
        "sha256-yGLOMzatHsBDPkNnKDdfeoTkVRQ4jXyrB6KjmuxsFrI=";
    lgpl3 =
      licence "lgpl-3.0.txt" "https://www.gnu.org/licenses/lgpl-3.0.txt"
        "sha256-46mU2C5kSwOnkqkw9XQAJlhBL2JAf1/uCD8lVcXyMRg=";
    gpl3 =
      licence "gpl-3.0.txt" "https://www.gnu.org/licenses/gpl-3.0.txt"
        "sha256-OXLcl0T2SZ8Pmy2/dmlvKuetivmyPd5m1q+Gyd+zaYY=";
  };

  # A reasoner reached through the OWL API, with ElkClassify compiled against its jars and a
  # `<name>-classify` wrapper that selects it.
  owlapiReasoner =
    {
      pname,
      version,
      reasoner,
      jars,
      pins,
      licenceFiles,
      meta,
    }:
    let
      classpath = lib.concatMapStringsSep ":" toString jars;
    in
    runCommand "${pname}-${version}"
      {
        inherit pname version;
        nativeBuildInputs = [ jdk ];
        passthru = { inherit jdk jars; };
        meta = {
          platforms = jdk.meta.platforms;
          mainProgram = "${pname}-classify";
          sourceProvenance = [ lib.sourceTypes.binaryBytecode ];
        }
        // meta;
      }
      ''
        mkdir -p $out/bin $out/share/java/${pname} $out/share/${pname}/classes $out/share/licenses/${pname}
        for f in ${lib.concatMapStringsSep " " toString licenceFiles}; do
          cp "$f" "$out/share/licenses/${pname}/$(stripHash "$f")"
        done
        grep -hv '^#' ${
          lib.concatMapStringsSep " " (p: "${p}") pins
        } > $out/share/licenses/${pname}/jars.tsv
        for jar in ${lib.concatMapStringsSep " " toString jars}; do
          ln -s "$jar" "$out/share/java/${pname}/$(stripHash "$jar")"
        done
        cp ${../../scripts/elk-compare/ElkClassify.java} ElkClassify.java
        javac -proc:none -nowarn -cp ${classpath} -d $out/share/${pname}/classes ElkClassify.java
        # writeTaxonomy against a stub reasoner, with the OWL API alone
        java -cp $out/share/${pname}/classes:${classpath} ElkClassify --self-test
        cat > $out/bin/${pname}-classify <<EOF
        #!${stdenv.shell}
        # shellcheck disable=SC2086
        exec ${jdk}/bin/java \$JAVA_OPTS -cp $out/share/${pname}/classes:${classpath} \\
          ElkClassify --reasoner ${reasoner} "\$@"
        EOF
        chmod +x $out/bin/${pname}-classify
      '';

  elkJars = pinned ./elk.tsv;

  # Nemo 0.10.1's release archives, with the SHA-256 that each archive's .sha256sum names.
  nemoReleases = {
    x86_64-linux = {
      target = "x86_64-unknown-linux-gnu";
      hash = "sha256-L2WcOAP6e5pM4clEmAMDjgRWLXTYPlzTJcykO0/N1vk=";
    };
    aarch64-linux = {
      target = "aarch64-unknown-linux-gnu";
      hash = "sha256-jGtf9BjbpZmm/comjovbGLit6I1s5rbFTqZN3iTPyPo=";
    };
    aarch64-darwin = {
      target = "aarch64-apple-darwin";
      hash = "sha256-Sx6an0ajFauMWQ+wEky0cMM7UpO586Yd1TIlsx7XnxY=";
    };
  };
in
{
  # ELK 0.6.0 on OWL API 5.1.20 (nix/reasoners/elk.tsv).
  elk = owlapiReasoner {
    pname = "elk";
    version = "0.6.0";
    reasoner = "elk";
    jars = elkJars;
    pins = [ ./elk.tsv ];
    licenceFiles = with licences; [
      elk
      lgpl3
      gpl3
    ];
    meta = {
      description = "ELK, the OWL 2 EL reasoner, through the OWL API for the classification benchmark";
      homepage = "https://github.com/liveontologies/elk-reasoner";
      license = with lib.licenses; [
        asl20
        lgpl3Plus
      ];
    };
  };

  # HermiT 1.4.5.519 on the OWL API 5.1.20 jars of ELK's set (nix/reasoners/hermit.tsv).
  hermit = owlapiReasoner {
    pname = "hermit";
    version = "1.4.5.519";
    reasoner = "hermit";
    jars = elkJars ++ pinned ./hermit.tsv;
    pins = [
      ./elk.tsv
      ./hermit.tsv
    ];
    licenceFiles = with licences; [
      elk
      lgpl3
      gpl3
    ];
    meta = {
      description = "HermiT, the OWL 2 DL reasoner, through the OWL API for the classification benchmark";
      homepage = "http://www.hermit-reasoner.com/";
      license = with lib.licenses; [
        lgpl3Plus
        asl20
      ];
    };
  };

  # Whelk 1.2.1 on OWL API 4.5.22 and Scala 2.13 (nix/reasoners/whelk.tsv).
  whelk = owlapiReasoner {
    pname = "whelk";
    version = "1.2.1";
    reasoner = "whelk";
    jars = pinned ./whelk.tsv;
    pins = [ ./whelk.tsv ];
    licenceFiles = with licences; [
      whelk
      lgpl3
      gpl3
    ];
    meta = {
      description = "Whelk, the OWL 2 EL reasoner in Scala, through the OWL API for the classification benchmark";
      homepage = "https://github.com/balhoff/whelk";
      license = with lib.licenses; [
        bsd3
        asl20
        lgpl3Plus
      ];
    };
  };

  # ROBOT's release jar, which bundles OWL API 4.5.29, ELK 0.6.0, HermiT and Whelk.
  robot =
    let
      version = "1.9.11";
      jar = fetchurl {
        url = "https://github.com/ontodev/robot/releases/download/v${version}/robot.jar";
        hash = "sha256-U80nXI5MDa0fwsHDQXSHZ5XNfWtpJuFESty7DLPHqyY=";
      };
    in
    runCommand "robot-${version}"
      {
        pname = "robot";
        inherit version;
        passthru = { inherit jdk; };
        meta = {
          description = "ROBOT, the OBO ontology release tool, from its pinned release jar";
          homepage = "https://robot.obolibrary.org/";
          changelog = "https://github.com/ontodev/robot/releases/tag/v${version}";
          license = lib.licenses.bsd3;
          sourceProvenance = [ lib.sourceTypes.binaryBytecode ];
          platforms = jdk.meta.platforms;
          mainProgram = "robot";
        };
      }
      ''
        install -Dm644 ${jar} $out/share/java/robot/robot.jar
        install -Dm644 ${licences.robot} $out/share/licenses/robot/LICENSE.txt
        mkdir -p $out/bin
        cat > $out/bin/robot <<EOF
        #!${stdenv.shell}
        # shellcheck disable=SC2086
        exec ${jdk}/bin/java \$ROBOT_JAVA_ARGS -jar $out/share/java/robot/robot.jar "\$@"
        EOF
        chmod +x $out/bin/robot
      '';
}
// lib.optionalAttrs (nemoReleases ? ${stdenv.hostPlatform.system}) {
  # Nemo's release binary, the reference OWL 2 RL materializer of docs/benchmarks/reasoning
  # (scripts/rl-reference). Nemo builds with a nightly Rust toolchain, so the package takes
  # the project's release archive for the host, pinned by the hash that the release
  # publishes beside it. The Linux binaries link only the C library and libgcc_s.
  nemo = stdenv.mkDerivation (finalAttrs: {
    pname = "nemo";
    version = "0.10.1";

    src = fetchurl {
      url = "https://github.com/knowsys/nemo/releases/download/v${finalAttrs.version}/nemo_v${finalAttrs.version}_${
        nemoReleases.${stdenv.hostPlatform.system}.target
      }.tar.gz";
      hash = nemoReleases.${stdenv.hostPlatform.system}.hash;
    };

    nativeBuildInputs = lib.optionals stdenv.hostPlatform.isLinux [ autoPatchelfHook ];
    buildInputs = lib.optionals stdenv.hostPlatform.isLinux [ stdenv.cc.cc.lib ];
    dontBuild = true;
    dontStrip = true;

    installPhase = ''
      runHook preInstall
      install -Dm755 nmo $out/bin/nmo
      install -Dm644 -t $out/share/licenses/nemo LICENSE-APACHE LICENSE-MIT
      install -Dm644 README.md $out/share/doc/nemo/README.md
      runHook postInstall
    '';

    doInstallCheck = true;
    installCheckPhase = ''
      $out/bin/nmo --version | grep -qx 'nemo-cli ${finalAttrs.version}'
    '';

    meta = {
      description = "Nemo, the Datalog rule engine, from its pinned release binary";
      homepage = "https://github.com/knowsys/nemo";
      changelog = "https://github.com/knowsys/nemo/releases/tag/v${finalAttrs.version}";
      license = with lib.licenses; [
        asl20
        mit
      ];
      sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
      platforms = builtins.attrNames nemoReleases;
      mainProgram = "nmo";
    };
  });
}
// lib.optionalAttrs (stdenv.hostPlatform.system == "x86_64-linux") {
  # Konclude's release binary, which links Qt statically and the C libraries dynamically.
  # It asks for libpcre.so.3, Debian's name for PCRE 8's libpcre.so.1.
  konclude = stdenv.mkDerivation (finalAttrs: {
    pname = "konclude";
    version = "0.7.0-1138";

    # the release's zip file, whose hash this is
    src = fetchurl {
      url = "https://github.com/konclude/Konclude/releases/download/v${finalAttrs.version}/Konclude-v${finalAttrs.version}-Linux-x64-GCC-Static-Qt5.12.10.zip";
      hash = "sha256-NyFsGPRagBZa7B52ru/WLZM9aj+PE0xaygcHxnb9p24=";
    };

    nativeBuildInputs = [
      autoPatchelfHook
      unzip
    ];
    buildInputs = [
      stdenv.cc.cc.lib
      glib
      xz
      zlib
    ];
    dontBuild = true;
    dontStrip = true;

    installPhase = ''
      runHook preInstall
      install -Dm755 Binaries/Konclude $out/bin/Konclude
      mkdir -p $out/lib
      ln -s ${lib.getLib pcre}/lib/libpcre.so.1 $out/lib/libpcre.so.3
      install -Dm644 -t $out/share/konclude/Configs Configs/*.xml
      install -Dm644 -t $out/share/licenses/konclude GPL.txt LGPL-3.0.txt
      install -Dm644 Readme.md $out/share/doc/konclude/Readme.md
      runHook postInstall
    '';

    meta = {
      description = "Konclude, the OWL 2 DL reasoner, from its pinned Linux release binary";
      homepage = "https://www.derivo.de/en/products/konclude/";
      changelog = "https://github.com/konclude/Konclude/releases/tag/v${finalAttrs.version}";
      license = lib.licenses.lgpl3Only;
      sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
      platforms = [ "x86_64-linux" ];
      mainProgram = "Konclude";
    };
  });
}
