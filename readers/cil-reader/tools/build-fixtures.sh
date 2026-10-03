#!/usr/bin/env bash
# Compile cil-reader's test fixtures from Cecil's .il/.cs test sources
# (PLAN.md §5.2). The root flake's `cilFixtures` derivation runs this; it is
# not committed output.
#
#   build-fixtures.sh <cecil-checkout> <out-dir> <work-dir>
#
# Needs `ilasm` (the standalone CoreCLR one, from the
# runtime.<rid>.Microsoft.NETCore.ILAsm NuGet package) and `dotnet` (a .NET 8
# SDK) on PATH. No network: nothing here references a NuGet package.
#
# - `.il` files: CoreCLR ilasm flag dialect (`-dll -output=<file>`, not the
#   Mono-style `/dll /out:` Cecil's harness uses, CompilationService.cs:319-326).
#   `-DET` makes the MVID and timestamp content-derived.
# - `.cs` files: one generated csproj per source, matching
#   RoslynCompilationService (library, Release, NET_CORE define, assembly named
#   after the source). Cecil's harness references the framework's
#   System.Security.AccessControl, which net8.0's reference pack already
#   carries. Deterministic, with no PDB, so no build path leaks into the bytes.
#
# Output is byte-for-byte reproducible across runs.
set -euo pipefail

cecil=$1
out=$2
work=$3

mkdir -p "$out" "$work"

for il in "$cecil"/Test/Resources/il/*.il; do
    name=$(basename "$il")
    ilasm -nologo -quiet -dll -DET "-output=$out/$name.dll" "$il" >/dev/null
    echo "assembled $name.dll"
done

for cs in "$cecil"/Test/Resources/cs/*.cs; do
    name=$(basename "$cs")
    stem=${name%.cs}
    build=$work/$stem
    rm -rf "$build"
    mkdir -p "$build"
    cat >"$build/$stem.csproj" <<EOF
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
    <AssemblyName>$stem</AssemblyName>
    <DefineConstants>NET_CORE</DefineConstants>
    <Optimize>true</Optimize>
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
    if ! (cd "$build" && dotnet build -c Release --nologo -v q >"$build/build.log" 2>&1); then
        cat "$build/build.log" >&2
        exit 1
    fi
    cp "$build/bin/Release/$stem.dll" "$out/$name.dll"
    echo "compiled $name.dll"
done
