#!/usr/bin/env bash
# Build the Windows GUI from Linux and collect a distributable zip.
#
# Two routes (see local-doc/research-packaging-windows.md):
#   MSVC (default, recommended): cargo xwin build --target x86_64-pc-windows-msvc
#     -> single .exe, no extra DLLs (WebView2 loader is statically linked);
#        target machine only needs the WebView2 Runtime (Win10/11 default).
#   MinGW (fallback):            cargo build --target x86_64-pc-windows-gnu
#     -> .exe + WebView2Loader.dll must be shipped side by side.
#
# Requirements: rustup targets installed (x86_64-pc-windows-msvc / -gnu) and,
# for MinGW, the mingw linker + pthreads from the nixpkgs windows-pack shell
# (or equivalent env: CARGO_TARGET_*_LINKER / CC_* / RUSTFLAGS).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT"

ROUTE="${1:-msvc}"
OUT_DIR="$ROOT/dist"
ZIP_NAME="hajizip-windows-x86_64.zip"

case "$ROUTE" in
  msvc)
    TARGET="x86_64-pc-windows-msvc"
    XWIN="${XWIN:-cargo-xwin}"
    "$XWIN" build --release --target "$TARGET" -p hajizip-gui
    EXE="target/$TARGET/release/hajizip-gui.exe"
    ;;
  gnu)
    TARGET="x86_64-pc-windows-gnu"
    cargo build --release --target "$TARGET" -p hajizip-gui
    EXE="target/$TARGET/release/hajizip-gui.exe"
    ;;
  *)
    echo "usage: $0 [msvc|gnu]" >&2
    exit 2
    ;;
esac

[ -f "$EXE" ] || { echo "build produced no exe: $EXE" >&2; exit 1; }

mkdir -p "$OUT_DIR/stage"
rm -rf "$OUT_DIR/stage"/*
cp "$EXE" "$OUT_DIR/stage/"
cp README.md "$OUT_DIR/stage/" 2>/dev/null || true

if [ "$ROUTE" = "gnu" ]; then
  # GNU builds link against the import lib; ship the loader DLL next to the exe
  # (from the Microsoft.Web.WebView2 NuGet package). Without it the app fails
  # with 0xc0000135 "cannot find WebView2Loader.dll".
  if [ -n "${WEBVIEW2_LOADER_DLL:-}" ]; then
    cp "$WEBVIEW2_LOADER_DLL" "$OUT_DIR/stage/WebView2Loader.dll"
  else
    echo "WARNING: GNU route needs WebView2Loader.dll next to the exe." >&2
    echo "  set WEBVIEW2_LOADER_DLL=/path/to/build/native/x64/WebView2Loader.dll" >&2
  fi
fi

cd "$OUT_DIR/stage"
rm -f "../$ZIP_NAME"
zip -q -r "../$ZIP_NAME" .
cd "$ROOT"
echo "packaged: $OUT_DIR/$ZIP_NAME ($(du -h "$OUT_DIR/$ZIP_NAME" | cut -f1))"
echo "  exe: $(du -h "$OUT_DIR/stage/$(basename "$EXE")" | cut -f1)"
