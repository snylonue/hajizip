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
        ];
        perSystem =
          {
            pkgs,
            ...
          }:
          {
            devShells.default = pkgs.mkShellNoCC {
              packages = [
                inputs.llm-agents.packages.${pkgs.stdenv.hostPlatform.system}.pi
                pkgs.uv
              ];
            };
          };
      }
    );
}
