{pkgs ? import <nixpkgs> {}, ...}:
with pkgs;
  mkShell {
    buildInputs = [
      sqlx-cli
      openssl
      rustup
    ];
  }
