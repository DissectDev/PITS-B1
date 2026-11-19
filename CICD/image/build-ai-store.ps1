<#
.SYNOPSIS
  Builds the Linux `ai-store` OCI image (`docker.io/pits/ai-store:v0.1.0`) via Nix on `pits-core`
  (`100.95.95.88`), publishes it to `talos-image-cache` (`10.89.20.14:3172`) & Zot (`10.89.20.21:30500`),
  and optionally cross-compiles `ai-storage/bin/ai-store.exe` for Windows (`x86_64-pc-windows-gnu`).
#>
[CmdletBinding()]
param(
    [string]$SshTarget = "root@100.95.95.88",
    [string]$ImageTag = "v0.1.0",
    [switch]$BuildWindows
)

$ErrorActionPreference = "Stop"
$CicdRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$RepoRoot = (Resolve-Path (Join-Path $CicdRoot "..")).Path
$AiStorageDir = Join-Path $RepoRoot "ai-storage"
$Utf8NoBom = [System.Text.UTF8Encoding]::new($false)

Write-Host "[1/4] Packing ai-storage sources & Nix OCI expression..." -ForegroundColor Cyan
$StageDir = Join-Path $env:TEMP "pits-ai-store-stage"
$TarPath  = Join-Path $env:TEMP "pits-ai-store-stage.tar.gz"
if (Test-Path $StageDir) { Remove-Item -Recurse -Force $StageDir }
if (Test-Path $TarPath)  { Remove-Item -Force $TarPath }
New-Item -ItemType Directory -Force -Path $StageDir, (Join-Path $StageDir "src-tree") | Out-Null

$nixTxt = [System.IO.File]::ReadAllText((Join-Path $CicdRoot "image\ai-store-image.nix")).Replace("`r`n", "`n")
[System.IO.File]::WriteAllBytes((Join-Path $StageDir "ai-store-image.nix"), $Utf8NoBom.GetBytes($nixTxt))
$impTxt = [System.IO.File]::ReadAllText((Join-Path $RepoRoot "test-pool\image\import-to-talos-cache.py")).Replace("`r`n", "`n")
[System.IO.File]::WriteAllBytes((Join-Path $StageDir "import-to-talos-cache.py"), $Utf8NoBom.GetBytes($impTxt))

Copy-Item -Force (Join-Path $AiStorageDir "Cargo.toml") (Join-Path $StageDir "src-tree\Cargo.toml")
if (Test-Path (Join-Path $AiStorageDir "Cargo.lock")) {
    Copy-Item -Force (Join-Path $AiStorageDir "Cargo.lock") (Join-Path $StageDir "src-tree\Cargo.lock")
}
Copy-Item -Recurse -Force (Join-Path $AiStorageDir "src") (Join-Path $StageDir "src-tree\src")
Copy-Item -Recurse -Force (Join-Path $AiStorageDir "tests") (Join-Path $StageDir "src-tree\tests")

& tar.exe -czf $TarPath -C $StageDir .
if ($LASTEXITCODE -ne 0) { throw "tar.exe pack failed" }

try {
    & scp -B -o ConnectTimeout=15 $TarPath "${SshTarget}:/tmp/pits-ai-store-stage.tar.gz"
    if ($LASTEXITCODE -ne 0) { throw "SCP upload failed" }
} finally {
    Remove-Item -Recurse -Force $StageDir -ErrorAction SilentlyContinue
    Remove-Item -Force $TarPath -ErrorAction SilentlyContinue
}

$winFlag = if ($BuildWindows) { "1" } else { "0" }
$remoteScript = @"
set -euo pipefail
WORK="/var/lib/pits/ai-store-build"
CACHE_ROOT="/var/lib/pits/tinkerbell-20261108/image-cache-v1.14.5"
mkdir -p "`$WORK/ctx" "`$WORK/cargo-home"
rm -rf "`$WORK/ctx/"*
tar -xzf /tmp/pits-ai-store-stage.tar.gz -C "`$WORK/ctx"
rm -f /tmp/pits-ai-store-stage.tar.gz

cat > "`$WORK/cargo-home/config.toml" <<'EOF'
[source.crates-io]
replace-with = 'rsproxy-sparse'

