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
            ...
          }:
          {
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
          };
      }
    );
}
