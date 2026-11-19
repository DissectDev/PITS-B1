# Reproducible offline OCI image builder for `docker.io/pits/ai-store:v0.1.0`
# Built natively on NixOS `pits-core` using `PITS-B1#core` nixpkgs.
{
  pkgs,
  aiStoreBin,
  imageTag ? "v0.1.0",
}:

let
  binPkg = pkgs.runCommand "pits-ai-store-bin" { } ''
    mkdir -p "$out/usr/local/bin" "$out/lib" "$out/var/lib/ai-store/ws" "$out/var/cache/ai-store/nvme"
    install -Dm755 "${aiStoreBin}" "$out/usr/local/bin/ai-store"
  '';
in
pkgs.dockerTools.buildLayeredImage {
  name = "docker.io/pits/ai-store";
  tag = imageTag;
  maxLayers = 40;

  contents = [
    pkgs.dockerTools.usrBinEnv
    pkgs.dockerTools.binSh
    pkgs.dockerTools.caCertificates
    pkgs.dockerTools.fakeNss
    pkgs.coreutils
    pkgs.sqlite
    binPkg
  ];

  config = {
    Entrypoint = [ "/usr/local/bin/ai-store" ];
    Cmd = [ "serve" "--profile" "/etc/ai-store/project-profile.yaml" ];
    ExposedPorts = {
      "8787/tcp" = { };
    };
    Env = [
      "PATH=/usr/local/bin:/bin:/usr/bin"
      "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
      "LD_LIBRARY_PATH=${pkgs.sqlite.out}/lib"
      "AI_STORE_HOME=/var/lib/ai-store"
      "RUST_LOG=info"
    ];
  };
}
