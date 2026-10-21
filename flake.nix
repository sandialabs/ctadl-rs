{
  inputs = {
    naersk.url = "github:nix-community/naersk/master";
    nixpkgs.url = "github:NixOS/nixpkgs";
    utils.url = "github:numtide/flake-utils";
    ctadl-souffle.url = "github:sandialabs/ctadl";
    # Upstream Rust releases, any version; nixpkgs has only its own rustc.
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Mono.Cecil, for testing cil-reader.
    cecil = {
      url = "github:jbevain/cecil/882ca5eedda1e62eb41bd5869aeb15d8f1538e51";
      flake = false;
    };
    # dnlib, for the tools that write cil-reader's Ptr-table and gap fixtures
    # (dnlibTool in ./nix/cil.nix).
    dnlib = {
      url = "github:0xd4d/dnlib/9ab9b58ab31b99b3f6cdef36efec613fc11ddbe2";
      flake = false;
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
      cecil,
      dnlib,
    }:
    utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          # The regression check and the dev shell pull in unfree components
          # (the Android SDKs and ghidra-bin), and the Android SDKs need their
          # license accepted non-interactively.
          config = {
            allowUnfree = true;
            android_sdk.accept_license = true;
          };
        };
        # The Rust toolchain rust-toolchain.toml names, the one the `test` workflow
        # runs. Everything here builds and checks with it, so a feature newer
        # than the pinned release fails locally the same way it fails in CI.
        # `sha256` is the hash of that release's manifest; update it with the
        # channel.
        rustToolchainFile = (builtins.fromTOML (builtins.readFile ./rust-toolchain.toml)).toolchain;
        rustRelease = {
          inherit (rustToolchainFile) channel;
          sha256 = "sha256-zC8E38iDVJ1oPIzCqTk/Ujo9+9kx9dXq7wAwPMpkpg0=";
        };
        rustDist = fenix.packages.${system}.toolchainOf rustRelease;
        rustToolchain = fenix.packages.${system}.combine (
          map (c: rustDist.${c}) (
            [
              "rustc"
              "cargo"
              "rust-std"
            ]
            ++ rustToolchainFile.components
          )
        );
        naersk-lib = pkgs.callPackage naersk {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };
        ctadl-souffle-wrapper = pkgs.writeShellScriptBin "ctadl-souffle" ''
          exec ${ctadl-souffle.packages.${system}.ctadlPackages.ctadl-full}/bin/ctadl "$@"
        '';
        checksarif = pkgs.callPackage ./nix/sarif-multitool/checksarif.nix { };

        # The one version this tree has. Read rather than repeated so these
        # derivations stay named after it; see the root Cargo.toml.
        workspaceVersion = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

        android = import ./nix/android-jvm.nix { inherit pkgs naersk-lib workspaceVersion; };

        cil = import ./nix/cil.nix { inherit pkgs cecil dnlib; };

        # The external toolchain `xtask regression` expects on PATH.
        # Deliberately excludes this repo's own package so that the
        # dev shell can be used to run parts of the suite against a locally
        # built ctadl/xtask; the `regression` check adds the Nix-built package
        # on top.
        testEnv = pkgs.buildEnv {
          name = "ctadl-nightly-test-env";
          # The cc-wrapper and binutils-wrapper both ship a few overlapping
          # binutils shims (e.g. `strings`); either copy is fine.
          ignoreCollisions = true;
          paths = [
            pkgs.bash
            pkgs.coreutils
            rustToolchain
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
            # The 32-bit x86 toolchain (`i686-unknown-linux-gnu-` prefixed, so it
            # claims no `cc` either), for the JNI cases that need a 32-bit
            # library: the ABI on which a `long` arrives split in two.
            pkgs.pkgsCross.gnu32.stdenv.cc
            pkgs.binutils
            android.dex-reader
            android.jvm-reader
            android.androidSdk.androidsdk
            android.jdk
            android.baksmali
            android.smali
            android.asmClasspath
            pkgs.ghidra-bin
            # The source cases need the SDK, and CIL imports discover framework
            # metadata through dotnet on PATH (the wrapped runners alone do not).
            pkgs.dotnet-sdk_8
            cil.cecilDump
            cil.flowRun
            cil.modelRun
          ];
        };

        release = import ./nix/release.nix {
          inherit
            pkgs
            naersk
            fenix
            rustRelease
            ;
          src = ./.;
          version = workspaceVersion;
        };
      in
      {
        packages.default = naersk-lib.buildPackage ./.;
        # Exposed so `nix build .#cecil-dump` can warm the cache on its own.
        packages.cecil-dump = cil.cecilDump;
        packages.cil-fixtures = cil.cilFixtures;
        packages.flow-run = cil.flowRun;
        packages.model-run = cil.modelRun;
        packages.ptr-gen = cil.ptrGen;
        packages.gap-gen = cil.gapGen;
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

        # The distributable binaries, under `legacyPackages` for the same reason as
        # the example above; what each is and which builder produces it are in
        # ./nix/release.nix. Build one with
        #   nix build .#legacyPackages.<system>.release.<target>
        legacyPackages.release = release;

        # CI/test logic lives under `checks` so the nightly GitHub workflow can
        # `nix build .#checks.<system>.<name>` instead of duplicating the test
        # harness in YAML. This does not rely on
        # `nix flake check`; the check is an ordinary buildable derivation. The
        # cheap per-PR checks (fmt/clippy/unit tests) are intentionally NOT here:
        # they run as a plain cargo job in the `test` workflow. Nix is reserved
        # for the expensive regression suite, whose toolchain (ctadl + ghidra +
        # the Android SDK) is what makes a reproducible Nix environment worthwhile.
        checks =
          let
            # The expensive regression suite: every frontend's source-sink taint
            # cases plus the checks that are not taint cases (nightly/README.md
            # lists them). The orchestration lives in
            # the `xtask` crate (`cargo xtask regression`); here we just run the
            # prebuilt `xtask` binary that ships in packages.default. This is a
            # full (non-local) derivation because it runs ctadl + ghidra + the
            # Android toolchain. The Nix sandbox has no network, so every tool
            # comes from testEnv.
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
                  # Keep the known exceptions beside the goldens where xtask's
                  # resolver expects them. These files are outside ./nightly.
                  mkdir xtask
                  ln -s ${./xtask/cil-goldens} xtask/cil-goldens
                  ln -s ${./xtask/cil-taint-known.json5} xtask/cil-taint-known.json5
                  ln -s ${./xtask/cil-models-known.json5} xtask/cil-models-known.json5
                  cd ./nightly

                  export PATH="${android.androidSdkBuildTools}:$PATH"
                  export ANDROID_SDK_ROOT="${android.androidSdk.androidsdk}/libexec/android-sdk"
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
                  # read the store back, model-check it unindexed. They need no
                  # toolchain, only the CTADL_BIN below.
                  # xtask normally rebuilds ctadl from source to guard against a
                  # stale binary, but the Nix sandbox has no source tree or cargo.
                  # Point it at the ctadl that ships in packages.default instead.
                  export CTADL_BIN="${self.packages.${system}.default}/bin/ctadl"
                  export CTADL_ANDROID_ICC_APKS="${android.droidbenchIccApks}"
                  # The `models:*` checks hold the model files ctadl ships to
                  # `ctadl-model-generator.schema.json`. Both live in the
                  # ctadl-ascent crate, outside ./nightly, and this sandbox has
                  # no source tree, so point xtask at them explicitly or they
                  # would self-skip here -- which is the one place the drift is
                  # meant to be caught.
                  # The `cil:*` checks read Cecil's test corpus (the pinned
                  # input), the Cecil goldens and the built fixtures, and diff
                  # against the `cecil-dump` that testEnv puts on PATH.
                  ${self.packages.${system}.default}/bin/xtask regression \
                    --frontend dex,jvm,pcode,lua,jni,c,android-icc,cil \
                    --jvm-samples ${./readers/jvm-reader/tests/sample} \
                    --dex-apk ${./xtask/tests/dex/com.noto_54.apk} \
                    --models-dir ${./ctadl-ascent/src/models} \
                    --cil-goldens ${./xtask/cil-goldens} \
                    --cecil-src ${cecil} \
                    --cil-fixtures ${cil.cilFixtures}

                  mkdir -p "$out"
                '';

          in
          {
            inherit regression;
            inherit (android) dex-reader-tests jvm-reader-tests;
          };

        formatter = pkgs.nixfmt;
        # One shell for everything: the day-to-day Rust/Nix tooling plus testEnv,
        # so `cargo xtask regression` runs straight from here.
        devShells.default =
          with pkgs;
          mkShell {
            buildInputs = [
              pre-commit
              rustToolchain
              cargo-typify
              rust-analyzer
              cargo-expand
              sarif-tools
              cargo-flamegraph
              (python3.withPackages (ps: [
                ps.pyarrow
                # The frida Python API, for scripting hooks with structured results.
                ps.frida-python
              ]))
              ctadl-souffle-wrapper
              parquet-tools
              graphviz
              checksarif
              nil
              nixd
              pkg-config
              bzip2
              ghidra-bin
              # Android app debugging; see androidDebugSdk in ./nix/android-jvm.nix. jdb comes
              # with the JDK in testEnv.
              frida-tools
              apktool
              jadx
              scrcpy
              android.androidLldb
            ];
            packages = [ testEnv ];

            RUST_SRC_PATH = "${rustDist.rust-src}/lib/rustlib/src/rust/library";
            # The pinned Mono.Cecil checkout: cil-reader's tests, the `cil:*`
            # checks and `cargo xtask cil` read it.
            CECIL_SRC = "${cecil}";
            # cil-reader's compiled fixtures, for the same consumers.
            CIL_READER_FIXTURES = "${cil.cilFixtures}";
            GHIDRA_HOME = "${pkgs.ghidra-bin}/lib/ghidra";
            # The debugging SDK, so the emulator finds its system image through
            # this.
            ANDROID_SDK_ROOT = "${android.androidDebugSdk.androidsdk}/libexec/android-sdk";
            ANDROID_HOME = "${android.androidDebugSdk.androidsdk}/libexec/android-sdk";
            ANDROID_NDK_ROOT = "${android.androidDebugSdk.androidsdk}/libexec/android-sdk/ndk-bundle";
            ANDROID_DEBUG_SYSTEM_IMAGE = android.androidDebugSystemImage;
            FRIDA_SERVER = "${android.fridaServer}";
            CTADL_ANDROID_ICC_APKS = "${android.droidbenchIccApks}";
            DOTNET_NOLOGO = "1";
            DOTNET_CLI_TELEMETRY_OPTOUT = "1";
            DOTNET_SKIP_FIRST_TIME_EXPERIENCE = "1";

            # The debugging SDK's bin/ (adb, emulator, avdmanager, ...) goes
            # ahead of testEnv's, which carries the regression SDK's adb too.
            # The NDK's toolchain stays off PATH, where its clang would shadow
            # the stdenv's; ANDROID_LLDB_SERVER names the binary to push for
            # `android-lldb`, and simpleperf is under $ANDROID_NDK_ROOT/simpleperf.
            shellHook = ''
              export PATH="${android.androidDebugSdk.androidsdk}/bin:${android.androidDebugSdkBuildTools}:$PATH"
              ANDROID_LLDB_SERVER=$(echo "$ANDROID_NDK_ROOT"/toolchains/llvm/prebuilt/*/lib/clang/*/lib/linux/${android.androidLldbArch}/lldb-server)
              export ANDROID_LLDB_SERVER
            '';
          };
      }
    );
}
