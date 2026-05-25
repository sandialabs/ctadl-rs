{
  inputs = {
    naersk.url = "github:nix-community/naersk/master";
    nixpkgs.url = "github:NixOS/nixpkgs";
    utils.url = "github:numtide/flake-utils";
    ctadl-souffle.url = "github:sandialabs/ctadl";
  };

  outputs =
    {
      self,
      nixpkgs,
      utils,
      naersk,
      ctadl-souffle,
    }:
    utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        naersk-lib = pkgs.callPackage naersk { };
      in
      {
        packages.default = naersk-lib.buildPackage ./.;
        formatter = pkgs.nixfmt;
        devShell =
          with pkgs;
          mkShell {
            buildInputs = [
              pre-commit
              cargo
              cargo-typify
              rustc
              rustfmt
              rustPackages.clippy
              rust-analyzer
              cargo-expand
              gnuplot
            ];
            RUST_SRC_PATH = rustPlatform.rustLibSrc;
          };
      }
    );
}
