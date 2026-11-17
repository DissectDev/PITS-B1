# Offline reproducible Nix derivation for `pitsctl` (Rust + embedded Vite Backstage v1.56 SPA).
#
# Expected paths:
#   - `src` (`/var/lib/pits/portal-src`): `client/` + built `portal/dist/`
#   - `vendorSrc` (`/var/lib/pits/portal-vendor`): vendored Cargo crates keyed by `Cargo.lock`
{
  lib,
  stdenv,
  rustc,
  cargo,
  src ? /var/lib/pits/portal-src,
  vendorSrc ? /var/lib/pits/portal-vendor,
}:

stdenv.mkDerivation {
  pname = "pitsctl";
  version = "1.1.0";
  inherit src;

  nativeBuildInputs = [
    rustc
    cargo
  ];

  dontConfigure = true;

  buildPhase = ''
    runHook preBuild
    export HOME="$TMPDIR"
    export CARGO_HOME="$TMPDIR/cargo"
    mkdir -p "$CARGO_HOME"

    cat > "$CARGO_HOME/config.toml" <<EOF
    [source.crates-io]
    replace-with = "vendored-sources"

    [source.vendored-sources]
    directory = "${vendorSrc}"

    [net]
    offline = true
    EOF

    (
      cd client
      cargo build --release --offline --frozen --target-dir "$TMPDIR/target"
    )
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    install -Dm755 "$TMPDIR/target/release/pitsctl" "$out/bin/pitsctl"
    runHook postInstall
  '';

  meta = with lib; {
    description = "PITS Backstage v1.56 Resource-Centric Control-Plane Portal & CLI (`pitsctl`)";
    platforms = [ "x86_64-linux" ];
  };
}
