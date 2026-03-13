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

        ssh-agent-filter = pkgs.stdenv.mkDerivation {
          pname = "ssh-agent-filter";
          version = "0.6.1-unstable";
          src = pkgs.fetchFromGitHub {
            owner = "tiwe-de";
            repo = "ssh-agent-filter";
            rev = "2f994963899ca8e039dd0aa38884693a9928e116";
            hash = "sha256-PK3ws6jwFu0lxWWMPwDP3jY8otrmGFZYISAaeTXVPh0=";
          };
          nativeBuildInputs = with pkgs; [ pandoc help2man pkg-config ];
          buildInputs = with pkgs; [ boost nettle ];
          dontInstall = true;
          postBuild = ''
            mkdir -p $out/bin
            cp ssh-agent-filter $out/bin/
            cp afssh $out/bin/
          '';
          env.NIX_CFLAGS_COMPILE = "-fpermissive";
          # version.h is generated from git tags; provide a fallback
          # Remove -lboost_system (merged into boost_filesystem in modern boost)
          preBuild = ''
            echo '#define SSH_AGENT_FILTER_VERSION "0.6.1"' > version.h
            sed -i 's/-lboost_system//' Makefile
          '';
        };

        claudewrap = craneLib.buildPackage {
          src = craneLib.cleanCargoSource ./.;
          strictDeps = true;
          buildInputs = [ ];
          nativeBuildInputs = [ ];
          BWRAP_PATH = "${pkgs.bubblewrap}/bin/bwrap";
          SSH_AGENT_FILTER_PATH = "${ssh-agent-filter}/bin/ssh-agent-filter";
          SSH_ADD_PATH = "${pkgs.openssh}/bin/ssh-add";
        };
      in
      {
        packages = {
          default = claudewrap;
          inherit ssh-agent-filter;
        };

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
