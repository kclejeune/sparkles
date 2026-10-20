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
in
engines
// {
  # The data generators of the WatDiv and SP2Bench runners, built from checksum-pinned
  # releases (scripts/bench-watdiv.sh, scripts/bench-sp2b.sh).
  watdiv-gen = import ../scripts/bench-watdiv/watdiv.nix { inherit pkgs; };
  sp2b-gen = import ../scripts/bench-sp2b/sp2b.nix { inherit pkgs; };
  benchmark-engines = pkgs.buildEnv {
    name = "sparkles-benchmark-engines";
    paths = builtins.attrValues engines;
    pathsToLink = [ "/bin" ];
  };
}