[source.rsproxy-sparse]
registry = "sparse+https://rsproxy.cn/index/"
EOF

export CARGO_HOME="`$WORK/cargo-home"
export CARGO_TARGET_DIR="`$WORK/target"
cd "`$WORK/ctx/src-tree"

nix --extra-experimental-features 'nix-command flakes' shell \
  'path:/home/libraskywalker/PITS-B1/Base#nixosConfigurations.core.pkgs.cargo' \
  'path:/home/libraskywalker/PITS-B1/Base#nixosConfigurations.core.pkgs.rustc' \
  'path:/home/libraskywalker/PITS-B1/Base#nixosConfigurations.core.pkgs.stdenv.cc' \
  'path:/home/libraskywalker/PITS-B1/Base#nixosConfigurations.core.pkgs.sqlite' \
  'path:/home/libraskywalker/PITS-B1/Base#nixosConfigurations.core.pkgs.skopeo' \
  'path:/home/libraskywalker/PITS-B1/Base#nixosConfigurations.core.pkgs.python3' \
  -c bash -c '
    SQLITE_LIB_DIR=`$(dirname "`$(find /nix/store -maxdepth 3 -name "libsqlite3.so.0" 2>/dev/null | head -n1)")
    export LD_LIBRARY_PATH="`${SQLITE_LIB_DIR}:`${LD_LIBRARY_PATH:-}"
    cargo test --release
    cargo build --release --bin ai-store
  '

