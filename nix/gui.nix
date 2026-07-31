# Linux GUI package for hajizip (Dioxus desktop, WebKitGTK stack).
#
# Standard nixpkgs recipe for wry/tao apps: rustPlatform + wrapGAppsHook4 +
# webkitgtk_4_1/gtk3 libraries. The `WEBKIT_DISABLE_DMABUF_RENDERER` wrapper
# arg is the well-known fix for blank/white WebKitGTK windows on some GPUs.

{ lib
, stdenv
, rustPlatform
, pkg-config
, wrapGAppsHook4
, webkitgtk_4_1
, gtk3
, libsoup_3
, glib
, cairo
, pango
, gdk-pixbuf
, atk
, openssl
, librsvg
, glib-networking
, xdotool
}:

rustPlatform.buildRustPackage {
  pname = "hajizip-gui";
  version = "0.1.0";

  src = lib.cleanSourceWith {
    src = ./..;
    filter = path: type:
      let
        base = baseNameOf path;
        # Skip fixture/testdata blobs (many.tar is 10 MB) and local docs.
        excluded = [ "testdata" "local-doc" "dist" ".github" "scripts" ];
      in
      !(builtins.elem base excluded);
  };

  cargoLock.lockFile = ./../Cargo.lock;

  # Tests need the testdata fixtures (filtered from src); CI runs them.
  doCheck = false;

  nativeBuildInputs = [
    pkg-config
    wrapGAppsHook4
  ];

  buildInputs = [
    webkitgtk_4_1
    gtk3
    libsoup_3
    glib
    cairo
    pango
    gdk-pixbuf
    atk
    openssl
    librsvg
    glib-networking # TLS inside the WebView
    xdotool # tao → libxdo-sys links `-lxdo` (see local-doc/progress.md)
  ];

  # Blank-window workaround (see nixpkgs tauri apps).
  preFixup = ''
    gappsWrapperArgs+=(--set-default WEBKIT_DISABLE_DMABUF_RENDERER 1)
  '';

  meta = {
    description = "Memory-safe 7-Zip alternative (Dioxus desktop GUI)";
    homepage = "https://github.com/snylonue/hajizip";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "hajizip-gui";
  };
}
