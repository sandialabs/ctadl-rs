# Kani, the Rust model checker, pinned and built without a network.
#
# Kani's own installer (`cargo kani setup`, from the `kani-verifier` crate)
# downloads a release bundle and installs a nightly toolchain with rustup, which
# the Nix sandbox forbids. This does what that setup does, from fixed-hash
# inputs:
#
#   1. Unpack the release bundle into `$out/kani-<version>/`, the directory
#      Kani finds itself in through `KANI_HOME`. It holds `bin/` (kani-driver,
#      kani-compiler, cbmc and the other CBMC tools), Kani's libraries, and
#      the name of the toolchain it was built against, in
#      `rust-toolchain-version` and `rustc-version`.
#   2. Link `kani-<version>/toolchain` to that nightly, after checking that its
#      `rustc --version` is what `rustc-version` says. kani-compiler loads
#      `librustc_driver` and `libLLVM` from that toolchain's `lib/`, and
#      kani-driver runs `toolchain/bin/cargo`.
#   3. On Linux, give the ELF binaries the store's interpreter and runtime
#      libraries (autoPatchelfHook; setup patchelfs only on NixOS). Nothing on
#      macOS, where kani-compiler's rpath already names `../toolchain/lib`.
#
# `cargo-kani` and `kani` are wrappers that do what the `kani-verifier`
# crate's binaries do once setup is done: run `kani-driver` under the name
# they were called by, with `KANI_HOME`, `PATH` and `RUSTUP_TOOLCHAIN` set.
# They also put a `gcc` on `PATH`, which goto-cc needs.
#
# The version is pinned here and nowhere else. To bump it, change `version`,
# the nightly's `date` and `sha256` (the date is in the new bundle's
# `rust-toolchain-version`; the build fails if it does not match), and the
# four bundle hashes.
{
  pkgs,
  fenix,
}:
let
  inherit (pkgs) lib stdenv;
  system = stdenv.hostPlatform.system;

  version = "0.68.0";

  # The nightly this Kani was built against. `sha256` is the hash of that
  # nightly's manifest.
  nightly = {
    channel = "nightly";
    date = "2026-08-21";
    sha256 = "sha256-PDDMZVp1SdCzABXNAy+Unocj2lrQOfZy0EUgu66k520=";
  };

  # The release bundle for each system the flake builds on.
  bundles = {
    aarch64-darwin = {
      target = "aarch64-apple-darwin";
      hash = "sha256-pdOaXV50glOlU6pi8pXGw5coeSeijl4yaR1/8u2gw5g=";
    };
    x86_64-darwin = {
      target = "x86_64-apple-darwin";
      hash = "sha256-sTZ/aUwTUKvK4TNwH3izvj/W6b62ZoGiLqC8rAzDSAg=";
    };
    aarch64-linux = {
      target = "aarch64-unknown-linux-gnu";
      hash = "sha256-AELSaIvxVQJw3vSnnJaq7qE4YxgYy0q1SAEg8tUalCw=";
    };
    x86_64-linux = {
      target = "x86_64-unknown-linux-gnu";
      hash = "sha256-MuK0hNc+3gu/ZKLPCHnEJZQiSX2K7E7mfUSN6a54Q9M=";
    };
  };
  bundle = bundles.${system};

  nightlyDist = fenix.packages.${system}.toolchainOf nightly;
  toolchain = fenix.packages.${system}.combine (
    map (c: nightlyDist.${c}) [
      "rustc"
      "cargo"
      "rust-std"
      "llvm-tools"
      "rustc-dev"
      "rust-src"
      "rustfmt"
    ]
  );

  kaniDir = "kani-${version}";
