{
  inputs = {
    nixpkgs.url = "nixpkgs/nixos-24.05";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
  };
  outputs = inputs @ {
    self,
    nixpkgs,
    flake-parts,
    ...
  }:
    flake-parts.lib.mkFlake {inherit inputs;} {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];

      flake = {...}: rec {
        nixosModules.default = import ./module.nix;
        nixosConfigurations.container = nixpkgs.lib.nixosSystem {
          system = "x86_64-linux";
          modules = [
            nixosModules.default
            ({pkgs, ...}: {
              # Only allow this to boot as a container
              boot.isContainer = true;
              networking.hostName = "wolog-hello";

              # Allow nginx through the firewall
              networking.firewall.allowedTCPPorts = [8000];

              services.wolog.package = self.packages.x86_64-linux.wolog;
              services.wolog.enable = true;
              services.wolog.articlesDir = builtins.toString ./articles;
              services.wolog.address = "0.0.0.0";
            })
          ];
        };
      };

      perSystem = {
        system,
        pkgs,
        lib,
        inputs',
        ...
      }: let
        cargo = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        pkg = pkgs.rustPlatform.buildRustPackage rec {
          pname = cargo.package.name;
          version = cargo.package.version;
          src = pkgs.runCommand "src" {} ''
            mkdir $out
            cp -r ${./src} $out/src
            cp -r ${./Cargo.toml} $out/Cargo.toml
            cp -r ${./Cargo.lock} $out/Cargo.lock
          '';
          cargoLock = {
            lockFile = ./Cargo.lock;
          };
        };
      in rec {
        packages = {
          wolog = pkg;
          default = packages.wolog;
        };
      };
    };
}
