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
            nixfmt.enable = true;
            statix.enable = true;
            deadnix.enable = true;
            rustfmt.enable = true;
            yamlfmt.enable = true;
            taplo.enable = true;
          };
        };
        deps = with pkgs; [
          cargo
          rust-analyzer
          rustfmt
          pre-commit
          rustPackages.clippy
          openssl
        ];
      in
      {
        packages = {
          api-router = pkgs.callPackage ./nix/pkgs/api-router.nix { };
          default = self.packages.${system}.api-router;
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
