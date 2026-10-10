# A model store (spec F12) built in the Nix store from snapshot manifests: each file of
# each snapshot is a fixed-output `fetchurl` of the Hub's resolve URL, checked against
# the SHA-256 that the manifest records. The result has the layout of
# `sparkles models pull`, `<owner>/<name>/<revision>/` with the files and
# `sparkles-manifest.json`, and serves as `serve --models-dir` or
# `services.sparkles.models.dir`.
#
# A manifest is the `sparkles-manifest.json` that `sparkles models pull` writes (a path,
# or the same value as an attribute set): `repo`, a pinned `revision` and `files` with
# `path`, `size` and `sha256`. Gated or private repositories need a mirror or a
# `fetchurl` with credentials, which this helper does not provide.
{
  lib,
  fetchurl,
  runCommandLocal,
}:
{
  manifests,
  endpoint ? "https://huggingface.co",
  name ? "sparkles-models",
}:
let
  read = m: if builtins.isAttrs m then m else lib.importJSON m;
  pinned = r: builtins.match "[0-9a-f]{40}" r != null;

  snapshot =
    m':
    let
      m = read m';
      check =
        lib.assertMsg (
          builtins.match "[A-Za-z0-9._-]+/[A-Za-z0-9._-]+" m.repo != null
        ) "modelSnapshots: invalid repository ${m.repo}"
        && lib.assertMsg (pinned m.revision) "modelSnapshots: ${m.repo} needs a pinned 40-character revision, not ${m.revision}";
      files = map (f: {
        inherit (f) path;
        src = fetchurl {
          url = "${endpoint}/${m.repo}/resolve/${m.revision}/${f.path}";
          sha256 = f.sha256;
          name = "${lib.replaceStrings [ "/" ] [ "-" ] m.repo}-${lib.replaceStrings [ "/" ] [ "-" ] f.path}";
        };
      }) m.files;
      manifest = builtins.toFile "sparkles-manifest.json" (
        builtins.toJSON {
          inherit (m) repo revision;
          files = map (f: { inherit (f) path size sha256; }) m.files;
        }
      );
      dir = "$out/${m.repo}/${m.revision}";
    in
    assert check;
    ''
      mkdir -p ${dir}
      ${lib.concatMapStrings (f: ''
        mkdir -p "$(dirname ${dir}/${f.path})"
        ln -s ${f.src} ${dir}/${f.path}
      '') files}
      cp ${manifest} ${dir}/sparkles-manifest.json
    '';
in
runCommandLocal name { } ''
  mkdir -p $out
  ${lib.concatMapStrings snapshot manifests}
''
