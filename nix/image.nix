# An OCI image of the server, built by Nix without a Dockerfile: `nix build .#image`
# writes a script that streams the image to stdout, so
#
#   nix build .#image && ./result | docker load
#
# loads it as `sparkles:<version>`, and `./result | skopeo copy docker-archive:/dev/stdin
# docker://REGISTRY/sparkles:TAG` pushes it without Docker. The layout follows the
# Dockerfile's image: the server as uid 10001, the data in /data, port 3030, and the same
# default command. With `ocr`, the image also holds PDFium and ONNX Runtime from nixpkgs,
# found through PDFIUM_LIB_PATH and ORT_DYLIB_PATH, and the server is built with
# `pdf-ocr`; `--pdf-ocr-models DIR` turns OCR on.
{
  lib,
  dockerTools,
  runCommand,
  cacert,
  sparkles,
  pdfium ? null,
  onnxruntime ? null,
  ocr ? false,
}:
let
  version = sparkles.version;
  # /etc/passwd and /etc/group with the server's user, and the directories it writes
  rootfs = runCommand "sparkles-image-rootfs" { } ''
    mkdir -p $out/etc $out/data $out/tmp
    echo 'root:x:0:0:root:/root:/sbin/nologin' > $out/etc/passwd
    echo 'sparkles:x:10001:10001:sparkles:/data:/sbin/nologin' >> $out/etc/passwd
    echo 'root:x:0:' > $out/etc/group
    echo 'sparkles:x:10001:' >> $out/etc/group
  '';
in
dockerTools.streamLayeredImage {
  name = "sparkles";
  tag = if ocr then "${version}-ocr" else version;
  contents = [
    sparkles
    cacert
    rootfs
  ]
  ++ lib.optionals ocr [
    pdfium
    onnxruntime
  ];
  # the server's writable directories belong to its user; /tmp is writable by all
  fakeRootCommands = ''
    chown 10001:10001 data
    chmod 1777 tmp
  '';
  enableFakechroot = false;
  config = {
    User = "10001:10001";
    WorkingDir = "/data";
    Volumes."/data" = { };
    ExposedPorts."3030/tcp" = { };
    StopSignal = "SIGTERM";
    Entrypoint = [ (lib.getExe sparkles) ];
    Cmd = [
      "serve"
      "--data"
      "/data"
      "--host"
      "0.0.0.0"
      "--port"
      "3030"
    ];
    Env = [
      "SSL_CERT_FILE=${cacert}/etc/ssl/certs/ca-bundle.crt"
      "TMPDIR=/tmp"
    ]
    ++ lib.optionals ocr [
      "PDFIUM_LIB_PATH=${pdfium}/lib/libpdfium.so"
      "ORT_DYLIB_PATH=${lib.getLib onnxruntime}/lib/libonnxruntime.so"
    ];
    # `sparkles ping` asks for GET /$/ready, as the Dockerfile's sparkles-healthcheck does
    Healthcheck = {
      Test = [
        "CMD"
        (lib.getExe sparkles)
        "ping"
        "--quiet"
        "--timeout"
        "4"
        "127.0.0.1:3030"
      ];
      Interval = 30000000000;
      Timeout = 5000000000;
      StartPeriod = 300000000000;
      Retries = 3;
    };
    Labels = {
      "org.opencontainers.image.title" = if ocr then "Sparkles (OCR)" else "Sparkles";
      "org.opencontainers.image.source" = "https://github.com/kclejeune/sparkles";
      "org.opencontainers.image.licenses" = "Apache-2.0";
      "org.opencontainers.image.version" = version;
    };
  };
}
