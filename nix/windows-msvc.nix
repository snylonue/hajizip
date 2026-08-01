# Windows GUI package for hajizip (MSVC cross build, fully declarative).
#
# Cross-compiles hajizip-gui for x86_64-pc-windows-msvc from Linux, without
# Visual Studio, using the community-proven combo (see
# local-doc/research-windows-msvc-nix-fod.md):
#   - rust-overlay toolchain with the windows-msvc rust-std (targets
#     override); rust-std itself comes from rust-overlay's own FODs
#   - cargo-xwin (clang-cl + lld-link + llvm-lib + MS CRT/SDK plumbing,
#     incl. cc-rs/cmake/bindgen env) with its offline cache pre-populated
#     by a fixed-output derivation (FOD)
#   - nixpkgs rustPlatform.cargoSetupHook for vendored cargo deps
#
# Result: a single hajizip-gui.exe with the WebView2 loader statically
# linked and no DLLs to ship (CRT comes from the system UCRT on Win10+).

{ lib
, stdenv
, stdenvNoCC
, rustPlatform
, cargo-xwin
, rust-toolchain
, llvmPackages_21
, cacert
}:

let
  # The only fixed-output derivation: pre-download the Microsoft CRT/Windows
  # SDK into cargo-xwin's cache layout. Network happens here (FODs are exempt
  # from the sandbox); the result is pinned by outputHash, so the actual
  # hajizip build is fully offline and reproducible.
  #
  # Reproducibility: XWIN_SDK_VERSION / XWIN_CRT_VERSION pin the exact SDK/CRT
  # versions (otherwise xwin picks "latest" from the mutable VS 17 channel
  # manifest). The pinned outputHash is the final content lock: any upstream
  # drift fails the build instead of silently changing the SDK. The versions
  # below come from the DONE file of the first unpinned run (2026-08).
  windows-sdk = stdenvNoCC.mkDerivation {
    pname = "windows-sdk-xwin-cache";
    version = "17";

    nativeBuildInputs = [ cargo-xwin cacert ];

    # Nix's build sandbox has no CA certificates; point rustls at the bundle
    # from nixpkgs or the HTTPS download fails with "No CA certificates".
    SSL_CERT_FILE = "${cacert}/etc/ssl/certs/ca-bundle.crt";

    buildCommand = ''
      XWIN_CACHE_DIR=$out XWIN_ARCH=x86_64 XWIN_ACCEPT_LICENSE=1 \
        XWIN_SDK_VERSION=10.0.26100 XWIN_CRT_VERSION=14.44.17.14 \
        cargo-xwin xwin cache xwin
      # Trim the splat: the Rust build only needs the CRT headers/libs, the
      # SDK import libs, and the UCRT C headers (sdk/include/ucrt — string.h
      # etc., required by zstd-sys; crt/include only holds C++ STL headers).
      # The Windows SDK C++ headers (um/shared/winrt/cppwinrt, ~350 MB) are
      # never read by rustc/clang-cl. Dropping them here shrinks both the
      # store path and the per-build copy in the main derivation.
      # cargo-xwin's DONE check only reads the first line (architectures),
      # so it never notices the removed dirs.
      find "$out/xwin/sdk/include" -mindepth 1 -maxdepth 1 ! -name ucrt -exec rm -rf {} +
    '';

    outputHashMode = "recursive";
    outputHashAlgo = "sha256";
    # Pinned 2026-08: xwin manifest v17, x86_64 desktop, SDK 10.0.26100,
    # CRT 14.44.17.14, sdk/include trimmed to ucrt. Update by deleting the
    # hash, building once (network), and pasting the reported hash.
    outputHash = "sha256-uZS5PJ1712mFC/oRZcp0KcQD3fusVUG47Ig+b1nvMtI=";
  };
in
stdenv.mkDerivation {
  pname = "hajizip-gui";
  version = "0.1.0";

  src = lib.cleanSourceWith {
    src = ./..;
    filter = path: type:
      let
        base = baseNameOf path;
        excluded = [ "testdata" "local-doc" "dist" ".github" "scripts" ];
      in
      !(builtins.elem base excluded);
  };

  doCheck = false; # tests need testdata fixtures; CI runs them on Linux

  # rust-toolchain first so its cargo/rustc (with the MSVC std) shadows
  # rustPlatform's native rustc.
  nativeBuildInputs = [
    rust-toolchain
    rustPlatform.cargoSetupHook # vendored deps: CARGO_HOME + offline config
    cargo-xwin # drives clang-cl/lld-link/llvm-lib and the MS CRT/SDK
    llvmPackages_21.clang-unwrapped # clang-cl (C deps, e.g. zstd-sys)
    llvmPackages_21.lld # lld-link (linker)
    llvmPackages_21.libllvm # llvm-lib (archiver)
  ];

  # Vendored cargo dependencies from Cargo.lock (FOD; no crates.io access in
  # the sandbox).
  cargoDeps = rustPlatform.importCargoLock {
    lockFile = ./../Cargo.lock;
  };

  XWIN_ARCH = "x86_64";
  CARGO_BUILD_TARGET = "x86_64-pc-windows-msvc";

  # Reference to the pinned CRT/SDK FOD: makes it a build dependency and
  # exposes it to the builder as $windowsSdk.
  windowsSdk = windows-sdk;

  # cargo-xwin writes a CMake toolchain file into its cache dir and the store
  # path is read-only, so copy the pinned FOD into the build sandbox. Plain
  # copy (no hardlinks: the sandbox /nix/store may be on a different device
  # than $TMPDIR, and store files are read-only anyway).
  preBuild = ''
    export XWIN_CACHE_DIR="$TMPDIR/xwin-cache"
    mkdir -p "$XWIN_CACHE_DIR"
    cp -r "$windowsSdk/." "$XWIN_CACHE_DIR/"
  '';

  buildPhase = ''
    runHook preBuild
    cargo xwin build --release --target x86_64-pc-windows-msvc -p hajizip-gui
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    install -Dm755 target/x86_64-pc-windows-msvc/release/hajizip-gui.exe \
      $out/bin/hajizip-gui.exe
    runHook postInstall
  '';

  # Don't let Nix strip/fixup the PE binary (Windows PDB/debug sections).
  dontFixup = true;
  dontStrip = true;

  meta = {
    description = "Memory-safe 7-Zip alternative (Windows MSVC build)";
    homepage = "https://github.com/snylonue/hajizip";
    license = lib.licenses.mit;
    # The derivation runs on Linux and produces a Windows .exe, so the build
    # (host) platforms are Linux.
    platforms = lib.platforms.linux;
    mainProgram = "hajizip-gui";
  };
}
