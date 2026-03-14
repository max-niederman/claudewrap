{
  description = "claudewrap - Sandbox Claude Code with bubblewrap";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, crane, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        craneLib = crane.mkLib pkgs;

        claudewrap = craneLib.buildPackage {
          src = craneLib.cleanCargoSource ./.;
          strictDeps = true;
          buildInputs = [ ];
          nativeBuildInputs = [ ];
          BWRAP_PATH = "${pkgs.bubblewrap}/bin/bwrap";
        };
      in
      {
        packages.default = claudewrap;

        devShells.default = craneLib.devShell {
          packages = with pkgs; [
            rust-analyzer
            cargo-watch
            bubblewrap
          ];
        };
      }
    ) // {
      overlays.default = final: prev: {
        claudewrap = self.packages.${prev.system}.default;
      };
    };
}
