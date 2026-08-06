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

        rustToolchain = pkgs.rust-bin.stable."1.93.0".default.override {
          targets = [ "wasm32-unknown-unknown" ];
        };

        # The Aurora controller is a separate contract that pins its own
        # toolchain (see rust-toolchain in that repository), and its near-sdk
        # refuses to be compiled by anything newer. Build it in its own shell
        # rather than dragging this repository back to an old compiler.
        controllerToolchain = pkgs.rust-bin.stable."1.86.0".default.override {
          targets = [ "wasm32-unknown-unknown" ];
        };
      in
      {
        devShells = {
          default = pkgs.mkShell {
            buildInputs = with pkgs; [
              rustToolchain
              cargo-watch
              cargo-audit
              rust-analyzer
              binaryen
              llvmPackages.clang
            ];
          };

          # Used by `just build-controller`.
          controller = pkgs.mkShell {
            buildInputs = with pkgs; [
              controllerToolchain
              cargo-make
              binaryen
              llvmPackages.clang
            ];
          };
        };
      }
    );
}
