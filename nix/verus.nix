# Verus, the SMT-based Rust verifier, pinned and run without rustup.
#
# A Verus release bundle holds the `verus` driver, `rust_verify` (a rustc
# driver linked against one stable toolchain's `librustc_driver`), the
# prebuilt `vstd` library, and the Z3 that release was tested with. The
# driver normally runs `rustup run <toolchain> rust_verify`, which the Nix
# sandbox cannot do. With `VERUS_USE_RUSTUP=0` it runs `rust_verify` directly
# and leaves finding the toolchain to us, so this:
#
#   1. Unpacks the bundle into `$out/libexec/verus`, where the driver finds
#      `rust_verify`, `vstd` and `z3` beside itself.
#   2. Checks that the toolchain the bundle names (`version.json`) is the one
#      pinned here, and that the bundled Z3 is the version expected.
#   3. Wraps `verus` so that `rust_verify` loads `librustc_driver` and
#      `libLLVM` from that toolchain (an `@rpath` load with no rpath on macOS;
#      autoPatchelfHook adds one on Linux), and with the toolchain's `bin/`
#      first on PATH, as `rustup run` would put it.
#
# The version is pinned here and nowhere else. To bump it, change `version`,
# the toolchain's `channel` and `sha256`, `z3Version`, and the bundle hashes.
{
  pkgs,
  fenix,
}:
let
  inherit (pkgs) lib stdenv;
  system = stdenv.hostPlatform.system;

  version = "0.2026.11.02.8bf68ee";

  # The stable release this Verus was built against. `sha256` is the hash of
  # that release's manifest.
  rustRelease = {
    channel = "1.99.1";
    sha256 = "sha256-+xUr6Al1wZSqX8aEsq7DpuFOK4Ve/i+qTl+K5uWQ9GM=";
  };

  # The Z3 the bundle ships, which is the one Verus requires.
  z3Version = "4.16.0";

  bundles = {
    aarch64-darwin = {
      name = "arm64-macos";
      target = "aarch64-apple-darwin";
      hash = "sha256-QIlKURRpQNPWIiIU/3YGmhMd6NVIpEj1R7RdCvKTDz8=";
    };
    x86_64-darwin = {
      name = "x86-macos";
      target = "x86_64-apple-darwin";
      hash = "sha256-wM60V2N5l8XP7zl0uresazJcljAONAzKrZis0VEmCKA=";
    };
    aarch64-linux = {
      name = "arm64-linux";
      target = "aarch64-unknown-linux-gnu";
      hash = "sha256-oPeng3aQhXtbC3hf+MG/uTlR5yL4udF1ZYzC/rse8F4=";
    };
    x86_64-linux = {
      name = "x86-linux";
      target = "x86_64-unknown-linux-gnu";
      hash = "sha256-m7u6bXGFeis39gpBlD5o3z4Btl1/CpbYuog03tzmKQA=";
    };
  };
  bundle = bundles.${system};

  rustDist = fenix.packages.${system}.toolchainOf rustRelease;
  toolchain = fenix.packages.${system}.combine (
    map (c: rustDist.${c}) [
      "rustc"
      "cargo"
      "rust-std"
    ]
  );
in
stdenv.mkDerivation {
  pname = "verus";
  inherit version;

  src = pkgs.fetchzip {
    url = "https://github.com/verus-lang/verus/releases/download/release/${version}/verus-${version}-${bundle.name}.zip";
    inherit (bundle) hash;
  };

  nativeBuildInputs = [
    pkgs.makeWrapper
    pkgs.jq
  ]
  ++ lib.optionals stdenv.hostPlatform.isLinux [ pkgs.autoPatchelfHook ];
  buildInputs = lib.optionals stdenv.hostPlatform.isLinux [
    stdenv.cc.cc.lib
    pkgs.zlib
    toolchain
  ];

  dontConfigure = true;
  dontBuild = true;
  dontStrip = true;

  installPhase = ''
    runHook preInstall

    dir="$out/libexec/verus"
    mkdir -p "$dir"
    cp -R . "$dir"

    expected="${rustRelease.channel}-${bundle.target}"
    actual=$(jq -r .verus.toolchain "$dir/version.json" | cut -d' ' -f1)
    if [[ "$actual" != "$expected" ]]; then
      echo "verus ${version} wants toolchain $actual, but verus.nix pins $expected" >&2
      exit 1
    fi
    if [[ "$(jq -r .verus.version "$dir/version.json")" != "${version}" ]]; then
      echo "the bundle is not verus ${version}" >&2
      exit 1
    fi

    makeWrapper "$dir/verus" "$out/bin/verus" \
      --set VERUS_USE_RUSTUP 0 \
      --prefix PATH : "${toolchain}/bin" \
      ${
        if stdenv.hostPlatform.isDarwin then
          ''--prefix DYLD_FALLBACK_LIBRARY_PATH : "${toolchain}/lib"''
        else
          ''--prefix LD_LIBRARY_PATH : "${toolchain}/lib"''
      }

    runHook postInstall
  '';

  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck

    z3=$("$out/libexec/verus/z3" --version)
    if [[ "$z3" != "Z3 version ${z3Version} "* ]]; then
      echo "the bundle's Z3 is '$z3', not ${z3Version}" >&2
      exit 1
    fi
    $out/bin/verus --version

    runHook postInstallCheck
  '';

  passthru = { inherit toolchain; };

  meta = {
    description = "Verus, an SMT-based verifier for Rust, from its release bundle";
    homepage = "https://github.com/verus-lang/verus";
    license = lib.licenses.mit;
    platforms = builtins.attrNames bundles;
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
}
