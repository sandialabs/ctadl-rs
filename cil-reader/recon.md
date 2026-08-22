# Recon: Mono.Cecil's reading API

Notes from reading the vendored Cecil checkout at `extern/cecil` (commit `882ca5e`,
just past the 0.11.6 bump). All line numbers below are pinned to that commit and are
relative to `extern/cecil/`.

## Summary

Cecil exposes a **materialized object-graph reader**, not a streaming table/cursor
reader. You open an image and get a mutable tree of `*Definition` objects; the raw
metadata tables and heaps are deliberately not part of the public surface.

## Entry points

```csharp
AssemblyDefinition.ReadAssembly(string fileName | Stream, ReaderParameters?)
ModuleDefinition.ReadModule  (string fileName | Stream, ReaderParameters?)
```

- `Mono.Cecil/AssemblyDefinition.cs:125-140`
- `Mono.Cecil/ModuleDefinition.cs:1107-1145`

Both return `IDisposable`; the file/stream stays open until disposed.

`ReaderParameters` (`Mono.Cecil/ModuleDefinition.cs:29`) carries the knobs:

| Knob | Notes |
| --- | --- |
| `ReadingMode` | `Deferred` (default) or `Immediate`; enum at `ModuleDefinition.cs:24` |
| `InMemory` | copy the image into memory rather than mapping the file |
| `ReadWrite` | open for later `Write()` back to the same target |
| `ReadSymbols`, `SymbolReaderProvider`, `SymbolStream`, `ThrowIfSymbolsAreNotMatching` | PDB/MDB loading |
| `AssemblyResolver`, `MetadataResolver` | how `Resolve()` crosses assemblies |
| `ReflectionImporterProvider`, `MetadataImporterProvider` | write-side import hooks |
| `ApplyWindowsRuntimeProjections` | WinRT type projection |

## Shape of the model

`AssemblyDefinition` -> `MainModule` / `Modules` -> `Types` -> `NestedTypes`,
`Methods`, `Fields`, `Properties`, `Events` -> `MethodBody` -> `Instructions`,
`Variables`, `ExceptionHandlers`.

Members are `Collection<T>`, each paired with a `HasXxx` guard (`HasTypes`,
`HasCustomAttributes`, `HasBody`, ...) so a caller can test without forcing the
collection to materialize.

Laziness is the default: `MethodDefinition.Body` runs the `CodeReader` on first
access (`Mono.Cecil/MethodDefinition.cs:150-165`). Until then only the method
signature has been read. `ModuleDefinition.ImmediateRead()` forces the whole graph.

## Reference vs. definition

`TypeReference` / `MethodReference` / `FieldReference` are what the metadata gives
directly; `*Definition` is the resolved declaration. `.Resolve()` crosses assembly
boundaries via `IMetadataResolver` + `IAssemblyResolver`
(`Mono.Cecil/MetadataResolver.cs:17-26`), with `BaseAssemblyResolver` /
`DefaultAssemblyResolver` as the stock probing implementation.

`Resolve()` returns **null** when the target assembly cannot be found. That is the
main failure mode in analysis tools built on Cecil.

## Module-level queries

`LookupToken(int | MetadataToken)`, `GetType(fullName)`, `GetTypes()`,
`HasTypeReference` / `TryGetTypeReference`, `GetTypeReferences()`,
`GetMemberReferences()`, `GetCustomAttributes()`, plus `EntryPoint`, `Resources`,
`AssemblyReferences`, `ExportedTypes`, `TypeSystem`.

## Symbols

`ReadSymbols` pulls PDB/MDB alongside the image: `PortablePdbReaderProvider`,
`EmbeddedPortablePdbReaderProvider`, `DefaultSymbolReaderProvider`, plus native
pdb/mdb under `symbols/`. That populates `MethodDebugInformation` — `SequencePoint`s,
`ScopeDebugInformation`, `VariableDebugInformation`,
`AsyncMethodBodyDebugInformation`, `SourceLinkDebugInformation`, and arbitrary
`CustomDebugInformation` (all in `Mono.Cecil.Cil/Symbols.cs`).

