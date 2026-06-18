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
        pkgs = import nixpkgs {
          inherit system;
          # The nightly regression suite pulls in unfree components (the Android
          # SDK and ghidra-bin) and needs the Android SDK license accepted
          # non-interactively. These only matter for `checks.<system>.nightly`;
          # the cheap checks and packages build the same as before.
          config = {
            allowUnfree = true;
            android_sdk.accept_license = true;
          };
          # nixpkgs ships a wrong hash for the Darwin platform-tools zip; fix it
          # here. This overlay is a no-op on Linux (the URL never matches), so CI
          # on ubuntu-latest is unaffected.
          overlays = [
            (final: prev: {
              fetchurl =
                args:
                if
                  (args.url or "") == "https://dl.google.com/android/repository/platform-tools_r37.0.0-darwin.zip"
                then
                  prev.fetchurl (
                    (builtins.removeAttrs args [
                      "sha256"
                      "sha1"
                      "sha512"
                      "md5"
                      "hash"
                    ])
                    // {
                      hash = "sha1-jEySbQyhkjdrKgSwMYSEckMZ5nw=";
                    }
                  )
                else
                  prev.fetchurl args;
            })
          ];
        };
        naersk-lib = pkgs.callPackage naersk { };
        ctadl-souffle-wrapper = pkgs.writeShellScriptBin "ctadl-souffle" ''
          exec ${ctadl-souffle.packages.${system}.ctadlPackages.ctadl-full}/bin/ctadl "$@"
        '';
        checksarif = pkgs.callPackage ./nix/sarif-multitool/checksarif.nix { };

        # --- Nightly regression suite dependencies -------------------------
        # These are only forced when building `checks.<system>.{regression,nightly}`.

        # Android SDK, pinned to build-tools 30.0.2 because the DEX tests invoke
        # `dx`, which was removed in build-tools 31+.
        androidSdk = pkgs.androidenv.composeAndroidPackages {
          buildToolsVersions = [ "30.0.2" ];
          platformVersions = [ "30" ];
          includeEmulator = false;
          includeNDK = false;
        };
        jdk = pkgs.temurin-bin-17;

        # `dex-reader` is a workspace-excluded crate (its bins must stay out of
        # the distro), so we build it on its own for the linemap step of the DEX
        # tests rather than expecting it in packages.default.
        dex-reader = naersk-lib.buildPackage {
          src = ./dex-reader;
          release = false;
        };

        # Non-interactive environment that mirrors the tools the regression
        # scripts expect on PATH (ctadl, dex-reader, javac, dx, gcc/addr2line,
        # ghidra, jq, python3).
        testEnv = pkgs.buildEnv {
          name = "ctadl-nightly-test-env";
          # gcc-wrapper and binutils-wrapper both ship a few binutils shims
          # (e.g. `strings`); we want both (gcc to compile, binutils for
          # addr2line), and either copy of the overlapping tools is fine.
          ignoreCollisions = true;
          paths = [
            pkgs.bash
            pkgs.coreutils
            pkgs.cargo
            pkgs.rustc
            pkgs.jq
            pkgs.python3
            pkgs.gcc
            pkgs.binutils
            self.packages.${system}.default
            dex-reader
            androidSdk.androidsdk
            jdk
            pkgs.ghidra-bin
          ];
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

            # The expensive regression suite: source-sink taint tests over Java
            # (DEX) and pcode (C) inputs, run via the vendored ./nightly harness
            # inside testEnv. This is a full (non-local) derivation because it
            # runs ctadl + ghidra + the Android toolchain. The Nix sandbox has no
            # network, so every tool comes from testEnv. Note `tests.sh` takes a
            # ctadl install prefix as $1 and puts its bin/ on PATH.
            regression =
              pkgs.runCommand "ctadl-checks-regression"
                {
                  nativeBuildInputs = [ testEnv ];
                  GHIDRA_HOME = "${pkgs.ghidra-bin}/lib/ghidra";
                  src = ./nightly;
                }
                ''
                  cp -R "$src" ./nightly
                  chmod -R u+w ./nightly
                  cd ./nightly

                  # `dx` lives under the SDK's build-tools, not on the default PATH.
                  export PATH="${androidSdk.androidsdk}/libexec/android-sdk/build-tools/30.0.2:$PATH"
                  export ANDROID_SDK_ROOT="${androidSdk.androidsdk}/libexec/android-sdk"
                  # ghidra/javac want a writable HOME; the sandbox HOME is not.
                  export HOME="$TMPDIR"

                  chmod +x ./tests.sh
                  ./tests.sh ${self.packages.${system}.default}

                  mkdir -p "$out"
                '';

            # Nightly is the superset run on a schedule: the cheap suite plus the
            # expensive regression tests. Interpolating both forces them to build.
            nightly = pkgs.runCommandLocal "ctadl-checks-nightly" { } ''
              {
                echo "cheap:      ${cheap}"
                echo "regression: ${regression}"
              } > "$out"
            '';
          in
          {
            inherit
              fmt
              clippy
              unit-tests
              cheap
              regression
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
