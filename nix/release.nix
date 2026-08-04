# Release binaries: the `ctadl` binary the release workflow attaches to a GitHub
# release, one per distributed target.
#
# `packages.default` cannot be released: it resolves its interpreter and
# libraries under /nix/store, so it only runs on a machine that has one. These
# are built to run anywhere -- Linux and Windows by cross-compiling and linking
# statically, macOS by building natively (Nix has no redistributable macOS SDK
# to cross-compile against) and rewriting its dylib references to /usr/lib.
#
# Which target a given builder can produce:
#
#   builder         | targets
#   ----------------|-------------------------------------------------------
#   x86_64-linux    | x86_64-unknown-linux-musl, x86_64-pc-windows-gnu
#   aarch64-darwin  | aarch64-apple-darwin
#
# CI builds the two cross targets on ubuntu-latest and the Darwin one on a macOS
# runner; see .github/workflows/release.yml. Asking a builder for a target it
# cannot reach is not an error here, it just will not succeed: the attribute set
# is the same on every system, and the failure comes from the build.
#
# The flake exposes these under `legacyPackages.<system>.release.<target>` for
# the same reason the nginx example lives there: a nested set is skipped by
# `nix flake check`, which keeps these slow cross builds -- impossible ones, on
# the wrong builder -- out of the per-PR checks. Build one with
#   nix build .#legacyPackages.<system>.release.<target>
#
# Only `ctadl` ships. `xtask` is the in-tree test harness.
{
  pkgs,
  naersk,
  fenix,
  # The source tree to build; the flake passes its own root.
  src,
}:
let
  inherit (pkgs) lib;
  system = pkgs.stdenv.hostPlatform.system;

  # naersk runs cargo on the builder, so what varies per target is the
  # toolchain, not the stdenv: a cargo and rustc that run here, plus the std of
  # the target being built. nixpkgs' rustc carries std only for its own host,
  # which is what fenix is here to supply.
  toolchainFor =
    target:
    with fenix.packages.${system};
    combine [
      stable.rustc
      stable.cargo
      targets.${target}.stable.rust-std
    ];

  buildFor =
    target: attrs:
    (pkgs.callPackage naersk {
      cargo = toolchainFor target;
      rustc = toolchainFor target;
    }).buildPackage
      (
        {
          inherit src;
          # A cross build cannot run the tests it compiles, and the workspace's
          # tests run in the `test` workflow regardless.
          doCheck = false;
          # Ship only `ctadl`. Selecting the bin here rather than with
          # `cargo build --bin` is deliberate: naersk reuses the build options
          # for a first pass over a stub source tree, where no bin target of
          # this workspace exists yet.
          copyBinsFilter = ''select(.reason == "compiler-artifact" and .executable != null and .profile.test == false and .target.name == "ctadl")'';
          CARGO_BUILD_TARGET = target;
        }
        // attrs
      );

  # Cargo picks the linker, and the `cc` crate picks the C compiler, out of
  # environment variables named after the target triple. Several dependencies
  # compile C (tree-sitter, zstd-sys); left unset, they would quietly build it
  # for the builder instead.
  crossToolEnv =
    crossPkgs: target:
    let
      prefix = crossPkgs.stdenv.cc.targetPrefix;
      upper = lib.toUpper (builtins.replaceStrings [ "-" ] [ "_" ] target);
      lower = builtins.replaceStrings [ "-" ] [ "_" ] target;
    in
    {
      "CARGO_TARGET_${upper}_LINKER" = "${prefix}cc";
      "CC_${lower}" = "${prefix}cc";
      "CXX_${lower}" = "${prefix}c++";
      "AR_${lower}" = "${prefix}ar";
      # Build scripts run on the builder, so the C they compile is the
      # builder's -- not the target's.
      HOST_CC = "${pkgs.stdenv.cc}/bin/cc";
    };
in
{
  # musl with the C runtime linked in, so the binary needs nothing from the host
  # but a kernel. `pkgsCross.musl64` rather than `pkgsStatic` because it names
  # the target outright: pkgsStatic follows the builder's architecture, which
  # would silently produce aarch64 code on an aarch64 builder.
  "x86_64-unknown-linux-musl" = buildFor "x86_64-unknown-linux-musl" (
    {
      strictDeps = true;
      # The cross toolchain runs on the builder but emits code for the target,
      # so it goes in a `depsBuild*` slot rather than `nativeBuildInputs`: that
      # keeps its cc-wrapper setup hook from claiming the plain `cc`/`CC` the
      # build scripts (which must target the builder) rely on. It still lands
      # on PATH, which is all `crossToolEnv`'s prefixed names need.
      depsBuildBuild = with pkgs.pkgsCross.musl64; [
        stdenv.cc
        stdenv.cc.bintools
      ];
      CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
    }
    // crossToolEnv pkgs.pkgsCross.musl64 "x86_64-unknown-linux-musl"
  );

  # mingw-w64, not MSVC: the MSVC target needs Microsoft's linker and CRT, which
  # Nix has no way to fetch. naersk names the copied binary after the artifact
  # cargo reports, so this one arrives as `$out/bin/ctadl.exe`.
  "x86_64-pc-windows-gnu" = buildFor "x86_64-pc-windows-gnu" (
    {
      strictDeps = true;
      depsBuildBuild = with pkgs.pkgsCross.mingwW64; [
        stdenv.cc
        stdenv.cc.bintools
      ];
      # winpthreads is what the GNU target's std links its threading against.
      # It goes in by search path rather than as a build input because it is a
      # Windows library: any dependency slot of this (native) derivation would
      # reject it on its platform.
      CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS = "-L native=${pkgs.pkgsCross.mingwW64.windows.pthreads}/lib";
    }
    // crossToolEnv pkgs.pkgsCross.mingwW64 "x86_64-pc-windows-gnu"
  );

  # Native, so only an aarch64-darwin builder produces this one.
  "aarch64-apple-darwin" = buildFor "aarch64-apple-darwin" {
    nativeBuildInputs = [
      # Rewriting a load command invalidates the code signature, and arm64 macOS
      # refuses to run a binary whose signature does not check out. This hook
      # re-signs (ad-hoc) in fixupPhase what the rewrite below breaks.
      pkgs.darwin.autoSignDarwinBinariesHook
    ];
    # rustc links libiconv out of /nix/store. Point it back at the system copy:
    # macOS resolves /usr/lib/libiconv.2.dylib from the dyld shared cache, so
    # that path works even though no such file exists. Then fail the build if
    # anything store-bound survived, because such a binary runs only on the
    # machine that built it.
    postInstall = ''
      # `tail -n +2` drops otool's header, which is the binary's own path --
      # itself under /nix/store, and not a dependency.
      deps() { otool -L $out/bin/ctadl | tail -n +2; }

      for dylib in $(deps | awk '/^\t\/nix\/store/ { print $1 }'); do
        install_name_tool -change "$dylib" "/usr/lib/$(basename "$dylib")" $out/bin/ctadl
      done
      if deps | grep -q /nix/store; then
        echo "error: release binary still references /nix/store:" >&2
        deps >&2
        exit 1
      fi
    '';
  };
}
