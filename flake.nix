{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    utils.url = "github:numtide/flake-utils";
    treefmt-nix.url = "github:numtide/treefmt-nix";
    treefmt-nix.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    {
      self,
      nixpkgs,
      utils,
      treefmt-nix,
      ...
    }:
    utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        treefmtEval = treefmt-nix.lib.evalModule pkgs {
          projectRootFile = "Cargo.toml";
          programs = {
            nixfmt.enable = true; # nix
            statix.enable = true; # nix static analysis
            deadnix.enable = true; # find dead nix code
            rustfmt.enable = true; # rust
            yamlfmt.enable = true; # yaml
            taplo.enable = true; # toml
          };
        };
        deps = with pkgs; [
          cargo
          # rustup
          rust-analyzer
          rustfmt
          pre-commit
          rustPackages.clippy
          openssl
        ];
      in
      {
        packages = {
          rust-playground = pkgs.callPackage ./nix/pkgs/rust-playground.nix { };
          default = self.packages.${system}.rust-playground;
        };
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [ pkg-config ];
          buildInputs = [ deps ];
          RUST_SRC_PATH = pkgs.rustPlatform.rustLibSrc;
          PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";
        };
        formatter = treefmtEval.config.build.wrapper;
        checks = {
          formatting = treefmtEval.config.build.check self;
        };
      }
    );
}
