# The CIL/.NET toolchain: ilasm, the dnlib-based fixture tools, the reference
# runners the `cil:*` checks compare against (CecilDump, FlowRun, ModelRun),
# and cil-reader's compiled fixtures. None of the .NET builds references a
# NuGet package, so each restores with no network, inside the sandbox.
{
  pkgs,
  cecil,
  dnlib,
}:
let
  system = pkgs.stdenv.hostPlatform.system;

  # The standalone CoreCLR IL assembler, which cil-reader's `.il` fixtures
  # are assembled with. It ships only as a NuGet package per runtime
  # identifier -- a plain zip holding one native binary -- so it is
  # fetched by hash rather than restored.
  ilasm =
    let
      version = "10.0.11";
      rid =
        {
          aarch64-darwin = "osx-arm64";
          x86_64-darwin = "osx-x64";
          x86_64-linux = "linux-x64";
          aarch64-linux = "linux-arm64";
        }
        .${system};
      hash =
        {
          osx-arm64 = "sha256-PspYHXxmIqjKPoomWcV8QCwuJJH2l5qi+WRLVoikrNw=";
          osx-x64 = "sha256-y2bQ9Q53gOzHfibUFyPDLZdICY5UdPJz3UL9LbhOWh8=";
          linux-x64 = "sha256-rurF5fJ8rh5M19yPfcFa1TohojXW3QBSNU79CF3/EoE=";
          linux-arm64 = "sha256-MEXawy+UO90M79QL1EqCaM0l2Kq2utDWoE3q/mUPB2k=";
        }
        .${rid};
    in
    pkgs.stdenv.mkDerivation {
      pname = "ilasm";
      inherit version;
      src = pkgs.fetchurl {
        name = "runtime.${rid}.Microsoft.NETCore.ILAsm.${version}.nupkg";
        url = "https://www.nuget.org/api/v2/package/runtime.${rid}.Microsoft.NETCore.ILAsm/${version}";
        inherit hash;
      };
      nativeBuildInputs = [
        pkgs.unzip
      ]
      ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.autoPatchelfHook ];
      buildInputs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
        pkgs.stdenv.cc.cc.lib
      ];
      # On Linux, ilasm dlopens libcrypto for `-DET`, which autoPatchelf
      # cannot see; put OpenSSL on its RPATH.
      runtimeDependencies = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
        (pkgs.lib.getLib pkgs.openssl)
      ];
      unpackPhase = ''
        unzip -q "$src"
      '';
      installPhase = ''
        install -Dm755 runtimes/${rid}/native/ilasm "$out/bin/ilasm"
      '';
    };

  # The quiet, telemetry-free .NET CLI environment every build here runs in.
  dotnetEnv = {
    DOTNET_NOLOGO = "1";
    DOTNET_CLI_TELEMETRY_OPTOUT = "1";
    DOTNET_SKIP_FIRST_TIME_EXPERIENCE = "1";
  };

  # A .NET console project, the directory `src`, built in Release and wrapped
  # as bin/<bin> to run on the .NET 8 runtime. `buildFlags` are extra
  # `dotnet build` arguments; `tag`, if given, is appended to the derivation
  # name.
  dotnetTool =
    {
      bin,
      src,
      buildFlags ? "",
      tag ? null,
    }:
    let
      project = baseNameOf src;
    in
    pkgs.stdenvNoCC.mkDerivation {
      # `name` rather than pname/version: MSBuild reads the environment as
      # properties, and a `version` variable lands as $(Version), which
      # must be a NuGet version string.
      name = if tag == null then bin else "${bin}-${tag}";
      inherit src;
      nativeBuildInputs = [
        pkgs.dotnet-sdk_8
        pkgs.makeWrapper
      ];
      env = dotnetEnv;
      buildPhase = ''
        runHook preBuild
        export HOME="$TMPDIR" DOTNET_CLI_HOME="$TMPDIR" NUGET_PACKAGES="$TMPDIR/nuget"
        dotnet build -c Release --nologo -p:UseAppHost=false ${buildFlags} -o out
        runHook postBuild
      '';
      installPhase = ''
        runHook preInstall
        mkdir -p "$out/lib/${bin}" "$out/bin"
        cp -R out/. "$out/lib/${bin}/"
        makeWrapper ${pkgs.dotnet-runtime_8}/bin/dotnet "$out/bin/${bin}" \
          --add-flags "$out/lib/${bin}/${project}.dll" \
          --set DOTNET_NOLOGO 1 \
          --set DOTNET_CLI_TELEMETRY_OPTOUT 1
        runHook postInstall
      '';
    };

  # A fixture tool in readers/cil-reader/tools/<project> that compiles the
  # pinned dnlib sources in.
  dnlibTool =
    bin: project:
    dotnetTool {
      inherit bin;
      src = ../readers/cil-reader/tools + "/${project}";
      buildFlags = "-p:DnlibSrc=${dnlib}";
      tag = builtins.substring 0 7 dnlib.rev;
    };

  # PtrGen: rewrites a module with dnlib so its member tables go through
  # FieldPtr/MethodPtr/ParamPtr/PropertyPtr/EventPtr, which neither
  # Roslyn nor ilasm emit.
  ptrGen = dnlibTool "ptr-gen" "PtrGen";

  # GapGen: writes small modules with metadata shapes that Roslyn and
  # ilasm do not emit but obfuscators do, each reproducing a divergence
  # from Cecil.
  gapGen = dnlibTool "gap-gen" "GapGen";

  # FlowRun: runs an assembly's methods on .NET with random arguments, the
  # reference that the `cil:exec` check's interpreter over cil-reader's flow
  # layer is compared against.
  flowRun = dotnetTool {
    bin = "flow-run";
    src = ../readers/cil-reader/tools/FlowRun;
  };

  # ModelRun: calls the .NET library methods the default CIL models select,
  # with marked arguments, the reference the `cil:models` check compares the
  # models against.
  modelRun = dotnetTool {
    bin = "model-run";
    src = ../frontends/ctadl-cil/tools/ModelRun;
  };

  # cil-reader's test fixtures: Cecil's .il/.cs test sources, compiled.
  # The tests find them through CIL_READER_FIXTURES. Byte-for-byte
  # reproducible:
  # - `.il`: the standalone CoreCLR ilasm, whose flag dialect is
  #   `-dll -output=<file>` (not the Mono-style `/dll /out:` of Cecil's
  #   harness). `-DET` makes the MVID and timestamp content-derived.
  # - `.cs`: one generated csproj per source, matching Cecil's
  #   RoslynCompilationService (library, Release, NET_CORE, assembly
  #   named after the source). Deterministic, with no PDB, so no build
  #   path leaks into the bytes.
  # - `ptr-<name>.cs.dll`: each `.cs` fixture rewritten by ptrGen, so the
  #   corpus has images with Ptr tables. dnlib's output is deterministic.
  # - `gap-<name>.il.dll` and `gap-<name>.dll`: cil-reader's own fixtures,
  #   repros of divergences from Cecil: the `.il` sources in
  #   readers/cil-reader/fixtures, assembled as Cecil's are, and the
  #   modules gapGen writes.
  # - `<name>.cs.dll` and `<name>-debug.cs.dll`: the `.cs` sources in
  #   readers/cil-reader/fixtures, compiled as Cecil's are and again
  #   unoptimized, since Debug and Release shape the IL differently
  #   (the `cil:exec` check runs both).
  cilFixtures = pkgs.stdenvNoCC.mkDerivation {
    # `name` rather than pname/version: see dotnetTool.
    name = "cil-fixtures-${builtins.substring 0 7 cecil.rev}";
    dontUnpack = true;
    nativeBuildInputs = [
      pkgs.dotnet-sdk_8
      ilasm
      ptrGen
      gapGen
    ];
    env = dotnetEnv // {
      DOTNET_SYSTEM_GLOBALIZATION_INVARIANT = "1";
    };
    buildPhase = ''
      runHook preBuild
      export HOME="$TMPDIR" DOTNET_CLI_HOME="$TMPDIR" NUGET_PACKAGES="$TMPDIR/nuget"
      mkdir -p "$out" work

      for il in ${cecil}/Test/Resources/il/*.il ${../readers/cil-reader/fixtures}/*.il; do
        name=$(basename "$il")
        ilasm -nologo -quiet -dll -DET "-output=$out/$name.dll" "$il" >/dev/null
        echo "assembled $name.dll"
      done

      # compile <source> <assembly name> <Release|Debug>: builds one
      # source into work/<assembly name>/bin/<config>/<assembly name>.dll.
      compile() {
        local cs=$1 project=$2 config=$3 optimize=true
        if [ "$config" = Debug ]; then optimize=false; fi
        local build=work/$project
        mkdir -p "$build"
        cat >"$build/$project.csproj" <<EOF
      <Project Sdk="Microsoft.NET.Sdk">
        <PropertyGroup>
          <TargetFramework>net8.0</TargetFramework>
          <AssemblyName>$project</AssemblyName>
          <DefineConstants>NET_CORE</DefineConstants>
          <Optimize>$optimize</Optimize>
          <Deterministic>true</Deterministic>
          <DebugType>none</DebugType>
          <EnableDefaultCompileItems>false</EnableDefaultCompileItems>
          <GenerateAssemblyInfo>false</GenerateAssemblyInfo>
          <Nullable>disable</Nullable>
          <LangVersion>latest</LangVersion>
          <NoWarn>CS0169;CS0649;CS0067;CS0414;CS0659;CS0661</NoWarn>
          <AppendTargetFrameworkToOutputPath>false</AppendTargetFrameworkToOutputPath>
        </PropertyGroup>
        <ItemGroup>
          <Compile Include="$cs" />
        </ItemGroup>
      </Project>
      EOF
        if ! (cd "$build" && dotnet build -c "$config" --nologo -v q >build.log 2>&1); then
          cat "$build/build.log" >&2
          exit 1
        fi
      }

      for cs in ${cecil}/Test/Resources/cs/*.cs; do
        name=$(basename "$cs")
        stem=''${name%.cs}
        compile "$cs" "$stem" Release
        cp "work/$stem/bin/Release/$stem.dll" "$out/$name.dll"
        echo "compiled $name.dll"
        ptr-gen "$out/$name.dll" "$out/ptr-$name.dll"
        echo "rewrote ptr-$name.dll"
      done

      for cs in ${../readers/cil-reader/fixtures}/*.cs; do
        name=$(basename "$cs")
        stem=''${name%.cs}
        compile "$cs" "$stem" Release
        cp "work/$stem/bin/Release/$stem.dll" "$out/$name.dll"
        echo "compiled $name.dll"
        compile "$cs" "$stem-debug" Debug
        cp "work/$stem-debug/bin/Debug/$stem-debug.dll" "$out/$stem-debug.cs.dll"
        echo "compiled $stem-debug.cs.dll"
      done

      gap-gen "$out"
      runHook postBuild
    '';
    dontInstall = true;
  };

  # CecilDump: Mono.Cecil's rendering of an assembly in the cil-dump text
  # format, the reference that cil-reader's output is compared against. The
  # project compiles the pinned Cecil sources in directly.
  cecilDump = dotnetTool {
    bin = "cecil-dump";
    src = ../readers/cil-reader/tools/CecilDump;
    buildFlags = "-p:CecilSrc=${cecil}";
    tag = builtins.substring 0 7 cecil.rev;
  };

in
{
  inherit
    ptrGen
    gapGen
    flowRun
    modelRun
    cilFixtures
    cecilDump
    ;
}
