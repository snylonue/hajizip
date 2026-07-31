# Windows GUI package for hajizip (MinGW cross build).
#
# Cross-compiles hajizip-gui for x86_64-pc-windows-gnu via
# `pkgsCross.mingwW64.rustPlatform`. Two quirks handled here:
#   1. Rust std links `-l:libpthread.a`; nixpkgs' mingw-w64 pthreads must be
#      on the library search path (see local-doc/research-packaging-windows.md).
#   2. GNU builds link against the WebView2 import lib, so the runtime loader
#      DLL must be shipped next to the exe. It is fetched from the
#      Microsoft.Web.WebView2 NuGet package and installed into $out/bin.
#
# Note: the MSVC/cargo-xwin route produces a single exe with the loader
# statically linked; it cannot be expressed declaratively in Nix (xwin
# downloads the MS CRT/SDK at build time), so this MinGW package is the
# declarative Windows artifact. Use scripts/package-windows.sh msvc for the
# MSVC build.

{ lib
, stdenv
, rustPlatform
, fetchurl
, unzip
, windows
}:

rustPlatform.buildRustPackage {
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

  cargoLock.lockFile = ./../Cargo.lock;

  # Tests need the testdata fixtures (filtered from src); CI runs them.
  doCheck = false;

  # MinGW pthreads for Rust's `-l:libpthread.a` (target-side, so buildInputs).
  buildInputs = [ windows.pthreads ];

  nativeBuildInputs = [ unzip ];

  # WebView2Loader.dll from the Microsoft.Web.WebView2 NuGet package.
  webview2Loader = fetchurl {
    url = "https://www.nuget.org/api/v2/package/Microsoft.Web.WebView2/1.0.2903.40";
    hash = "sha256-7xKAFt0eUcWReMgn7VuKozIsV6+oZ12TD4EJUFVCrXQ=";
  };

  # GNU/Windows build: libpthread.a comes from the cross package set above.
  RUSTFLAGS = "-L native=${windows.pthreads}/lib";

  installPhase = ''
    runHook preInstall
    install -Dm755 target/*/release/hajizip-gui.exe $out/bin/hajizip-gui.exe
    unzip -o "$webview2Loader" 'build/native/x64/WebView2Loader.dll' -d "$TMPDIR/wv2"
    install -Dm644 "$TMPDIR/wv2/build/native/x64/WebView2Loader.dll" \
      $out/bin/WebView2Loader.dll
    runHook postInstall
  '';

  meta = {
    description = "Memory-safe 7-Zip alternative (Windows build)";
    homepage = "https://github.com/snylonue/hajizip";
    license = lib.licenses.mit;
    platforms = [ "x86_64-windows" ];
    mainProgram = "hajizip-gui";
  };
}
