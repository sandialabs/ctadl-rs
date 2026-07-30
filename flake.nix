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

        # baksmali, the reference smali disassembler, used as ground truth for
        # the dex-reader `dex:baksmali` regression check. nixpkgs has no smali
        # package, so we fetch the pinned 3.0.9 "fat" jar (the same version the
        # original dex-reader test used) and wrap it so `baksmali` runs it. The
        # jar is fetched, never committed to this repo.
        baksmali =
          let
            jar = pkgs.fetchurl {
              url = "https://github.com/baksmali/smali/releases/download/3.0.9/baksmali-3.0.9-fat.jar";
              hash = "sha256-r0qBj26b/Koxst4t3ZkRfZlyXXbWZuklfmM9MF0r1NQ=";
            };
          in
          pkgs.writeShellScriptBin "baksmali" ''
            exec ${jdk}/bin/java -jar ${jar} "$@"
          '';

        # `dex-reader` is a workspace-excluded crate (its bins must stay out of
        # the distro), so we build it on its own for the linemap step of the DEX
        # tests rather than expecting it in packages.default.
        dex-reader = naersk-lib.buildPackage {
          src = ./dex-reader;
          release = false;
        };

        # `jvm-reader` is workspace-excluded for the same reason; build its bin
        # for the JVM E2E linemap step rather than expecting it in packages.default.
        jvm-reader = naersk-lib.buildPackage {
          src = ./jvm-reader;
          release = false;
        };

        # The two classes jvm-reader's `flow.rs` unit tests load at runtime,
        # compiled from the committed sample sources (no `.class` is committed).
        # Passed to the `jvm-reader-tests` check via JVM_READER_TEST_FIXTURES.
        jvmTestFixtures =
          pkgs.runCommand "jvm-reader-test-fixtures"
            {
              nativeBuildInputs = [ jdk ];
              # Import the dir (not the files) so the sources keep their real
              # names; javac requires a public class's file to match its name.
              src = ./jvm-reader/tests/sample;
            }
            ''
              mkdir -p "$out"
              javac -d "$out" "$src/HelloWorld.java" "$src/ArrayFlow.java" "$src/LoopFlow.java"
            '';

        # The external toolchain the regression scripts expect on PATH
        # (dex-reader, jvm-reader, javac, dx, gcc/addr2line, ghidra, jq,
        # python3). Deliberately excludes this repo's own package so that
        # `devShells.regression` can be used to run parts of the suite against a
        # locally built ctadl/xtask; the `regression` check adds the Nix-built
        # package on top.
        testEnv = pkgs.buildEnv {
          name = "ctadl-nightly-test-env";
          # The cc-wrapper and binutils-wrapper both ship a few overlapping
          # binutils shims (e.g. `strings`); either copy is fine.
          ignoreCollisions = true;
          paths = [
            pkgs.bash
            pkgs.coreutils
            pkgs.cargo
            pkgs.rustc
            pkgs.jq
            pkgs.python3
            # `xtask regression` validates the SARIF each taint case emits by
            # running this; without it on PATH the checks self-skip.
            checksarif
            # The C compiler `pick_toolchain` prefers. A native `pkgs.gcc` here
            # would also claim `cc`, and on Darwin that is fatal: rustc links
            # through `cc`, and gcc passes -no_compact_unwind, so the binary
            # ends up with no __unwind_info -- which the arm64 unwinder needs to
            # find a handler. Every panic then aborts instead of unwinding.
            # Every binary this ships is `x86_64-unknown-linux-gnu-` prefixed,
            # so it never claims `cc` and the stdenv's clang stays the linker.
            # Where it is not cross (x86_64 Linux) it collapses to the native
            # gcc, which is the right linker there anyway.
            pkgs.pkgsCross.gnu64.stdenv.cc
            pkgs.binutils
            dex-reader
            jvm-reader
            androidSdk.androidsdk
            jdk
            baksmali
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
        # Example target binaries live under `legacyPackages` rather than
        # `packages` on purpose: `nix flake check` requires every
        # `packages.<system>.<name>` to be a flat derivation, but this is a
        # nested set (`examples.nginx`). `legacyPackages` is the standard home
        # for nested/non-conforming trees and is skipped by flake check, which
        # also keeps the slow cross-compile out of the check. Build it with
        #   nix build .#legacyPackages.<system>.examples.nginx
        legacyPackages.examples.nginx = pkgs.pkgsCross.gnu64.enableDebugging (
          pkgs.pkgsCross.gnu64.nginx.override {
            withDebug = true;
            withPerl = false;
            perl = null;
          }
        );

        # CI/test logic lives under `checks` so the nightly GitHub workflow can run
        #   nix build .#checks.<system>.regression .#checks.<system>.dex-reader-tests
        # instead of duplicating the test harness in YAML. This does not rely on
        # `nix flake check`; the check is an ordinary buildable derivation. The
        # cheap per-PR checks (fmt/clippy/unit tests) are intentionally NOT here:
        # they run as a plain cargo job in the `test` workflow. Nix is reserved
        # for the expensive regression suite, whose toolchain (ctadl + ghidra +
        # the Android SDK) is what makes a reproducible Nix environment worthwhile.
        checks =
          let
            # The expensive regression suite: source-sink taint tests over Java
            # (DEX) and pcode (C) inputs. The orchestration lives in the `xtask`
            # crate (`cargo xtask regression`); here we just run the prebuilt
            # `xtask` binary that ships in packages.default. This is a full
            # (non-local) derivation because it runs ctadl + ghidra + the Android
            # toolchain. The Nix sandbox has no network, so every tool comes from
            regression =
              pkgs.runCommand "ctadl-checks-regression"
                {
                  nativeBuildInputs = [
                    testEnv
                    self.packages.${system}.default
                  ];
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

                  # The jvm-reader checks compile the sample .java sources (which
                  # live in the crate, outside ./nightly) and exercise jvm-reader
                  # on the resulting .class/.jar. The dex-reader checks compile
                  # the same samples down to .dex (via `dx`) and also parse a
                  # real-world APK owned by xtask. javac/javap/jar/dx come from
                  # the JDK and Android SDK in testEnv / PATH.
                  # xtask normally rebuilds ctadl from source to guard against a
                  # stale binary, but the Nix sandbox has no source tree or cargo.
                  # Point it at the ctadl that ships in packages.default instead.
                  export CTADL_BIN="${self.packages.${system}.default}/bin/ctadl"
                  # The `models:*` checks hold the model files ctadl ships to
                  # `ctadl-model-generator.schema.json`. Both live in the
                  # ctadl-ascent crate, outside ./nightly, and this sandbox has
                  # no source tree, so point xtask at them explicitly or they
                  # would self-skip here -- which is the one place the drift is
                  # meant to be caught.
                  ${self.packages.${system}.default}/bin/xtask regression \
                    --jvm-samples ${./jvm-reader/tests/sample} \
                    --dex-apk ${./xtask/tests/dex/com.noto_54.apk} \
                    --models-dir ${./ctadl-ascent/src/models}

                  mkdir -p "$out"
                '';

            # The dex-reader crate is workspace-excluded (its bins must not ship
            # in the distro), so `cargo test --workspace` never runs its tests.
            # Run its pure-Rust unit tests here instead, built offline by naersk
            # in test mode. No external toolchain needed. The integration-style
            # full-parse checks (compiled samples + the real-world APK) were
            # moved to `xtask regression`.
            dex-reader-tests = naersk-lib.buildPackage {
              src = ./dex-reader;
              mode = "test";
            };

            # jvm-reader is workspace-excluded for the same reason (its bin must
            # not ship in the distro), which also keeps its tests out of
            # `cargo test --workspace`. Run them here. Its `flow.rs` unit tests
            # are `#[ignore]`d and load two classes at runtime from
            # JVM_READER_TEST_FIXTURES (compiled from source by jvmTestFixtures,
            # below); `--include-ignored` runs them. The integration-style
            # checks were moved to `xtask regression`.
            jvm-reader-tests = naersk-lib.buildPackage {
              src = ./jvm-reader;
              mode = "test";
              JVM_READER_TEST_FIXTURES = "${jvmTestFixtures}";
              cargoTestOptions = opts: opts ++ [ "--" "--include-ignored" ];
            };
          in
          {
            inherit regression dex-reader-tests jvm-reader-tests;
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
              nil
              nixd
              pkg-config
              bzip2
              ghidra-bin
            ];
            RUST_SRC_PATH = rustPlatform.rustLibSrc;
          };

          # nix develop .#regression
          devShells.regression = pkgs.mkShell {
            buildInputs = with pkgs; [
              nil
              nixd
            ];
            packages = [ testEnv ];

            GHIDRA_HOME = "${pkgs.ghidra-bin}/lib/ghidra";
            ANDROID_SDK_ROOT = "${androidSdk.androidsdk}/libexec/android-sdk";
            RUST_SRC_PATH = pkgs.rustPlatform.rustLibSrc;

            shellHook = ''
              export PATH="${androidSdk.androidsdk}/libexec/android-sdk/build-tools/30.0.2:$PATH"
            '';
          };
      }
    );
}