BIN_PATH="`$CARGO_TARGET_DIR/release/ai-store"
echo "[2/4] Building Nix OCI image docker.io/pits/ai-store:${ImageTag}..."
IMG_TAR=`$(nix --extra-experimental-features 'nix-command flakes' build --impure --no-warn-dirty --no-link --print-out-paths \
  --option substituters https://mirrors.ustc.edu.cn/nix-channels/store \
  --expr "let p = (builtins.getFlake \"path:/home/libraskywalker/PITS-B1/Base\").nixosConfigurations.core.pkgs; in import `$WORK/ctx/ai-store-image.nix { pkgs = p; aiStoreBin = builtins.path { path = `$BIN_PATH; name = \"ai-store\"; }; imageTag = \"${ImageTag}\"; }")

echo "[3/4] Publishing docker.io/pits/ai-store:${ImageTag} to talos-image-cache..."
nix --extra-experimental-features 'nix-command flakes' shell \
  'path:/home/libraskywalker/PITS-B1/Base#nixosConfigurations.core.pkgs.skopeo' \
  'path:/home/libraskywalker/PITS-B1/Base#nixosConfigurations.core.pkgs.python3' \
  -c bash -c "
    rm -rf `$WORK/oci && mkdir -p `$WORK/oci
    skopeo copy --insecure-policy docker-archive:`$IMG_TAR oci:`$WORK/oci:${ImageTag}
    python3 `$WORK/ctx/import-to-talos-cache.py `$WORK/oci `$CACHE_ROOT docker.io/pits/ai-store:${ImageTag}
    rm -rf `$WORK/oci
  "

if [ "${winFlag}" = "1" ]; then
  echo "[4/4] Cross-compiling ai-store.exe (x86_64-pc-windows-gnu)..."
  cat > "`$WORK/rust-win-toolchain.nix" <<'NIXEOF'
let
  p = (builtins.getFlake "path:/home/libraskywalker/PITS-B1/Base").nixosConfigurations.core.pkgs;
  rustDist = p.fetchurl {
    urls = [ "https://mirrors.ustc.edu.cn/rust-static/dist/rust-1.95.0-x86_64-unknown-linux-gnu.tar.xz" ];
    sha256 = "2e0338f18ecbaa4a0f631b9e80e8b8e26bb6fe77dd5454fba8a70cf96c1e84a1";
  };
  rustStdWin = p.fetchurl {
    urls = [ "https://mirrors.ustc.edu.cn/rust-static/dist/rust-std-1.95.0-x86_64-pc-windows-gnu.tar.xz" ];
    sha256 = "f57e045016a04130125fb43295d95f9ad2bebc296150eadb031dbf5167ad12bd";
  };
in p.stdenv.mkDerivation {
  pname = "pits-rust-win-toolchain";
  version = "1.95.0";
  dontUnpack = true;
  nativeBuildInputs = [ p.autoPatchelfHook p.xz p.gnutar ];
  buildInputs = [ p.stdenv.cc.cc.lib p.zlib p.openssl p.curl ];
  autoPatchelfIgnoreMissingDeps = [ "libgcc_s.so.1" ];
  installPhase = ''
    mkdir -p "`$out" work-rust work-std
    tar -xJf "`${rustDist}" -C work-rust --strip-components=1
    tar -xJf "`${rustStdWin}" -C work-std --strip-components=1
    bash work-rust/install.sh --prefix="`$out" --disable-ldconfig \
      --components=rustc,cargo,rust-std-x86_64-unknown-linux-gnu
    bash work-std/install.sh --prefix="`$out" --disable-ldconfig \
      --components=rust-std-x86_64-pc-windows-gnu
  '';
}
NIXEOF
  RUST_WIN_TC=`$(nix --extra-experimental-features 'nix-command flakes' build --impure --no-warn-dirty --no-link --print-out-paths \
    --option substituters https://mirrors.ustc.edu.cn/nix-channels/store \
    -f "`$WORK/rust-win-toolchain.nix")
  export WORK
  export RUST_WIN_TC
  export PTHREADS_ROOT=`$(nix --extra-experimental-features 'nix-command flakes' eval --raw 'path:/home/libraskywalker/PITS-B1/Base#nixosConfigurations.core.pkgs.pkgsCross.mingwW64.windows.pthreads.outPath')
  export PTHREADS_LIB="`$PTHREADS_ROOT/lib"
  export PTHREADS_INC="`$PTHREADS_ROOT/include"
  nix --extra-experimental-features 'nix-command flakes' shell \
    --option substituters https://mirrors.ustc.edu.cn/nix-channels/store \
    'path:/home/libraskywalker/PITS-B1/Base#nixosConfigurations.core.pkgs.stdenv.cc' \
    'path:/home/libraskywalker/PITS-B1/Base#nixosConfigurations.core.pkgs.pkgsCross.mingwW64.stdenv.cc' \
    -c bash -ec '
      export PATH="`$RUST_WIN_TC/bin:`$PATH"
      export RUSTFLAGS="-L native=`$PTHREADS_LIB"
      export CFLAGS_x86_64_pc_windows_gnu="-I`$PTHREADS_INC"
      export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
      export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_AR=x86_64-w64-mingw32-ar
      export CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc
      export AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar
      cargo build --release --target x86_64-pc-windows-gnu --bin ai-store
    '
fi
echo "DONE!"
"@

$tmpSh = Join-Path $env:TEMP "pits-build-ai-store.sh"
[System.IO.File]::WriteAllBytes($tmpSh, $Utf8NoBom.GetBytes($remoteScript.Replace("`r`n", "`n") + "`n"))
try {
    & scp -B -o ConnectTimeout=15 $tmpSh "${SshTarget}:/tmp/pits-build-ai-store.sh"
    & ssh -T -o BatchMode=yes -o ConnectTimeout=15 $SshTarget "bash '/tmp/pits-build-ai-store.sh'; rc=`$?; rm -f '/tmp/pits-build-ai-store.sh'; exit `$rc"
    if ($LASTEXITCODE -ne 0) { throw "Remote build-ai-store failed (exit $LASTEXITCODE)" }
} finally {
    Remove-Item -Force $tmpSh -ErrorAction SilentlyContinue
}
if ($BuildWindows) {
    $binDir = Join-Path $AiStorageDir "bin"
    New-Item -ItemType Directory -Force -Path $binDir | Out-Null
    & scp -B -o ConnectTimeout=15 "${SshTarget}:/var/lib/pits/ai-store-build/target/x86_64-pc-windows-gnu/release/ai-store.exe" (Join-Path $binDir "ai-store.exe")
    if ($LASTEXITCODE -ne 0) { throw "Failed to download ai-store.exe" }
    Write-Host "Downloaded Windows binary -> $(Join-Path $binDir 'ai-store.exe')" -ForegroundColor Green
}
