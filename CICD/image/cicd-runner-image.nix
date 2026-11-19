# Reproducible offline OCI image builder for `docker.io/pits/cicd-runner:v1.1`
# Extends PITS TestPool toolchain with Gitea Actions runner (`act_runner`),
# Rust 1.95.0 + `x86_64-pc-windows-gnu` (MinGW-w64 cross-compile), `kubectl`, `helm`,
# `skopeo`, `devspace`, `tea`, and Python+PyYAML for zero-disk-wear CI/CD on DDR5 tmpfs.
{
  pkgs,
  assetsDir,
  cargoVendorDir ? /var/lib/pits/portal-vendor,
  portalNodeModulesDir,
  imageTag ? "v1.1",
}:

let
  fontsConf = pkgs.makeFontsConf {
    fontDirectories = [ pkgs.dejavu_fonts ];
  };

  goVendorSrc = builtins.path {
    path = "${assetsDir}/go-vendor";
    name = "pits-go-vendor";
  };

  pythonEnv = pkgs.python3.withPackages (ps: [ ps.pyyaml ]);

  rustDistTarball = pkgs.fetchurl {
    urls = [
      "https://mirrors.ustc.edu.cn/rust-static/dist/rust-1.95.0-x86_64-unknown-linux-gnu.tar.xz"
      "https://static.rust-lang.org/dist/rust-1.95.0-x86_64-unknown-linux-gnu.tar.xz"
    ];
    sha256 = "2e0338f18ecbaa4a0f631b9e80e8b8e26bb6fe77dd5454fba8a70cf96c1e84a1";
  };

  rustStdWinTarball = pkgs.fetchurl {
    urls = [
      "https://mirrors.ustc.edu.cn/rust-static/dist/rust-std-1.95.0-x86_64-pc-windows-gnu.tar.xz"
      "https://static.rust-lang.org/dist/rust-std-1.95.0-x86_64-pc-windows-gnu.tar.xz"
    ];
    sha256 = "f57e045016a04130125fb43295d95f9ad2bebc296150eadb031dbf5167ad12bd";
  };

  rustToolchain = pkgs.stdenv.mkDerivation {
    pname = "pits-rust-toolchain";
    version = "1.95.0";
    dontUnpack = true;
    nativeBuildInputs = [
      pkgs.autoPatchelfHook
      pkgs.xz
      pkgs.gnutar
    ];
    buildInputs = [
      pkgs.stdenv.cc.cc.lib
      pkgs.zlib
      pkgs.openssl
      pkgs.curl
    ];
    autoPatchelfIgnoreMissingDeps = [ "libgcc_s.so.1" ];
    installPhase = ''
      mkdir -p "$out" work-rust work-std
      tar -xJf "${rustDistTarball}" -C work-rust --strip-components=1
      tar -xJf "${rustStdWinTarball}" -C work-std --strip-components=1
      bash work-rust/install.sh --prefix="$out" --disable-ldconfig \
        --components=rustc,cargo,clippy-preview,rust-std-x86_64-unknown-linux-gnu
      bash work-std/install.sh --prefix="$out" --disable-ldconfig \
        --components=rust-std-x86_64-pc-windows-gnu
    '';
  };

  pitsVendorCache = pkgs.runCommand "pits-cicd-vendor-cache" { } ''
    mkdir -p "$out/opt/pits-cache/cargo-vendor" \
             "$out/opt/pits-cache/go-vendor" \
             "$out/opt/pits-cache/portal-node-modules" \
             "$out/opt/pits-cache/mingw-pthreads/lib"

    cat > "$out/opt/pits-cache/cargo-config.toml" <<'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "/opt/pits-cache/cargo-vendor"

[net]
offline = true

[target.x86_64-pc-windows-gnu]
linker = "x86_64-w64-mingw32-gcc"
ar = "x86_64-w64-mingw32-ar"
rustflags = ["-L", "native=/opt/pits-cache/mingw-pthreads/lib"]
EOF

    cp -a "${cargoVendorDir}/." "$out/opt/pits-cache/cargo-vendor/"
    cp -a "${goVendorSrc}/." "$out/opt/pits-cache/go-vendor/"
    cp -a "${portalNodeModulesDir}/." "$out/opt/pits-cache/portal-node-modules/"
    cp -a "${pkgs.pkgsCross.mingwW64.windows.pthreads}/lib/." "$out/opt/pits-cache/mingw-pthreads/lib/"
  '';

  runnerScripts = pkgs.runCommand "pits-cicd-runner-scripts" { } ''
    mkdir -p "$out/usr/local/bin"

    install -Dm755 "${assetsDir}/devspacehelper" "$out/usr/local/bin/devspacehelper"
    install -Dm755 "${assetsDir}/devspace" "$out/usr/local/bin/devspace"
    install -Dm755 "${assetsDir}/tea" "$out/usr/local/bin/tea"
    install -Dm755 "${assetsDir}/act_runner" "$out/usr/local/bin/act_runner"
    install -Dm755 "${assetsDir}/cargo-nextest" "$out/usr/local/bin/cargo-nextest"
    install -Dm755 "${assetsDir}/runner-verify-shard.sh" "$out/usr/local/bin/runner-verify-shard"
    install -Dm755 "${assetsDir}/portal-headless-e2e.mjs" "$out/usr/local/bin/portal-headless-e2e.mjs"
    install -Dm755 "${assetsDir}/cicd-runner-entrypoint.sh" "$out/usr/local/bin/cicd-runner-entrypoint"

    cat > "$out/usr/local/bin/runner-entrypoint" <<'EOF'
#!/bin/sh
set -eu
mkdir -p /tmp /ramdisk/home /ramdisk/work /ramdisk/cargo-home /ramdisk/cargo-target /ramdisk/go-cache /ramdisk/reports
chmod 1777 /tmp 2>/dev/null || true
cp -f /usr/local/bin/devspacehelper /tmp/devspacehelper
chmod 0755 /tmp/devspacehelper
if [ "$#" -gt 0 ]; then
  exec "$@"
fi
exec sleep infinity
EOF
    chmod 0755 "$out/usr/local/bin/runner-entrypoint"
  '';
in
pkgs.dockerTools.buildLayeredImage {
  name = "docker.io/pits/cicd-runner";
  tag = imageTag;
  maxLayers = 60;

  contents = [
    pkgs.dockerTools.usrBinEnv
    pkgs.dockerTools.binSh
    pkgs.dockerTools.caCertificates
    pkgs.dockerTools.fakeNss

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
    pkgs.openssh
    pythonEnv

    pkgs.stdenv.cc
    pkgs.bintools
    pkgs.pkgsCross.mingwW64.stdenv.cc
    pkgs.pkgsCross.mingwW64.stdenv.cc.bintools

    rustToolchain
    pkgs.sqlite

    pkgs.go

    pkgs.nodejs
    pkgs.chromium
    pkgs.fontconfig
    pkgs.dejavu_fonts

    pkgs.kubectl
    pkgs.kubernetes-helm
    pkgs.skopeo

    pitsVendorCache
    runnerScripts
  ];

  extraCommands = ''
    mkdir -p tmp ramdisk var/tmp etc/containers
    chmod 1777 tmp var/tmp
    cat > etc/containers/policy.json <<'EOF'
{"default":[{"type":"insecureAcceptAnything"}]}
EOF
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
