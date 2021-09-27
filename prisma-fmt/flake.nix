{
  description = "WASM prisma-fmt";

  inputs = {
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        bash = pkgs.bash;
        rust = pkgs.rust-bin.stable.latest.default;
      in {
        nixpkgs.overlays = [ rust-overlay.overlay ];
        defaultPackage = derivation {
          name = "prisma-fmt-wasm";
          builder = "${bash}/bin/bash";
          args = [ ./builder.sh ];
          system = system;

          inherit rust;
        };
        devShell = import ./shell.nix { inherit pkgs; };
      });
}
