# The TaintBench source-sink regression suite.
#
# TaintBench (https://github.com/TaintBench) ships real Android malware APKs
# with hand-curated ground-truth taint findings. Unlike the nightly
# `regression` suite this needs no javac/dx/Ghidra/Android SDK: ctadl imports
# the prebuilt APK directly (`import -l apk`), so the only tool required at
# runtime is ctadl itself (xtask reads the DEX line map via the in-tree
# dex-reader library, not the binary).
#
# Each app under `<src>/apps/<name>/` carries its ground truth (findings.json),
# query model (model.json), baseline (expected.json), and APK coordinates
# (app.json: url + SRI hash). The APK is fetched here as a fixed-output
# derivation, never committed. Adding an app is data-only: drop in the four
# files and it is picked up automatically by the `readDir` below.
#
# The flake exposes the result as `checks.<system>.taintbench`; see
# ../taintbench/README.md for what the suite checks and how it passes.
{
  pkgs,
  # The suite's data directory; the flake passes its own ./taintbench.
  src,
  # The package providing the `xtask` and `ctadl` binaries (packages.default).
  ctadl,
}:
let
  appsDir = src + "/apps";

  # One entry per app directory that carries an app.json, each with the APK its
  # coordinates name. An app.json with an `excluded` reason is kept in the tree
  # but left out of the run: its APK is never fetched, and xtask reads the same
  # key to report it skipped for that reason rather than for a missing APK.
  apps =
    let
      entries = builtins.readDir appsDir;
      names = builtins.filter (
        n: entries.${n} == "directory" && builtins.pathExists (appsDir + "/${n}/app.json")
      ) (builtins.attrNames entries);
      withMeta = builtins.map (name: {
        inherit name;
        meta = builtins.fromJSON (builtins.readFile (appsDir + "/${name}/app.json"));
      }) names;
      included = builtins.filter (a: !(a.meta ? excluded)) withMeta;
    in
    builtins.map (
      { name, meta }:
      {
        inherit name;
        apk = pkgs.fetchurl {
          url = meta.apk.url;
          hash = meta.apk.sha256;
        };
      }
    ) included;
in
# xtask imports each app's APK, runs the model, and credits a ground-truth
# finding when ctadl reports a connected source->sink path whose endpoint
# callees match it. The APKs are supplied as `--apk <name>=<store-path>`, since
# the sandbox has no network of its own.
pkgs.runCommand "ctadl-checks-taintbench"
  {
    nativeBuildInputs = [ ctadl ];
    inherit src;
  }
  ''
    cp -R "$src" ./taintbench
    chmod -R u+w ./taintbench
    cd ./taintbench
    export HOME="$TMPDIR"

    xtask taintbench --apps-dir ./apps \
      ${pkgs.lib.concatMapStringsSep " " (a: "--apk ${a.name}=${a.apk}") apps}

    mkdir -p "$out"
  ''
