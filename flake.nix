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
        ctadl-souffle-wrapper = pkgs.writeShellScriptBin "ctadl-souffle" ''
          exec ${ctadl-souffle.packages.${system}.ctadlPackages.ctadl-full}/bin/ctadl "$@"
        '';
        checksarif =
          # Use x86_64 version on aarch because aarch version doesn't work
          (import nixpkgs {
            system =
              if system == "aarch64-darwin"
              then "x86_64-darwin"
              else system;
          })
          .callPackage
          ./nix/sarif-multitool/checksarif.nix {
          };
      in
      {
        packages.default = naersk-lib.buildPackage ./.;
        # Legacy ctadl: nix run .#ctadl-souffle
        packages.ctadl-souffle = ctadl-souffle.packages.${system}.ctadl-full;
        # Run a wrapper around ctadl-ascent that lets Ghidra process pcode
        packages.legacy-pcode-cli =
          let
            ctadl = pkgs.writeShellScriptBin "ctadl-legacy-pcode-cli" ''
              ${self.packages.${system}.default}/bin/ctadl legacy-pcode-cli "$@"
            '';
          in
          ctadl;
        packages.checksarif = checksarif;

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
              sarif-tools
              cargo-flamegraph
              (python3.withPackages (ps: [ ps.pyarrow ]))
              ctadl-souffle-wrapper
              parquet-tools
              graphviz
              checksarif
              ghidra-bin
            ];
            RUST_SRC_PATH = rustPlatform.rustLibSrc;
          };
      }
    );
}
