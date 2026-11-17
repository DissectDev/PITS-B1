# Reproducible offline OCI image builder for `docker.io/pits/test-runner:v1`
# Built natively on NixOS `pits-core` using `PITS-B1#core` nixpkgs — zero Docker daemon required.
{
  pkgs,
  assetsDir,
  cargoVendorDir ? /var/lib/pits/portal-vendor,
  portalNodeModulesDir,
  imageTag ? "v1.2",
}:

let
  fontsConf = pkgs.makeFontsConf {
    fontDirectories = [ pkgs.dejavu_fonts ];
  };

  goVendorSrc = builtins.path {
    path = "${assetsDir}/go-vendor";
    name = "pits-go-vendor";
  };

  pitsVendorCache = pkgs.runCommand "pits-test-vendor-cache" { } ''
    mkdir -p "$out/opt/pits-cache/cargo-vendor" \
             "$out/opt/pits-cache/go-vendor" \
             "$out/opt/pits-cache/portal-node-modules"

    cat > "$out/opt/pits-cache/cargo-config.toml" <<'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "/opt/pits-cache/cargo-vendor"

[net]
offline = true
EOF

    cp -a "${cargoVendorDir}/." "$out/opt/pits-cache/cargo-vendor/"
    cp -a "${goVendorSrc}/." "$out/opt/pits-cache/go-vendor/"
    cp -a "${portalNodeModulesDir}/." "$out/opt/pits-cache/portal-node-modules/"
  '';

  runnerScripts = pkgs.runCommand "pits-test-runner-scripts" { } ''
    mkdir -p "$out/usr/local/bin"

    install -Dm755 "${assetsDir}/devspacehelper" "$out/usr/local/bin/devspacehelper"
    install -Dm755 "${assetsDir}/runner-verify-shard.sh" "$out/usr/local/bin/runner-verify-shard"
    install -Dm755 "${assetsDir}/portal-headless-e2e.mjs" "$out/usr/local/bin/portal-headless-e2e.mjs"

    cat > "$out/usr/local/bin/runner-entrypoint" <<'EOF'
#!/bin/sh
set -eu
mkdir -p /tmp /ramdisk/home /ramdisk/work /ramdisk/cargo-home /ramdisk/cargo-target /ramdisk/go-cache /ramdisk/reports
chmod 1777 /tmp 2>/dev/null || true
cp -f /usr/local/bin/devspacehelper /tmp/devspacehelper
chmod 0755 /tmp/devspacehelper
if [ "''${PITS_PREWARM_PORTAL_DEPS:-0}" = "1" ] && [ ! -d /ramdisk/portal-node-modules ] && [ ! -d /ramdisk/work/portal/node_modules ]; then
  (
    STORE_NM="$(dirname "$(dirname "$(readlink -f /opt/pits-cache/portal-node-modules/vite/package.json)")")"
    cp -a "$STORE_NM" /ramdisk/portal-node-modules.tmp &&
    chmod -R u+w /ramdisk/portal-node-modules.tmp &&
    mv /ramdisk/portal-node-modules.tmp /ramdisk/portal-node-modules
  ) >/dev/null 2>&1 &
fi
if [ "$#" -gt 0 ]; then
  exec "$@"
fi
exec sleep infinity
EOF
    chmod 0755 "$out/usr/local/bin/runner-entrypoint"
  '';
in
pkgs.dockerTools.buildLayeredImage {
  name = "docker.io/pits/test-runner";
  tag = imageTag;
  maxLayers = 60;

  contents = [
    pkgs.dockerTools.usrBinEnv
    pkgs.dockerTools.binSh
    pkgs.dockerTools.caCertificates
    pkgs.dockerTools.fakeNss

    # Core POSIX & process utilities required by DevSpace sync & scripts
    pkgs.bashInteractive
    pkgs.coreutils
    pkgs.findutils
    pkgs.gnugrep
    pkgs.gnused
    pkgs.gawk
    pkgs.gnutar
    pkgs.gzip
    pkgs.xz
    pkgs.procps
    pkgs.iproute2
    pkgs.curl
    pkgs.jq
    pkgs.git
    pkgs.which

    # C/C++ toolchain & libc headers (required by `rustc` linker & `go test -race` ThreadSanitizer)
    pkgs.stdenv.cc
    pkgs.bintools

    # Shard 1: Rust + cargo-nextest (plus sqlite runtime for SQLite WAL sourcing tests)
    pkgs.rustc
    pkgs.cargo
    pkgs.cargo-nextest
    pkgs.clippy
    pkgs.sqlite

    # Shard 2: Go (Linux native sysfs/netns/ioctl/GPT + race detector)
    pkgs.go

    # Shard 3: Node.js + Headless Chromium E2E
    pkgs.nodejs
    pkgs.chromium
    pkgs.fontconfig
    pkgs.dejavu_fonts

    # Pre-baked offline vendor caches & DevSpace/shard runner scripts (separate layers)
    pitsVendorCache
    runnerScripts
  ];

  extraCommands = ''
    mkdir -p tmp ramdisk var/tmp
    chmod 1777 tmp var/tmp
  '';

  config = {
    Entrypoint = [ "/usr/local/bin/runner-entrypoint" ];
    WorkingDir = "/ramdisk/work";
    Env = [
      "PATH=/usr/local/bin:/bin:/usr/bin"
      "HOME=/ramdisk/home"
      "TMPDIR=/tmp"
      "FONTCONFIG_FILE=${fontsConf}"
      "SSL_CERT_FILE=/etc/ssl/certs/ca-bundle.crt"
      "CC=cc"
      "CXX=c++"
      "CARGO_TARGET_DIR=/ramdisk/cargo-target"
      "GOCACHE=/ramdisk/go-cache"
      "LD_LIBRARY_PATH=${pkgs.sqlite.out}/lib"
    ];
  };
}