## Mono.Cecil.Rocks

Separate assembly, reaching in through `InternalsVisibleTo`. Useful readers:
`GetAllTypes()` (flattens nested types), `GetConstructors()`, `GetMethods()`,
`GetEnumUnderlyingType()`, `GetBaseMethod()`, `SimplifyMacros()` /
`OptimizeMacros()`, `GetDocCommentId()`, and `ILParser.Parse(method, IILVisitor)` —
a visitor-style IL walk that decodes operands without building an `Instruction` list
(`rocks/Mono.Cecil.Rocks/ILParser.cs:53`, visitor interface at line 21).

## What is NOT exposed

There is no raw metadata-table or heap API.

- `Mono.Cecil.PE` has **zero** public types (`ImageReader`, `Section`, `ByteBuffer`,
  `BinaryStreamReader` all internal).
- `MetadataReader` is internal (`Mono.Cecil/AssemblyReader.cs:446`).
- `TableHeap`, `Row`, `CodedIndex`, `BlobHeap`, `StringHeap`, and even `ElementType`
  (`Mono.Cecil.Metadata/ElementType.cs:13`) are internal.
- The only public types in `Mono.Cecil.Metadata` are `MetadataToken` and `TokenType`.

Access to the internals is granted only via `InternalsVisibleTo`
(`Mono.Cecil/AssemblyInfo.cs:19-22`): `Mono.Cecil.Tests`, `.Pdb`, `.Mdb`, `.Rocks`.

So for streaming access to `#~` tables, blob signatures, or heap offsets, Cecil is
the wrong layer — that is `System.Reflection.Metadata` territory, or a fork.

## Minimal IL read

```csharp
using Mono.Cecil;
using Mono.Cecil.Cil;

using var module = ModuleDefinition.ReadModule ("MyApp.dll");

var method = module.GetType ("MyApp.Program").Methods.First (m => m.Name == "Main");

foreach (var ins in method.Body.Instructions)
    Console.WriteLine (ins);   // "IL_0007: call System.Void System.Console::WriteLine(System.String)"
```

`Instruction.ToString()` already renders `IL_xxxx: opcode operand`.

Unpacking operands — the concrete type is determined by `ins.OpCode.OperandType`:

```csharp
switch (ins.Operand) {
case MethodReference callee:  /* call/callvirt/newobj */ break;
case FieldReference field:    /* ldfld/stsfld/...     */ break;
case TypeReference type:      /* castclass/box/...    */ break;
case string s:                /* ldstr                */ break;
case Instruction target:      /* br/brtrue/leave      */ break;
case Instruction [] targets:  /* switch               */ break;
case VariableDefinition v:    /* ldloc/stloc          */ break;
case ParameterDefinition p:   /* ldarg/starg          */ break;
}
```

Branch operands are already resolved to the target `Instruction` object — no offset
arithmetic at the call site. Compare with `ins.OpCode == OpCodes.Call` or the
flattened `ins.OpCode.Code == Code.Call`.

### Gotchas

- **Guard the body.** Abstract, extern, and P/Invoke methods have none — check
  `method.HasBody` before touching `.Body`.
- **`module.Types` is top-level only.** Nested types are missed; recurse, or use
  `GetAllTypes()` from Rocks.
- **Source lines need symbols.** `new ReaderParameters { ReadSymbols = true }`, then
  `method.DebugInformation.GetSequencePoint (ins)`
  (`Mono.Cecil.Cil/Symbols.cs:766`); check `sp.IsHidden`. `ReadSymbols = true`
  throws `SymbolsNotFoundException` when no PDB is present — catch it, or set
  `SymbolReaderProvider`, if symbols are optional.

## Reference implementation to crib from

`Test/Mono.Cecil.Tests/Formatter.cs` is Cecil's own IL printer: operand formatting,
locals with debug names, sequence points, and exception handlers in ~150 lines.

## Verification status

The C# snippets above were written against the API as it reads in this tree and were
**not compiled** — there is no `dotnet` on this machine. Everything else is quoted
from the source at commit `882ca5e`.
