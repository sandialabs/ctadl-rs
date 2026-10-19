# The dex/Android/JVM toolchain: the Android SDKs (the one the regression suite
# builds with, and a larger one for debugging apps), the app-debugging tools,
# the JDK, baksmali, the DroidBench APKs, and the dex-reader and jvm-reader
# packages and unit tests.
{
  pkgs,
  naersk-lib,
  workspaceVersion,
}:
let
  # nixpkgs ships a wrong hash for the Darwin platform-tools zip, which both
  # SDKs below fetch; fix it for them. The URL never matches on Linux, so there
  # this is a no-op.
  androidenv =
    (pkgs.extend (
      final: prev: {
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
      }
    )).androidenv;

  # --- Nightly regression suite dependencies -------------------------
  # These are only forced when building `checks.<system>.regression`.

  # The build-tools both SDKs carry, pinned because the DEX tests invoke `dx`,
  # which was removed in build-tools 31+.
  buildToolsVersion = "30.0.2";
  # An SDK's build-tools directory, where `dx` and the other build tools are;
  # it is not on PATH by default.
  buildToolsDir = sdk: "${sdk.androidsdk}/libexec/android-sdk/build-tools/${buildToolsVersion}";

  # The platform both SDKs carry, the one the regression suite builds against.
  platformVersion = "30";

  # Android SDK the regression suite builds with.
  androidSdk = androidenv.composeAndroidPackages {
    buildToolsVersions = [ buildToolsVersion ];
    platformVersions = [ platformVersion ];
    includeEmulator = false;
    includeNDK = false;
  };
  jdk = pkgs.temurin-bin-17;

  # --- Android app debugging (dev shell only) --------------------------
  # A superset of `androidSdk` for poking at running apps: platform-tools
  # (adb), the cmdline-tools (sdkmanager/avdmanager), the emulator with a
  # host-native Google APIs system image (a userdebug-style image, so
  # `adb root` works and frida-server can run), and the NDK (lldb,
  # lldb-server, simpleperf). It is its own composition so the
  # regression check does not pay for the emulator, image and NDK.
  #
  # The SDK is read-only in the store, so `sdkmanager --install` cannot
  # add to it; change this list instead. AVDs live in ~/.android/avd:
  #   avdmanager create avd -n dbg -k "$ANDROID_DEBUG_SYSTEM_IMAGE"
  #   emulator -avd dbg -no-window -no-audio &
  #
  # Frida works on any app on this image after `adb root`. JDWP (jdb) does
  # not: ART only loads the agent into an app built debuggable, so a
  # release APK has to be repacked with `apktool`, its <application>
  # given android:debuggable="true", then `zipalign -p` (page-aligning
  # its native libs) and `apksigner` with any key.
  androidDebugAbi = if pkgs.stdenv.hostPlatform.isAarch64 then "arm64-v8a" else "x86_64";
  # The platform of the debugging SDK's system image, alongside `platformVersion`.
  debugPlatformVersion = "34";
  # The system image's sdkmanager name, which `avdmanager create avd -k` takes.
  androidDebugSystemImage = "system-images;android-${debugPlatformVersion};google_apis;${androidDebugAbi}";
  androidDebugSdk = androidenv.composeAndroidPackages {
    buildToolsVersions = [ buildToolsVersion ];
    platformVersions = [
      platformVersion
      debugPlatformVersion
    ];
    includeEmulator = true;
    includeSystemImages = true;
    systemImageTypes = [ "google_apis" ];
    abiVersions = [ androidDebugAbi ];
    includeNDK = true;
  };

  # Native (JNI) debugging pairs the NDK's device-side lldb-server with
  # a host lldb. The NDK's own host lldb does not start from the store:
  # it wants a libpython3.11.dylib the NDK package leaves out. So the
  # host side is nixpkgs' lldb, under its own name because it carries an
  # Android-only setting: lldb 21 segfaults attaching to an Android
  # process unless modules load one at a time, hence the -O.
  #   adb push $ANDROID_LLDB_SERVER /data/local/tmp/ && adb shell chmod 755 /data/local/tmp/lldb-server
  #   adb shell '/data/local/tmp/lldb-server platform --server --listen unix-abstract:///data/local/tmp/dbg.sock &'
  #   android-lldb -o 'platform select remote-android' \
  #     -o 'platform connect unix-abstract-connect:///data/local/tmp/dbg.sock' -o 'process attach -p <pid>'
  androidLldbArch = if androidDebugAbi == "arm64-v8a" then "aarch64" else "x86_64";
  androidLldb = pkgs.writeShellScriptBin "android-lldb" ''
    exec ${pkgs.lldb}/bin/lldb -O 'settings set target.parallel-module-load false' "$@"
  '';

  # frida-server must match the host's frida version exactly, so fetch the
  # one that goes with `frida-python` rather than leaving it to a manual
  # download. Push it with
  #   adb root && adb push $FRIDA_SERVER /data/local/tmp/frida-server
  #   adb shell 'chmod 755 /data/local/tmp/frida-server && /data/local/tmp/frida-server &'
  fridaVersion = pkgs.python3Packages.frida-python.version;
  fridaServer =
    let
      arch = if androidDebugAbi == "arm64-v8a" then "arm64" else "x86_64";
    in
    pkgs.runCommand "frida-server-${fridaVersion}-android-${arch}"
      {
        src = pkgs.fetchurl {
          url = "https://github.com/frida/frida/releases/download/${fridaVersion}/frida-server-${fridaVersion}-android-${arch}.xz";
          hash = "sha256-CdH62GeyfWlWKnkon0xBLoWGf104q3KHcDbtNeQiMCE=";
        };
        nativeBuildInputs = [ pkgs.xz ];
      }
      ''
        xz -dc "$src" > "$out"
        chmod 755 "$out"
      '';

  # baksmali, the reference smali disassembler, used as ground truth for
  # the dex-reader `dex:baksmali` regression check. nixpkgs has no smali
  # package, so we fetch the pinned 3.0.9 "fat" jar and wrap it so
  # `baksmali` runs it. The jar is fetched, never committed to this repo.
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

  # The readers' source: the workspace root plus the two readers, not the
  # whole tree, so that editing the rest of the repo does not rebuild
  # them. The root manifest still lists every member, which cargo would
  # fail to load, so `preBuild` cuts `members` down to the two readers;
  # naersk runs it in its dependency pass too.
  readersSrc = pkgs.lib.fileset.toSource {
    root = ../.;
    fileset = pkgs.lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../readers/dex-reader
      ../readers/jvm-reader
    ];
  };
  readersPreBuild = ''
    sed -z -i 's|members *= *\[[^]]*\]|members = ["readers/dex-reader", "readers/jvm-reader"]|' Cargo.toml
  '';

  # `dex-reader` is the DEX dumper the linemap step of the DEX regression
  # tests runs. It is an *example* of the dex-reader crate, not a bin, so
  # that a plain `cargo build` -- which is what packages.default runs --
  # leaves it out of the distro. Examples are not built by default, so
  # ask for it by name.
  #
  # Two details of the target selection:
  #
  # - The src is `readersSrc` below, not ../readers/dex-reader. dex-reader
  #   is a workspace member, and a member cannot be built alone: its
  #   `version.workspace = true` needs the root manifest.
  # - `--lib` rides along with `--examples` for naersk's sake. naersk
  #   builds dependencies in a first pass over a stub source tree, which
  #   has no examples/ directory; with `--examples` alone that pass
  #   matches no targets, builds nothing, and the dependency caching it
  #   exists for is lost. `--lib` gives it the crate's real dependencies
  #   to chew on.
  #
  # naersk installs whatever cargo reports as an executable, and an
  # example is one, so this lands at $out/bin/dex-reader.
  dex-reader = naersk-lib.buildPackage {
    src = readersSrc;
    preBuild = readersPreBuild;
    version = workspaceVersion;
    name = "dex-reader";
    release = false;
    cargoBuildOptions =
      x:
      x
      ++ [
        "--package"
        "dex-reader"
        "--lib"
        "--examples"
      ];
  };

  # `jvm-reader` is the same arrangement for the JVM E2E linemap step.
  jvm-reader = naersk-lib.buildPackage {
    src = readersSrc;
    preBuild = readersPreBuild;
    version = workspaceVersion;
    name = "jvm-reader";
    release = false;
    cargoBuildOptions =
      x:
      x
      ++ [
        "--package"
        "jvm-reader"
        "--lib"
        "--examples"
      ];
  };

  # The DroidBench ICC APKs the `android-icc` regression specs name, fetched
  # at the commit the specs pin instead of being committed to this repo.
  # xtask finds them through CTADL_ANDROID_ICC_APKS, which the dev shell
  # and the regression check both set; without it those cases SKIP.
  droidbenchIccApks =
    let
      rev = "a57fa6f42f278591695672f1aa8b37c275139370";
      apk = name: hash: {
        name = "${name}.apk";
        path = pkgs.fetchurl {
          url = "https://raw.githubusercontent.com/secure-software-engineering/DroidBench/${rev}/apk/InterComponentCommunication/${name}.apk";
          inherit hash;
        };
      };
    in
    pkgs.linkFarm "droidbench-icc-apks" [
      (apk "ActivityCommunication2" "sha256-KyJvdF2BdCccHox9k+op/GbzEbeHKGmQ6op/HOrxvcA=")
      (apk "ActivityCommunication5" "sha256-MVtam+x1OU2TUN2PBaJnULM2PlC/jBawvLwPdS05FVw=")
      (apk "ComponentNotInManifest1" "sha256-MHB8889dYdMwcUE6GJIcpkWTWEExEd/6iUna+ArFrHc=")
    ];