in
stdenv.mkDerivation {
  pname = "kani";
  inherit version;

  src = pkgs.fetchurl {
    url = "https://github.com/model-checking/kani/releases/download/kani-${version}/kani-${version}-${bundle.target}.tar.gz";
    inherit (bundle) hash;
  };

  nativeBuildInputs = [
    pkgs.makeWrapper
  ]
  ++ lib.optionals stdenv.hostPlatform.isLinux [ pkgs.autoPatchelfHook ];
  # What autoPatchelfHook resolves the bundle's libraries from: the compiler
  # runtime (libgcc_s, and libstdc++ for any binary that is not static), and
  # the toolchain's `lib/` for kani-compiler.
  buildInputs = lib.optionals stdenv.hostPlatform.isLinux [
    stdenv.cc.cc.lib
    toolchain
  ];

  dontConfigure = true;
  dontBuild = true;
  # Keep the bundle's binaries as released.
  dontStrip = true;

  installPhase = ''
    runHook preInstall

    dir="$out/${kaniDir}"
    mkdir -p "$dir"
    cp -R . "$dir"

    expected="${nightly.channel}-${nightly.date}-${bundle.target}"
    actual=$(cat "$dir/rust-toolchain-version")
    if [[ "$actual" != "$expected" ]]; then
      echo "kani ${version} wants toolchain $actual, but kani.nix pins $expected" >&2
      exit 1
    fi
    rustc_version=$(${toolchain}/bin/rustc --version)
    if [[ "$rustc_version" != "$(cat "$dir/rustc-version")" ]]; then
      echo "kani ${version} was built with $(cat "$dir/rustc-version"), but the pinned nightly is $rustc_version" >&2
      exit 1
    fi
    ln -s ${toolchain} "$dir/toolchain"

    # goto-cc preprocesses C by running `gcc`, which the macOS stdenv's
    # compiler does not provide by that name.
    mkdir -p "$out/libexec/kani"
    ln -s ${stdenv.cc}/bin/cc "$out/libexec/kani/gcc"

    for bin in cargo-kani kani; do
      makeWrapper "$dir/bin/kani-driver" "$out/bin/$bin" \
        --argv0 "$bin" \
        --set KANI_HOME "$out" \
        --prefix PATH : "$dir/bin:$out/libexec/kani" \
        --set RUSTUP_TOOLCHAIN "$actual"
    done

    runHook postInstall
  '';

  # kani-compiler is linked against this nightly's compiler. Fail here, not on
  # first use, if the loader would take either library from anywhere else.
  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck

    compiler="$out/${kaniDir}/bin/kani-compiler"
    lib="${toolchain}/lib"
    # The loader's own list of what it loads for kani-compiler, one path per
    # line: after `=>` (glibc), or after `dyld[pid]: <uuid> ` (macOS).
    ${
      if stdenv.hostPlatform.isDarwin then
        ''loaded=$(DYLD_PRINT_LIBRARIES=1 "$compiler" --version 2>&1 | sed -n 's|^dyld\[[0-9]*\]: [^/]*||p' || true)''
      else
        ''loaded=$(LD_TRACE_LOADED_OBJECTS=1 "$compiler" | sed -n 's/.* => \([^ ]*\) .*/\1/p')''
    }
    for want in librustc_driver- libLLVM; do
      path=$(echo "$loaded" | grep "/$want" | head -n1)
      # The toolchain's `lib/` is a join of symlinks, so compare what both
      # resolve to.
      if [[ -z "$path" || ! -e "$lib/$(basename "$path")" \
            || "$(realpath "$path")" != "$(realpath "$lib/$(basename "$path")")" ]]; then
        echo "kani-compiler loads $want from '$path', not from $lib. It loads:" >&2
        echo "$loaded" >&2
        exit 1
      fi
      echo "kani-compiler loads $want from $path"
    done
    $out/bin/cargo-kani --version

    runHook postInstallCheck
  '';

  passthru = { inherit toolchain; };

  meta = {
    description = "Kani Rust Verifier, from its release bundle";
    homepage = "https://github.com/model-checking/kani";
    license = with lib.licenses; [
      asl20
      mit
    ];
    platforms = builtins.attrNames bundles;
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
}
