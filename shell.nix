{pkgs ? import <nixpkgs> {}, ...}:
with pkgs;
  mkShell {
    buildInputs = [
      sqlx-cli
      openssl
      cargo
      rustc
      clippy
      rust-analyzer
      pandoc
    ];
  }
