{
  description = "Mnemos IDE — native NixOS package and Rust development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      };
      rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      rustPlatform = pkgs.makeRustPlatform {
        cargo = rustToolchain;
        rustc = rustToolchain;
      };
      mnemos = pkgs.callPackage ./nix/package.nix {
        inherit rustPlatform;
        revision = self.rev or "";
      };
    in
    {
      packages.${system} = {
        default = mnemos;
        inherit mnemos;
      };
      apps.${system}.default = {
        type = "app";
        program = "${mnemos}/bin/mnemos";
        meta.description = "Mnemos IDE terminal";
      };
      devShells.${system}.default = pkgs.mkShell {
        inputsFrom = [ mnemos ];
        packages = with pkgs; [
          rustToolchain
          git
          git-lfs
          jq
          python3
          cargo-deny
        ];
        # Cargo runs need the same dlopen libraries as the installed wrapper.
        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath mnemos.runtimeLibraries;
      };
      checks.${system}.package = mnemos;
      formatter.${system} = pkgs.nixfmt-tree;
    };
}
