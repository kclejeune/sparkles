# The reference reasoners that docs/benchmarks/el-classification compares the classifier of
# sparkles-owl with: ROBOT, ELK, HermiT, Whelk and Konclude. Each comes from a
# checksum-pinned release, the Java tools from Maven Central or the project's GitHub
# releases and Konclude from its Linux x86_64 release binary. They are benchmark-only
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

  # A reasoner reached through the OWL API, with ElkClassify compiled against its jars and a
  # `<name>-classify` wrapper that selects it.
  owlapiReasoner =
    {
      pname,
      version,
      reasoner,
      jars,
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
        mkdir -p $out/bin $out/share/java/${pname} $out/share/${pname}/classes
        for jar in ${lib.concatMapStringsSep " " toString jars}; do
          ln -s "$jar" "$out/share/java/${pname}/$(stripHash "$jar")"
        done
        cp ${../../scripts/elk-compare/ElkClassify.java} ElkClassify.java
        javac -proc:none -nowarn -cp ${classpath} -d $out/share/${pname}/classes ElkClassify.java
        cat > $out/bin/${pname}-classify <<EOF
        #!${stdenv.shell}
        # shellcheck disable=SC2086
        exec ${jdk}/bin/java \$JAVA_OPTS -cp $out/share/${pname}/classes:${classpath} \\
          ElkClassify --reasoner ${reasoner} "\$@"
        EOF
        chmod +x $out/bin/${pname}-classify
      '';

  elkJars = pinned ./elk.tsv;
in
{
  # ELK 0.6.0 on OWL API 5.1.20 (nix/reasoners/elk.tsv).
  elk = owlapiReasoner {
    pname = "elk";
    version = "0.6.0";
    reasoner = "elk";
    jars = elkJars;
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
        mkdir -p $out/bin
        cat > $out/bin/robot <<EOF
        #!${stdenv.shell}
        # shellcheck disable=SC2086
        exec ${jdk}/bin/java \$ROBOT_JAVA_ARGS -jar $out/share/java/robot/robot.jar "\$@"
        EOF
        chmod +x $out/bin/robot
      '';
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
