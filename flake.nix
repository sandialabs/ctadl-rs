{
  inputs = {
    naersk.url = "github:nix-community/naersk/master";
    nixpkgs.url = "github:NixOS/nixpkgs";
    utils.url = "github:numtide/flake-utils";
    ctadl-souffle.url = "github:sandialabs/ctadl";
    # Only the release builds use fenix: they cross-compile, and cross-compiling
    # with naersk needs a rustc that carries the *target's* std alongside a cargo
    # that runs on the builder. fenix serves those upstream Rust artifacts per
    # target; nixpkgs' rustc only ships std for its own host. Everything else in
    # this flake still builds with the nixpkgs toolchain.
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      utils,
      naersk,
      ctadl-souffle,
      fenix,
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

        # The one version this tree has. Read rather than repeated so these
        # derivations stay named after it; see the root Cargo.toml.
        workspaceVersion = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

        # `dex-reader` is the DEX dumper the linemap step of the DEX regression
        # tests runs. It is an *example* of the dex-reader crate, not a bin, so
        # that a plain `cargo build` -- which is what packages.default runs --
        # leaves it out of the distro. Examples are not built by default, so
        # ask for it by name.
        #
        # Two details of the target selection:
        #
        # - The src is the whole workspace, not ./dex-reader. dex-reader is a
        #   workspace member now, and a member cannot be built alone: its
        #   `version.workspace = true` needs the root manifest.
        # - `--lib` rides along with `--examples` for naersk's sake. naersk
        #   builds dependencies in a first pass over a stub source tree, which
        #   has no examples/ directory; with `--examples` alone that pass
        #   matches no targets, builds nothing, and the dependency caching it
        #   exists for is lost. `--lib` gives it the crate's real dependencies
        #   to chew on.
        #
        # naersk installs whatever cargo reports as an executable, and an
        # example is one, so this lands at $out/bin/dex-reader as before.
        dex-reader = naersk-lib.buildPackage {
          src = ./.;
          version = workspaceVersion;
          name = "dex-reader";
          release = false;
          cargoBuildOptions = x: x ++ [ "--package" "dex-reader" "--lib" "--examples" ];
        };

        # `jvm-reader` is the same arrangement for the JVM E2E linemap step.
        jvm-reader = naersk-lib.buildPackage {
          src = ./.;
          version = workspaceVersion;
          name = "jvm-reader";
          release = false;
          cargoBuildOptions = x: x ++ [ "--package" "jvm-reader" "--lib" "--examples" ];
        };

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

        # The distributable `ctadl` binaries, one per released target. What each
        # one is, why it cannot just be `packages.default`, and which builder
        # can produce which target all live in ./nix/release.nix.
        release = import ./nix/release.nix {
          inherit pkgs naersk fenix;
          src = ./.;
          version = workspaceVersion;
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

        # The distributable binaries. They live under `legacyPackages` rather
        # than `packages` for the same reason the example above does; see
        # ./nix/release.nix for the rest.
        legacyPackages.release = release;

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
                  # `--dex-apk` feeds a second family too: the `apk:*` checks,
                  # which drive ctadl itself over that same app -- import it,
                  # read the store back, model-check it unindexed. They were
                  # `#[test]`s in ctadl-ascent until the ~13 s import made them
                  # most of `cargo test`'s wall clock. They need no toolchain,
                  # only the CTADL_BIN below.
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

            # dex-reader's pure-Rust unit tests, built offline by naersk in test
            # mode. No external toolchain needed. The integration-style
            # full-parse checks (compiled samples + the real-world APK) were
            # moved to `xtask regression`.
            #
            # `cargo test --workspace` in the test workflow now covers these
            # too -- dex-reader is a workspace member, where it used to be
            # excluded. This check is kept because it runs on the nightly's Nix
            # builder with no toolchain assumptions, and because it costs
            # nothing to keep.
            #
            # `--package` is not optional here: without it, `src = ./.` would
            # test the whole workspace, which is a far larger build than this
            # check wants.
            dex-reader-tests = naersk-lib.buildPackage {
              src = ./.;
              version = workspaceVersion;
              name = "dex-reader-tests";
              mode = "test";
              cargoBuildOptions = x: x ++ [ "--package" "dex-reader" ];
              cargoTestOptions = opts: opts ++ [ "--package" "dex-reader" ];
            };

            # jvm-reader's unit tests, same arrangement -- and, like dex-reader's,
            # now entirely hermetic: no `#[ignore]`, no JDK, no fixture directory.
            # Everything that needed a compiled class moved out, to the `jvm:*`
            # checks in `xtask regression` and to the `SwitchFlow` /
            # `StringSwitchFlow` / `WideParamFlow` / `ShiftFlow` taint cases.
            jvm-reader-tests = naersk-lib.buildPackage {
              src = ./.;
              version = workspaceVersion;
              name = "jvm-reader-tests";
              mode = "test";
              cargoBuildOptions = x: x ++ [ "--package" "jvm-reader" ];
              cargoTestOptions = opts: opts ++ [ "--package" "jvm-reader" ];
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
