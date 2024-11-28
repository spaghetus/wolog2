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
      pkg-config
      rustup
    ];
    DATABASE_URL = "sqlite:dev.db";
  }
