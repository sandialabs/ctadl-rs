# The dex/Android/JVM toolchain: the Android SDKs (the one the regression suite
# builds with, and a larger one for debugging apps), the app-debugging tools,
# the JDK, baksmali, smali, ASM, and the dex-reader and jvm-reader unit tests.
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

  # --- Regression suite dependencies -----------------------------------

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

  # smali, the assembler from the same 3.0.9 release, for building .dex
  # inputs by hand: opcodes that javac + dx never emit, such as
  # invoke-polymorphic and invoke-custom, which `dex:dexlib2-decode` and
  # `dex:opcodes-replay` assemble. Fetched like baksmali above, never committed.
  smali =
    let
      jar = pkgs.fetchurl {
        url = "https://github.com/baksmali/smali/releases/download/3.0.9/smali-3.0.9-fat.jar";
        hash = "sha256-z2L8JWRqfs27CfHGt2Prx+nfYJKS2Ws7vgP7o6Qs2eQ=";
      };
    in
    pkgs.writeShellScriptBin "smali" ''
      exec ${jdk}/bin/java -jar ${jar} "$@"
    '';

  # ASM, the bytecode library, for the jvm-reader checks that need an
  # independent model of JVM bytecode: its analyzer is the oracle the
  # `jvm:asm-flow` check compares jvm-reader's dataflow with, and its
  # class writer builds bytecode javac never emits. nixpkgs packages no
  # ASM jars, so fetch the pinned release from Maven Central.
  # `asm-classpath` prints the classpath, which is how xtask finds them.
  asmClasspath =
    let
      jar =
        name: hash:
        pkgs.fetchurl {
          url = "https://repo1.maven.org/maven2/org/ow2/asm/${name}/9.8/${name}-9.8.jar";
          inherit hash;
        };
      jars = [
        (jar "asm" "sha256-h26raoPa7K1cpn65/KuwY8l7WuuM8fynqYns3hdSIFE=")
        (jar "asm-tree" "sha256-FLeIDLfIXu0QHicQQy/D/7gydVMqaolNxMQJXUmtWfE=")
        (jar "asm-analysis" "sha256-5kBzL7zTxicZJaUE8SXjg4Roj037v5LIYi387g0J7bk=")
        (jar "asm-util" "sha256-i6BGDsso/Q4pgOXz7zQzpROkV7wHf4GlO9x1tYegjRU=")
      ];
    in
    pkgs.writeShellScriptBin "asm-classpath" ''
      echo ${pkgs.lib.concatStringsSep ":" jars}
    '';

  # The pure-Rust unit tests of the reader crate `name`, built offline by
  # naersk in test mode.
  #
  # `cargo test --workspace` in the test workflow covers these too. These
  # checks are kept because they run on the nightly's Nix builder with no
  # toolchain assumptions, and because they cost nothing to keep.
  #
  # `--package` is not optional here: `src` is the whole tree, so without
  # it this would test the whole workspace, which is a far larger build
  # than these checks want.
  readerTests =
    name:
    naersk-lib.buildPackage {
      src = ../.;
      version = workspaceVersion;
      name = "${name}-tests";
      mode = "test";
      cargoBuildOptions =
        x:
        x
        ++ [
          "--package"
          name
        ];
      cargoTestOptions =
        opts:
        opts
        ++ [
          "--package"
          name
        ];
    };

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
    smali
    asmClasspath
    ;
  androidSdkBuildTools = buildToolsDir androidSdk;
  androidDebugSdkBuildTools = buildToolsDir androidDebugSdk;
  # dex-reader's tests need no external toolchain. The integration-style
  # full-parse checks (compiled samples + the real-world APK) are in
  # `xtask regression`.
  dex-reader-tests = readerTests "dex-reader";

  # jvm-reader's tests are entirely hermetic too: no `#[ignore]`, no JDK, no
  # fixture directory. What needs a compiled class is in the `jvm:*` checks
  # in `xtask regression` and in the `SwitchFlow` / `StringSwitchFlow` /
  # `WideParamFlow` / `ShiftFlow` taint cases.
  jvm-reader-tests = readerTests "jvm-reader";
}
