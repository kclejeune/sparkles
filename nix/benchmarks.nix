# Existing nixpkgs definitions retain the repository's flake.lock versions and toolchains.
{ pkgs }:
let
  lib = pkgs.lib;
  engines = {
    inherit (pkgs)
      apache-jena
      apache-jena-fuseki
      qlever
      oxigraph
      ;
  }
  // lib.optionalAttrs (builtins.elem pkgs.stdenv.hostPlatform.system [
    "x86_64-linux"
    "aarch64-linux"
    "aarch64-darwin"
  ]) { fluree = pkgs.callPackage ./fluree.nix { }; };
  reasoners = pkgs.callPackage ./reasoners { };
in
engines
// {
  # The data generators of the WatDiv and SP2Bench runners, built from checksum-pinned
  # releases (scripts/bench-watdiv.sh, scripts/bench-sp2b.sh).
  watdiv-gen = import ../scripts/bench-watdiv/watdiv.nix { inherit pkgs; };
  sp2b-gen = import ../scripts/bench-sp2b/sp2b.nix { inherit pkgs; };
  # The reference reasoners of the classification benchmark (nix/reasoners,
  # scripts/elk-compare/elk.sh), with Konclude on x86_64-linux only.
  robot = reasoners.robot;
  elk = reasoners.elk;
  hermit = reasoners.hermit;
  whelk = reasoners.whelk;
  benchmark-engines = pkgs.buildEnv {
    name = "sparkles-benchmark-engines";
    paths = builtins.attrValues engines;
    pathsToLink = [ "/bin" ];
  };
}
// lib.optionalAttrs (reasoners ? konclude) {
  konclude = reasoners.konclude;
}
# The reference OWL 2 RL materializer of the reasoning benchmark (scripts/rl-reference).
// lib.optionalAttrs (reasoners ? nemo) {
  nemo = reasoners.nemo;
}
