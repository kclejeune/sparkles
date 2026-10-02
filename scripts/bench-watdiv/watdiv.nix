# The WatDiv v0.6 data and query generator (https://dsg.uwaterloo.ca/watdiv/), built from
# its checksum-pinned source release for scripts/bench-watdiv.sh. WatDiv is free to use
# provided that publications cite Aluç et al., ISWC 2014. Its source is downloaded at build
# time, never vendored.
#
# The release seeds its random generators from the clock and std::random_device, takes the
# upper bound of its date literals from today's date, and reads the system word list. The
# patches below make a run a function of its inputs. WATDIV_SEED (default 1) seeds every
# generator, WATDIV_DATE (default 2015-01-01) replaces today's date, and the word list is
# Ubuntu's wamerican 7.1-1, the one the WatDiv site points to. The data files are read
# from the package rather than from ../../files, so the binary runs from any directory.
# -fsigned-char keeps non-ASCII bytes of the word list mapped the same way on x86_64 and
# aarch64.
{ pkgs }:
pkgs.stdenv.mkDerivation {
  pname = "watdiv";
  version = "0.6";
  src = pkgs.fetchurl {
    url = "https://dsg.uwaterloo.ca/watdiv/watdiv_v06.tar";
    hash = "sha256-+42TC3Sz+8j5SBAb+vZYqQ0vdAAvH+/aRlxF/9M6cdI=";
  };
  words = pkgs.fetchurl {
    url = "https://launchpadlibrarian.net/83495804/wamerican_7.1-1_all.deb";
    hash = "sha256-Fl+blr55f0jSNCPqlA+Ej0QEzV+Rj5ns2WlWATLcWso=";
  };
  buildInputs = [ pkgs.boost ];
  postPatch = ''
    cat > include/watdiv_fixed.h <<'EOF'
    #include <cstdlib>
    #include <string>
    static inline unsigned watdiv_seed() {
      const char *s = std::getenv("WATDIV_SEED");
      return s ? (unsigned) std::strtoul(s, 0, 10) : 1u;
    }
    static inline std::string watdiv_date() {
      const char *s = std::getenv("WATDIV_DATE");
      return std::string(s ? s : "2015-01-01");
    }
    EOF
    substituteInPlace src/model.cpp \
      --replace-fail 'static_cast<unsigned> (time(0))' 'watdiv_seed()' \
      --replace-fail 'srand (time(NULL));' 'srand (watdiv_seed());' \
      --replace-fail 'boost::gregorian::to_iso_extended_string(cur_date)' 'watdiv_date()' \
      --replace-fail '"/usr/share/dict/words"' "\"$out/share/watdiv/files/words\"" \
      --replace-fail '"../../files/firstnames.txt"' "\"$out/share/watdiv/files/firstnames.txt\"" \
      --replace-fail '"../../files/lastnames.txt"' "\"$out/share/watdiv/files/lastnames.txt\""
    substituteInPlace src/statistics.cpp \
      --replace-fail 'srand (time(NULL));' 'srand (watdiv_seed());'
    substituteInPlace src/volatility_gen.cpp \
      --replace-fail 'new mt19937 (rd())' 'new mt19937 (watdiv_seed())'
  '';
  buildPhase = ''
    runHook preBuild
    make release CXX=c++ LD=c++ INC=-Iinclude LIBDIR= LIB_RELEASE= \
      CFLAGS_RELEASE="-O3 -std=c++14 -w -fsigned-char -include include/watdiv_fixed.h"
    runHook postBuild
  '';
  installPhase = ''
    runHook preInstall
    install -Dm755 bin/Release/watdiv $out/bin/watdiv
    mkdir -p $out/share/watdiv
    cp -r model testsuite files $out/share/watdiv/
    mkdir words && cd words
    $AR x $words
    tar xf data.tar.gz ./usr/share/dict/american-english
    install -Dm644 usr/share/dict/american-english $out/share/watdiv/files/words
    runHook postInstall
  '';
  meta.description = "WatDiv data and query generator, patched for reproducible output";
}
