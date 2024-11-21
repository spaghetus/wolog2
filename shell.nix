{pkgs ? import <nixpkgs> {}, ...}:
with pkgs;
  mkShell {
    buildInputs = [
      sqlx-cli
      openssl
      pkg-config
      rustup
    ];
    DATABASE_URL = "sqlite:dev.db";
  }
