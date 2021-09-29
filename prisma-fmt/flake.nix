{
  description = "WASM prisma-fmt";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
    wasm-bindgen = {
      url =
        "https://github.com/rustwasm/wasm-bindgen/archive/refs/tags/0.2.78.tar.gz";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, wasm-bindgen }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        bash = pkgs.bash;
        rust = pkgs.rust-bin.stable.latest.default;
        coreutils = pkgs.coreutils;
        wasm_bindgen = pkgs.wasm-bindgen-cli;
        glibc = pkgs.glibc;
      in {
        defaultPackage = pkgs.rustPlatform.buildRustPackage rec {
          buildInputs = [ actualHello which ];
          name = "prisma-fmt-wasm";
          src = ./.;
          cargoSha256 = pkgs.lib.fakeHash;
          target = "wasm32-unknown-unknown";
          cargoBuildFlags = [ "-p" "prisma-fmt" ];
          # cargoLock = {
          #   outputHashes = {
          #     # "barrel-0.6.6-alpha.0" = pkgs.lib.fakeHash;
          #     # "cuid-0.1.0" = pkgs.lib.fakeHash;
          #     # "graphql-parser-0.3.0" = pkgs.lib.fakeHash;
          #     # "mysql_async-0.27.0" = pkgs.lib.fakeHash;
          #     # "postgres-native-tls-0.5.0" = pkgs.lib.fakeHash;
          #     # "quaint-0.2.0-alpha.13" = pkgs.lib.fakeHash;
          #     # "tokio-native-tls-0.3.0" = pkgs.lib.fakeHash;
          #   };
          # };
        };
        # derivation {
        # name = "prisma-fmt-wasm";
        # builder = "${bash}/bin/bash";
        # args = [ ./builder.sh ];
        # system = system;
        # src = ../.;

        #   inherit rust coreutils wasm_bindgen;
        # };

        devShell = pkgs.mkShell { buildInputs = [ rust ]; };
      });
}
