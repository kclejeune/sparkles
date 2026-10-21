# The SP2Bench v1.01 data generator and queries (https://dbis.informatik.uni-freiburg.de/),
# built from the checksum-pinned release for scripts/bench-sp2b.sh. SP2Bench is published
# under the BSD license (the COPYING file of the release), which allows this use. The
# release is downloaded at build time and never vendored.
#
# The generator seeds its random numbers with a fixed value, so its output is a function of
# its size. v1.00 produced different documents on 32-bit and 64-bit systems (the generator
# stores values in `unsigned long`), and v1.01 fixes that. The release's Makefile builds
# with -m32 and -O2 and warns against -O3, because floating-point optimizations at -O3
# change the documents. This derivation builds a 64-bit binary at -O2.
{ pkgs }:
pkgs.stdenv.mkDerivation {
  pname = "sp2b";
  version = "1.01";
  src = pkgs.fetchurl {
    url = "https://dbis.informatik.uni-freiburg.de/content/projects/SP2B/docs/sp2b-v1_01-full.tar.gz";
    hash = "sha256-ZkBQbPLi0ULqNDWxuMw5mYeTSevNGjzcYB24MfXLPXM=";
  };
  sourceRoot = "sp2b/src";
  # Darwin uses 64-bit file offsets with the standard stdio API and has no fopen64.
  postPatch = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
    substituteInPlace generator.cpp --replace-fail 'fopen64(' 'fopen('
  '';
  buildPhase = ''
    runHook preBuild
    make CPP="c++ -O2 -w -include cstring -include cstdlib -include cstdio -include climits"
    runHook postBuild
  '';
  installPhase = ''
    runHook preInstall
    install -Dm755 sp2b_gen $out/bin/sp2b_gen
    mkdir -p $out/share/sp2b
    cp familynames.txt givennames.txt titlewords.txt $out/share/sp2b/
    cp ../COPYING $out/share/sp2b/COPYING
    cp -r ../queries $out/share/sp2b/queries
    runHook postInstall
  '';
  meta.description = "SP2Bench data generator and queries, 64-bit build";
}