in
{
  inherit
    androidSdk
    jdk
    androidDebugSdk
    androidDebugSystemImage
    androidLldbArch
    androidLldb
    fridaServer
    baksmali
    dex-reader
    jvm-reader
    droidbenchIccApks
    ;
  androidSdkBuildTools = buildToolsDir androidSdk;
  androidDebugSdkBuildTools = buildToolsDir androidDebugSdk;
  # dex-reader's pure-Rust unit tests, built offline by naersk in test
  # mode. No external toolchain needed. The integration-style
  # full-parse checks (compiled samples + the real-world APK) are in
  # `xtask regression`.
  #
  # `cargo test --workspace` in the test workflow covers these too. This
  # check is kept because it runs on the nightly's Nix builder with no
  # toolchain assumptions, and because it costs nothing to keep.
  #
  # `--package` is not optional here: `src` is the whole tree, so without
  # it this would test the whole workspace, which is a far larger build
  # than this check wants.
  dex-reader-tests = naersk-lib.buildPackage {
    src = ../.;
    version = workspaceVersion;
    name = "dex-reader-tests";
    mode = "test";
    cargoBuildOptions =
      x:
      x
      ++ [
        "--package"
        "dex-reader"
      ];
    cargoTestOptions =
      opts:
      opts
      ++ [
        "--package"
        "dex-reader"
      ];
  };

  # jvm-reader's unit tests, same arrangement -- and, like dex-reader's,
  # entirely hermetic: no `#[ignore]`, no JDK, no fixture directory.
  # What needs a compiled class is in the `jvm:*` checks in
  # `xtask regression` and in the `SwitchFlow` / `StringSwitchFlow` /
  # `WideParamFlow` / `ShiftFlow` taint cases.
  jvm-reader-tests = naersk-lib.buildPackage {
    src = ../.;
    version = workspaceVersion;
    name = "jvm-reader-tests";
    mode = "test";
    cargoBuildOptions =
      x:
      x
      ++ [
        "--package"
        "jvm-reader"
      ];
    cargoTestOptions =
      opts:
      opts
      ++ [
        "--package"
        "jvm-reader"
      ];
  };
}
