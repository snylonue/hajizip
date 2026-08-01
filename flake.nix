{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    llm-agents = {
      url = "github:numtide/llm-agents.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Rust toolchain for packaging/CI only (dev shell keeps the system
    # toolchain). Used by the declarative MSVC Windows package.
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{
      flake-parts,
      nixpkgs,
      ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } (
      {
        ...
      }:
      {
        systems = [
          "x86_64-linux"
          "aarch64-linux"
          "x86_64-darwin"
          "aarch64-darwin"
        ];
        perSystem =
          {
            pkgs,
            lib,
            self',
            system,
            ...
          }:
          {
            # Declarative packages: Linux GUI (native) and Windows GUI
            # (MinGW cross build, fallback) + Windows GUI via MSVC
            # (rust-overlay toolchain + cargo-xwin, recommended). Defined only
            # on Linux hosts (the cross package set and WebKit stack are
            # Linux-only here); the Windows packages are also what the CI
            # release job builds.
            packages = lib.optionalAttrs pkgs.stdenv.isLinux {
              gui = pkgs.callPackage ./nix/gui.nix { };
              windows = pkgs.pkgsCross.mingwW64.callPackage ./nix/windows.nix { };
              # MSVC route: single exe, no WebView2Loader.dll to ship, CET
              # bit set; CRT/SDK pre-downloaded via a fixed-output derivation
              # (see local-doc/research-windows-msvc-nix-fod.md).
              windows-msvc = let
                pkgs' = import inputs.nixpkgs {
                  inherit system;
                  overlays = [ (import inputs.rust-overlay) ];
                };
                rust-toolchain = (pkgs'.rust-bin.stable.latest.default.override {
                  targets = [ "x86_64-pc-windows-msvc" ];
                });
              in pkgs'.callPackage ./nix/windows-msvc.nix { inherit rust-toolchain; };
              default = self'.packages.gui;
            };

            devShells.default = pkgs.mkShell {
              packages =
                [
                  inputs.llm-agents.packages.${pkgs.stdenv.hostPlatform.system}.pi
                  pkgs.dioxus-cli
                  pkgs.pkg-config
                ]
                ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
                  pkgs.webkitgtk_4_1
                  pkgs.gtk3
                  pkgs.libsoup_3
                  pkgs.glib
                  pkgs.cairo
                  pkgs.pango
                  pkgs.gdk-pixbuf
                  pkgs.atk
                  pkgs.openssl
                  pkgs.xdotool
                  pkgs.libayatana-appindicator
                  pkgs.librsvg
                ];
            };

            # Packaging shell: cross-compile the Windows GUI from Linux.
            # MSVC route (recommended, no WebView2Loader.dll to ship) via
            # cargo-xwin; MinGW route kept as fallback. Rust toolchain itself
            # stays system-provided (rustup targets must be installed
            # separately, see local-doc/research-packaging-windows.md).
            devShells.windows-pack = pkgs.mkShell {
              packages =
                [
                  pkgs.cargo-xwin
                  pkgs.wine64
                  # MSVC route: clang-cl / llvm-lib / lld-link.
                  pkgs.llvmPackages_21.clang-unwrapped
                  pkgs.llvmPackages_21.libllvm
                  pkgs.llvmPackages_21.lld
                ]
                ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
                  # MinGW route: cross linker + pthreads (for -l:libpthread.a).
                  # Cross packages must live in buildInputs, not packages
                  # (they are not available on the native hostPlatform).
                  pkgs.pkgsCross.mingwW64.stdenv.cc
                ];
              buildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [
                pkgs.pkgsCross.mingwW64.windows.pthreads
              ];
              shellHook = ''
                echo "hajizip windows-pack shell"
                echo "  MSVC:  cargo xwin build --release --target x86_64-pc-windows-msvc"
                echo "  MinGW: cargo build --release --target x86_64-pc-windows-gnu (see scripts/package-windows.sh)"
                echo "  Wine:  wine64 target/x86_64-pc-windows-msvc/release/hajizip-gui.exe"
              '';
            };
          };
      }
    );
}
