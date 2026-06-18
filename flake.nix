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
        checksarif = pkgs.callPackage ./nix/sarif-multitool/checksarif.nix { };
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
        packages.examples.nginx = pkgs.pkgsCross.gnu64.enableDebugging (
          pkgs.pkgsCross.gnu64.nginx.override {
            withDebug = true;
            withPerl = false;
            perl = null;
          }
        );

        # CI/test logic lives under `checks` so the GitHub workflows can run
        #   nix build .#checks.<system>.cheap
        #   nix build .#checks.<system>.nightly
        # instead of duplicating cargo invocations in YAML. This does not rely on
        # `nix flake check`; each check is an ordinary buildable derivation.
        checks =
          let
            # `cargo fmt --check` only parses sources: no dependency build and no
            # network. So we run it as a tiny standalone derivation (which also lets
            # us keep the project's exact `--all` flag) rather than a naersk mode,
            # whose fmt support neither adds rustfmt to PATH nor passes `--all`.
            fmt = pkgs.runCommandLocal "ctadl-check-fmt" {
              nativeBuildInputs = [ pkgs.cargo pkgs.rustfmt ];
            } ''
              export HOME="$TMPDIR"
              cd ${self}
              cargo fmt --all -- --check
              touch "$out"
            '';

            # Clippy and the unit tests need the workspace + deps compiled, so we
            # drive them through naersk, which vendors crates for the offline Nix
            # sandbox. `release = false` keeps these "cheap" checks fast. naersk's
            # clippy mode already defaults to `cargo clippy -- -D warnings`.
            clippy = naersk-lib.buildPackage {
              src = ./.;
              mode = "clippy";
              release = false;
            };

            unit-tests = naersk-lib.buildPackage {
              src = ./.;
              mode = "test";
              release = false;
              cargoTestOptions = opts: opts ++ [ "--workspace" ];
            };

            # Aggregate: interpolating a derivation forces it to build, so building
            # `cheap` builds fmt + clippy + unit-tests. This is the per-PR/per-push
            # suite. runCommandLocal avoids shipping this trivial glue to a builder.
            cheap = pkgs.runCommandLocal "ctadl-checks-cheap" { } ''
              {
                echo "fmt:        ${fmt}"
                echo "clippy:     ${clippy}"
                echo "unit-tests: ${unit-tests}"
              } > "$out"
            '';

            # Nightly runs everything `cheap` does and is where the expensive
            # regression / evaluation suite will go. Placeholder for now.
            nightly = pkgs.runCommandLocal "ctadl-checks-nightly" { } ''
              {
                echo "cheap: ${cheap}"
                # TODO: add the expensive regression / evaluation suite here.
                echo "nightly: expensive suite not implemented yet (placeholder)"
              } > "$out"
            '';
          in
          {
            inherit
              fmt
              clippy
              unit-tests
              cheap
              nightly
              ;
          };

        formatter = pkgs.nixfmt;
        devShells.default =
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
              pkgsCross.gnu64.stdenv.cc
              pkgsCross.gnu64.binutils
            ];
            RUST_SRC_PATH = rustPlatform.rustLibSrc;
          };
      }
    );
}
