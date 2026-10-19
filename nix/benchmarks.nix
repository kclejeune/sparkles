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
  benchmark-engines = pkgs.buildEnv {
    name = "sparkles-benchmark-engines";
    paths = builtins.attrValues engines;
    pathsToLink = [ "/bin" ];
  };
}
