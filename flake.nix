# This is a simple deterministic rust development environment
# This exposes Cargo, rustfmt, rust-analyzer and clippy
# This does not allow you to build binaries using nix
{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };
  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      flake-utils,
      ...
    }:

    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        rustToolchain = pkgs.rust-bin.stable."1.86.0".default.override {
          targets = [ "wasm32-unknown-unknown" ];
        };
      in
      {
        devShell = pkgs.mkShell {

          buildInputs = with pkgs; [
            rustToolchain
            cargo-watch
            cargo-audit
            rust-analyzer
            binaryen
            llvmPackages.clang
          ];
        };
      }
    );
}
