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
          # Do not set BWRAP_PATH — fall back to PATH lookup so the NixOS
          # setuid wrapper at /run/wrappers/bin/bwrap is used when present.
          # The store bwrap is unprivileged and forces a userns, which makes
          # root-owned files appear as nobody and breaks ssh config checks.
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
