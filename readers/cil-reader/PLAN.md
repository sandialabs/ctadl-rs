# cil-reader: Implementation Plan

A read-only CIL (ECMA-335) metadata + bytecode reader in Rust, modeled on
Mono.Cecil's reading API (vendored at `extern/cecil`, commit `882ca5e`). See
`recon.md` for the survey of Cecil's public reading surface; this plan adds the
internals inventory, the port architecture, and the evaluation strategy.

All `extern/cecil` file/line references below are pinned to that commit.

## 0. Status (2026-08-21)

**Implemented: M0–M7. Remaining: M8 (portable PDB, optional).**

- `cargo test`: 165 ported Cecil tests green (per-milestone gates below).
- `cargo xtask regression`: 118/118 corpus files sweep clean; committed
  goldens (1.8 MB, `tests/expected/`, since moved to the root
  `xtask/cil-goldens/`) match.
- `cargo xtask compare`: **114/114 comparable corpus files byte-identical**
  to CecilDump (M6 acceptance). Only exclusion: `*.winmd` (Cecil applies
  WinRT projections — a declared non-goal). Five giants are compared live
  but have no committed goldens (dumps too large): `Mono.Android.dll`,
  `System.Private.Xml.dll`, `mscorlib.dll`, `FSharp.Core.dll`, `cecil.dll`.
- All 19 `.il`/`.cs` fixtures compiled and committed via `cargo xtask
  fixtures`; CecilDump builds with the devshell's dotnet SDK.
- CI runs `cargo xtask vendor` (nix fetches Cecil at the commit pinned in
  `flake.lock` and exports `$CECIL_SRC`; extern/cecil is gitignored) and then
  tests + regression + compare. `cargo test` and `cargo xtask regression`
  need no .NET; the compare step does (the devshell's `dotnet-sdk_8`), and is
  the only check that covers the five no-golden giants.

Deviations from the plan as written are marked **[deviation]** inline below.

## 1. Goals and non-goals

**Goals**

- Read PE images containing CLI metadata: headers, streams, heaps, all 38+
  metadata tables (including portable-PDB tables later).
- Cecil-shaped object model for consumers: module → types → methods/fields/
  properties/events → method body → instructions/variables/exception handlers,
  with the same laziness defaults (nothing decoded until touched).
- First-class iteration APIs: metadata tables/rows, type graph, IL instructions.
- Disassemble every instruction form (all ~225 opcodes, every `OperandType`),
  with operands resolved to typed values (method/field/type refs, user strings,
  branch targets, locals, params, call sites).
- Read data values: constants (Constant table), field RVA data, user-string
  heap, blob heap, embedded manifest resources, custom-attribute values
  (parsed, not just blobs).
- Type hierarchy: base type, interface implementations, nested types, generic
  params/constraints; in-module resolution always, cross-assembly `resolve()`
  via a pluggable resolver (later phase).
- Zerocopy techniques throughout: the reader borrows a single `&[u8]` and
  returns subslice-backed values wherever the format allows.

**Non-goals**

- No writing/emit, no import, no mutation (`AssemblyWriter`, `ImageWriter`,
  `Buffers`, `CodeWriter`, `ILProcessor`, `Import`, strong-name signing — all
  out of scope).
- No WinRT projections (`WindowsRuntimeProjections.cs`) — Windows-only tests.
- Native Windows PDB / Mono MDB readers out of scope. Portable PDB is an
  optional late phase.

## 2. What we are porting (sizing)

Read-side essential surface in Cecil, in dependency order (~14–15k lines C#):

| Phase | Cecil source | ~Lines |
| --- | --- | --- |
| PE + metadata root | `Mono.Cecil.PE/{ImageReader,Image,Section,DataDirectory,BinaryStreamReader}.cs`, read half of `ByteBuffer.cs` | 1,200 |
| Heaps, tables, coded indices | `Mono.Cecil.Metadata/*` (minus write-only `Buffers.cs`) | 1,380 |
| Type-system model | `TypeReference/TypeDefinition/TypeSpecification` + 9 spec subtypes, members, enums | 5,500 |
| Table reading + laziness | `Mono.Cecil/AssemblyReader.cs:26-3267`, `MetadataSystem.cs` | 3,650 |
| Signatures/blobs | `AssemblyReader.cs:3269-3901` (`SignatureReader`), `TypeParser.cs` | 1,160 |
| IL bodies | `Mono.Cecil.Cil/{CodeReader (read half),OpCode,OpCodes,Code,Instruction,MethodBody,ExceptionHandler,Variable*}.cs` | 2,500 |
| Optional: cross-assembly resolve | `MetadataResolver.cs`, `BaseAssemblyResolver.cs`, `TypeSystem.cs`, `TypeResolver.cs`, `GenericParameterResolver.cs` | 1,600 |
| Optional: portable PDB | `Mono.Cecil.Cil/{PortablePdb,Symbols,...}.cs`, `AssemblyReader.cs:2810-3267` | 2,600 |

Key Cecil internals to mirror (they are the hard-won part of the format):

- Row-size computation: `ImageReader.ComputeTableInformations`
  (`Mono.Cecil.PE/ImageReader.cs:483-740`) — the ~240-line switch giving each
  table's row size from heap-index sizes + table row counts.
- Three index-sizing mechanisms: heap indices from the `HeapSizes` bits
  (`ImageReader.cs:465-471`); simple table indices (`row count < 65536 ? 2 : 4`,
  `Image.cs:187-190`); coded indices (`CodedIndex.GetSize`,
  `Mono.Cecil.Metadata/Utilities.cs:564-648`, cached per image).
- Coded-index decode: `Utilities.GetMetadataToken` (`Utilities.cs:42-301`).
- List-column ranges: `MetadataReader.ReadListRange`
  (`AssemblyReader.cs:980-1004`) — length derived by peeking the next row's
  column; owner-sorted range maps via `InitializeRanges`
  (`AssemblyReader.cs:1948-1976`); reverse owner lookup by binary search
  (`MetadataSystem.BinaryRangeSearch`, `MetadataSystem.cs:362-380`).
- Signature decoding: `SignatureReader.ReadTypeSignature`
  (`AssemblyReader.cs:3392-3452`) — the ElementType-driven core; custom
  attribute values at `:3508-3721` (incl. undocumented `Boxed=0x51`,
  `Enum=0x55`, `Type=0x50` element types); marshal info at `:3736-3789`.
- Method bodies: tiny/fat header dispatch (`CodeReader.cs:89-130`), operand
  decode switch (`:174-221`), branch fixup second pass (`:243-265`), small/fat
  exception-handler sections (`:298-369`).
- Type-name grammar: `TypeParser.cs` (532 lines) — needed for `typeof(...)`
  arguments inside custom-attribute blobs, not just for the public
  `GetType(name)` API. Easy to under-scope; it's its own work item.

Two Cecil quirks we deliberately design away:

1. Cecil's `MetadataReader` is a single mutable cursor; every reentrant read
   must save/restore `position` and `context`, guarded by a module lock
   (`ModuleDefinition.cs:963-1007`). We use immutable per-call cursors over
   `&[u8]`, which deletes the lock and the restore dance.
2. `OpCode`'s constructor self-registers into static arrays
   (`OpCode.cs:159-162`). We generate a `const` opcode table instead.

Also unlike Cecil (which keeps a live `FileStream` and copies each heap into
its own `byte[]`; only bodies/resources/RVA data stream from the file), we
take one contiguous `&[u8]` for the whole image — strictly simpler, and it
makes every heap/body/resource access a subslice.

## 3. Architecture

### 3.1 Workspace layout

```
Cargo.toml                # [workspace] members = ["crates/*", "xtask"]
crates/cil-reader/        # the library (published crate)
crates/cil-dump/          # CLI: deterministic text dump of an image (used by tests + compare)
xtask/                    # cargo xtask: fixtures, regression, compare, bless, codegen
tools/CecilDump/          # C# console app referencing extern/cecil, same dump format
tests/fixtures/           # our compiled .il/.cs fixture binaries (committed)
tests/expected/           # committed CecilDump outputs for the corpus (goldens; now root xtask/cil-goldens/)
extern/cecil/             # vendored (nix-pinned, gitignored): source of truth + test resources
```

Root `Cargo.toml` becomes a workspace; current `src/lib.rs` placeholder moves
to `crates/cil-reader`.

### 3.2 Dependencies

Keep the core lean:

- **[deviation]** `zerocopy` was dropped: a hand-rolled checked little-endian
  cursor (`raw::Cur`) covers every fixed-layout read with no unsafe and no
  alignment concerns, so the core ended up with **zero mandatory
  dependencies**. `memmap2` remains declared as an optional `mmap` feature
  (unused so far); the core API takes `&[u8]` so callers choose mmap vs
  `Vec<u8>`.
- xtask uses no external crates either (process running and diffing are
  hand-rolled).

### 3.3 Layered API (raw + model)

Cecil hides the raw layer (`recon.md` §"What is NOT exposed"); we expose both,
because "iterate over metadata" is a stated requirement:

**Layer 0 — `cil_reader::raw`** (zerocopy, zero-allocation):

```rust
let image = raw::PeImage::parse(data)?;          // headers, sections, cli header
let md = image.metadata()?;                       // streams located, row counts read
md.strings().get(idx)? -> &str;                   // #Strings, UTF-8 validated on demand
md.blobs().get(idx)? -> &[u8];                    // #Blob window (Cecil GetView analogue)
md.user_strings().get(rid)? -> Utf16Str<'a>;      // #US, UTF-16LE slice + lossy decode
md.guids().get(idx)? -> &Guid;
md.table::<TypeDefRaw>()                          // typed table accessor
  .rows()                                         // ExactSizeIterator over raw rows
md.table::<TypeDefRaw>().row(rid)? -> TypeDefRaw  // O(1): offset + row_size * (rid-1)
```

Row types are *not* zerocopy structs (column widths vary 2/4 bytes per image),
so each table gets a generated accessor that reads columns via per-image
precomputed column offsets — computed once at open, the Rust analogue of
`ComputeTableInformations`. Raw rows hold unresolved indices
(`StringIndex`, `BlobIndex`, `CodedIndex<TypeDefOrRef>`, `TableIndex<Field>`).

**Layer 1 — `cil_reader` model** (Cecil-shaped, lazy, borrow-based):

```rust
let module = Module::read(data)?;                       // Module<'a>, borrows data
for t in module.types() { ... }                         // top-level TypeDef handles
for t in module.all_types() { ... }                     // incl. nested (Rocks' GetAllTypes)
let ty: TypeDef<'_>;
ty.name()?; ty.namespace()?; ty.full_name()?;           // Cecil FullName rules
ty.base_type()? -> Option<TypeDefOrRef>;                // hierarchy
ty.interfaces()?; ty.nested_types()?; ty.generic_params()?;
let m: MethodDef<'_> = ty.methods()?.next().unwrap();
if m.has_body() {
    let body = m.body()?;                               // decoded on first access
    for ins in body.instructions() { println!("{ins}"); }
    body.exception_handlers(); body.variables()?; body.max_stack();
}
module.lookup_token(tok)? -> Entity<'_>;                // LookupToken equivalent
module.get_type("Foo.Bar/Nested")?;                     // TypeParser-backed
```

Handles are `Copy` wrappers `(module: &'a ModuleData, rid: u32)` — no `Rc`
graphs, no cycles. Everything resolves through the module, so the object model
is just views over the raw layer plus memoized side tables.

### 3.4 Laziness

Mirror Cecil's `MetadataSystem` as a `LazyTables` struct on the module: each
precomputed map (nested-type map, interface map, generic-param ranges,
custom-attribute ranges, semantics map, field/method reverse ranges, …) lives
in a `OnceLock`, built on first touch by a single linear pass over its table —
exactly Cecil's `InitializeXxx` methods. Method bodies, signatures, and
custom-attribute values are decoded per call (cheap, borrow-backed) rather
than cached; `OnceLock` caching can be added later where profiling justifies
it. `Module<'a>: Send + Sync` falls out of this design.

`has_*` guards mirror Cecil's `HasXxx` pattern where they avoid work (e.g.
`has_body()` checks RVA != 0 without touching the body; `has_custom_attributes()`
consults the range map only).

### 3.5 Instructions

Two levels, matching the two use cases in Cecil (`Instruction` list vs Rocks'
`ILParser` streaming visitor):

```rust
// Streaming decoder: no allocation, operands raw, branch targets as offsets.
pub struct InstructionDecoder<'a> { ... }             // Iterator<Item = Result<Ins<'a>>>
pub struct Ins<'a> { pub offset: u32, pub opcode: OpCode, pub operand: RawOperand<'a> }
pub enum RawOperand<'a> {
    None, I8(i8), U8(u8), I32(i32), I64(i64), F32(f32), F64(f64),
    Target(u32), Switch(SwitchTargets<'a>),           // absolute offsets, lazily iterated
    Token(MetadataToken), String(UserStringToken),
    Var(u16), Arg(u16), CallSite(StandAloneSigToken),
}

// Body view: decoded Vec<Instruction>, tokens resolvable, Display renders
// Cecil-style "IL_0007: call System.Void System.Console::WriteLine(System.String)".
body.instructions()                                    // &[Instruction]
ins.resolve_operand(&module)? -> Operand<'_>           // MethodRef/FieldRef/TypeRef/... handles
```

The `OpCode` table (name, sizes, `Code`, `FlowControl`, `OperandType`,
`StackBehaviour` push/pop — `OpCodes.cs:15-16` + ~220 statics) is mechanical
data: an xtask `codegen` command parses `extern/cecil/Mono.Cecil.Cil/OpCodes.cs`
once and emits a checked-in `opcodes.rs` (`const ONE_BYTE: [OpCode; 225]`,
`const TWO_BYTE: [OpCode; 31]`), so the table provably matches Cecil's.

### 3.6 Signatures, constants, data

- `SigReader<'a>` over blob windows: field sigs, method sigs (calling
  convention, `has_this`/`explicit_this`, generic arity, sentinel/varargs),
  local var sigs, `TypeSpec` sigs, generic instances, modifiers, pinned,
  function pointers — port of `SignatureReader`.
- Type signatures decode into an owned `TypeSig` tree referencing tokens (not
  handles), so signatures can outlive borrows cheaply; the model layer wraps
  them for `full_name()` rendering.
- Constants: `Constant` table lookup by owner (sorted, binary search) →
  `ConstantValue<'a>` enum (`Bool/Char/I1..R8/String(&'a [u8] as UTF-16)/Null`),
  including Cecil's quirks (null string vs empty, class/object constants).
- Field RVA data: `field.initial_value()? -> &'a [u8]` with size computed from
  the field type (`GetFieldTypeSize`, `AssemblyReader.cs:1398-1443`).
- Custom attributes: full blob parse → constructor args + named field/property
  args, enum + `typeof` + boxed + array values, using `TypeParser` for
  serialized type names.
- Manifest resources: embedded → `&'a [u8]` slice; linked/assembly-ref →
  metadata record only (matches `ModuleTests.cs:124` expectations).
- Security declarations: both the 2.0 attribute encoding and the legacy
  .NET 1.1 XML form (`AssemblyReader.cs:2625-2724`).

### 3.7 Errors

`thiserror`-free hand-rolled `Error` enum; malformed input must never panic
(all slicing through checked helpers; `BadImageFormat`-style variants carrying
offsets). `ModuleTests.cs:249` (text file → `BadImageFormatException`) becomes
a unit test; a `cargo-fuzz` target over `Module::read` + full-dump is a cheap
later addition.

## 4. Phases and milestones

Each milestone has a test gate drawn from the ported Cecil suite (§5).

- ✅ **M0 — Scaffolding.** Workspace conversion, xtask skeleton, opcode
  codegen, nix devshell gains `dotnet-sdk_8` (available for aarch64-darwin),
  CI keeps building via the existing flake. *Done.* **[deviation]** Table row
  accessors are a declarative `tables!` macro in `raw/tables.rs` rather than
  xtask codegen (only the opcode table is generated); CI additionally vendors
  the nix-pinned cecil commit (`cargo xtask vendor`) and runs `cargo test` +
  `cargo xtask regression` + `cargo xtask compare`.
- ✅ **M1 — PE + metadata root + heaps.** MZ/PE/COFF/optional headers (PE32 vs
  PE32+), sections, CLI header, BSJB metadata root, stream headers, heap
  accessors, `HeapSizes`, row counts, row-size computation, compressed
  int/uint. *Gate met:* ported `ImageReadTests` (21 tests, in
  `tests/image_read.rs`) and the `ByteBufferTests` compressed-int sign case
  (unit test in `raw/cursor.rs`).
- ✅ **M2 — Tables + tokens.** All table accessors, coded-index decode, list
  ranges, owner range maps, `MetadataToken`/`TokenType`, module manifest,
  assembly + assembly refs, module refs, exported types, resources. *Gate
  met:* ported `ModuleTests` read subset (15, `tests/module.rs`),
  `AssemblyTests` read subset (5, `tests/assembly.rs`); the write-side and
  environment-bound cases are documented as skipped in the test files.
- ✅ **M3 — Type system + signatures.** TypeDef/TypeRef/TypeSpec, all
  `TypeSpecification` forms, members, nested types, interfaces, class layout,
  generic params + constraints, semantics, pinvoke, marshal, constants, field
  RVA; `full_name()` parity with Cecil; `TypeParser`. *Gate met:* ported
  `TypeTests` (12), `FieldTests` (16), `MethodTests` (7 + body halves in M4),
  `ParameterTests` (9), `PropertyTests`+`EventTests` (6),
  `NestedTypesTests` (3). **[deviation]** `TypeParserTests` and the comparer
  tests construct Cecil objects (write side); the parse grammar is covered by
  unit tests in `model/typeparser.rs` and signature equality by `TypeSig:
  PartialEq` assertions throughout — documented in the test files.
- ✅ **M4 — IL bodies.** Tiny/fat headers, instruction decode for every
  `OperandType`, branch resolution, switch, calli/`InlineSig` call sites,
  small/fat EH sections, locals via StandAloneSig, `lookup_token` for all 7
  token kinds, `this`-parameter sequence rules. Cecil's test `Formatter`
  ported byte-compatibly (`MethodBody::formatted`). *Gate met:* ported
  `MethodBodyTests` read subset + the deferred body-dependent Type/Method
  tests (16, `tests/method_body.rs`), expected-IL strings verbatim.
  `BranchOutsideMethod` is in the gate as planned (CoreCLR ilasm assembles
  `branch-out.il`).
- ✅ **M5 — Blob-heavy features.** Custom-attribute value parsing end-to-end
  (incl. generic attributes with type-argument substitution and the
  0x50/0x51/0x55 element types), security declarations (both encodings),
  marshal shapes. *Gate met:* ported `CustomAttributesTests` read subset (33,
  `tests/custom_attributes.rs`), `SecurityDeclarationTests` (4).
  **[deviation]** Foreign-enum CA arguments take a pluggable `EnumResolver`
  until callers wire an `AssemblyCache` (the `AceFlags` test supplies one);
  mirrored Cecil's exact failure split: in-module resolution misses are
  swallowed into empty values (`ResolutionException`), assembly-resolution
  misses propagate (`AssemblyResolutionException`).
- ✅ **M6 — Regression + side-by-side green.** `cil-dump` covers the full dump
  format; `cargo xtask regression` clean over the whole corpus (118/118);
  `cargo xtask compare` shows zero diffs vs CecilDump on every comparable
  assembly (114/114). Parity bugs the comparison flushed out (now encoded in
  the reader + tests): `ParameterDefinition.Sequence` is 0-based for static
  methods; `GenericInstanceType.IGenericContext.Type` returns `ElementType`
  (member-ref signature contexts); array-parent member refs keep the reading
  context; `PropertyDefinition.Parameters` mirror the accessors; event types
  render in declaring context; Cecil zeroes `FieldRVA` when the field-type
  size is unknown and returns empty data for unmappable RVAs; an
  ELEMENT_TYPE_STRING constant with an empty blob is `""`, not null (blob
  index 0 is the heap's zero-length entry, not an absent blob) — found by
  bringing `Mono.Android.dll` into the comparison.
- ✅ **M7 (optional) — Resolution.** `AssemblyLoader` trait + directory-probing
  `DirectoryLoader`, owning `AssemblyCache` (append-only frozen-map),
  `MetadataResolver` port: `Module::{resolve_type, resolve_type_sig,
  resolve_type_name, resolve_member}` return `Option`, mirroring Cecil's null;
  type forwarders (loops error, as Cecil throws), netmodules, base-type walks,
  overload matching via scope-blind structural signature shapes (`AreSame`
  semantics). *Gate met:* ported `ResolveTests` subset (8,
  `tests/resolve.rs`) against the pinned mdb `mscorlib.dll` fixture, with
  references drawn from corpus IL instead of `typeof` imports (we have no
  import machinery — reading only).
- ⬜ **M8 (optional) — Portable PDB.** `#Pdb` stream, debug tables, sequence
  points, scopes, embedded (deflate) PDBs. *Gate:* `PortablePdbTests` read
  subset, `.line` directives in the formatter. **Not started** — groundwork in
  place: the `#Pdb` stream is located (`raw::Metadata::pdb_data`) and all
  debug-table row types (`DocumentRaw` … `CustomDebugInformationRaw`) exist
  and participate in row-size layout.

## 5. Testing strategy

### 5.1 Ported unit tests

Port the read-relevant fixtures listed per milestone above (~175 tests landed
across 15 test files/suites; 165 green as of 2026-08-21 — the estimate was
~190; the difference is the construction-based tests documented as skipped
per milestone in §4).
Mechanics:

- Tests live in `crates/cil-reader/tests/`, one file per Cecil fixture file,
  same test names, with a comment pointing at the Cecil source
  (`// Test/Mono.Cecil.Tests/FieldTests.cs:123`).
- Cecil runs each `TestModule` lambda in 4 modes (deferred/immediate read +
  2 write passes — `BaseTestFixture.cs:141`); we drop the write passes. Our
  laziness is per-property rather than a module-wide mode, so each test runs
  once; an `all_types().for_each(force_everything)` "immediate" smoke pass over
  the corpus lives in the regression sweep instead.
- Fixture location: a `read_resource` helper resolves
  `<cecil>/Test/Resources/assemblies/<name>`
  (the analogue of `GetAssemblyResourcePath`, `BaseTestFixture.cs:45-63`).
- Expected-IL strings are copied verbatim from the Cecil tests; `assert_code!`
  normalizes exactly like `BaseTestFixture.Normalize` (trim + CRLF→LF only —
  interior tabs are significant).
- Skip list (documented per test): everything Windows-only
  (`WindowsRuntimeProjectionsTests`, `ImageReadTests.WindowsRuntimeComponentAssembly`),
  write-side fixtures wholesale (`ILProcessorTests`, `Import*Tests`,
  `VariableTests`), native pdb/mdb. (`MethodBodyTests.BranchOutsideMethod` is
  *not* skipped: it's Windows-only in Cecil only because Mono's ilasm can't
  assemble it, and our CoreCLR ilasm can — verified, §5.2.)

### 5.2 Fixtures for `TestIL` / `TestCSharp` tests

Cecil compiles `Test/Resources/il/*.il` (10 files, via ilasm) and
`Test/Resources/cs/*.cs` (9 files, via Roslyn) at test time
(`CompilationService.cs:94-195`). We instead compile once and commit the
binaries to `tests/fixtures/` (never modifying `extern/cecil`):

- `cargo xtask fixtures` — assembles the `.il` files with ilasm from the
  `runtime.<rid>.Microsoft.NETCore.ILAsm` NuGet package and compiles the `.cs`
  files with a tiny generated csproj per source (Release, matching Cecil's
  Roslyn options including the `NET_CORE` define; this half needs the nix
  devshell's dotnet SDK). Records source hashes in
  `tests/fixtures/manifest.toml` so the command is idempotent and CI can check
  freshness. **Verified on this machine (2026-08-21):** the ILAsm nupkg
  (10.0.11, osx-arm64) downloads directly from
  `https://www.nuget.org/api/v2/package/runtime.osx-arm64.Microsoft.NETCore.ILAsm`
  (a plain zip — no dotnet needed for the `.il` half), the extracted native
  `ilasm` runs standalone, and all 10 Cecil `.il` fixtures assemble to valid
  CLI images. Note the flag dialect: CoreCLR ilasm wants
  `-nologo -quiet -dll -output=<file>`, not the Mono-style `/dll /out:<file>`
  Cecil's harness uses (`CompilationService.cs:319-326`).
- Rust tests then treat these exactly like `TestModule` inputs — no toolchain
  needed at `cargo test` time, which keeps plain `nix develop; cargo test`
  working with no .NET.

Corlib-sensitive tests (`ThisParameter*` in `MethodBodyTests.cs:232-271`,
parts of `ResolveTests`) read the *running* framework's corlib in Cecil. We
pin instead: use the checked-in
`extern/cecil/symbols/mdb/Test/Resources/assemblies/mscorlib.dll` as the fixed
corlib fixture and adjust expected values once against it.

### 5.3 Regression corpus (`cargo xtask regression`) — implemented

Corpus = every `.dll`/`.exe`/`.netmodule`/`.winmd` under
`extern/cecil/Test/Resources/assemblies/` (106 files incl. mixed-mode
`cppcli.dll`, .NET 1.1 `hello1.exe`, multi-module `mma.exe` + netmodules,
winmds, `text_file.txt` as the must-fail case) plus the sub-project resource
dirs (`rocks/`, `symbols/mdb`, `symbols/pdb` assemblies) plus
`tests/fixtures/`. For each input:

1. **Sweep:** full read — walk every type (nested included), member,
   signature, custom attribute, constant, and decode every method body
   (implemented as an all-sections `cil_dump::dump` under
   `catch_unwind`). Assertion: no error (except the known-bad inputs list),
   no panic. *118/118 pass.*
2. **Dump + golden:** diff against the committed golden in
   `tests/expected/<dirtag>-<name>.dump`. Goldens are *generated by
   CecilDump* (§6), so passing means byte-parity with Cecil.
   **[deviation]** Golden names carry a corpus-directory tag
   (`main-`/`rocks-`/`mdb-`/`pdb-`/`fixtures-`) because several inputs share
   a file name across corpus dirs. Compared live but no committed goldens
   (dumps too large): `Mono.Android.dll`, `System.Private.Xml.dll`,
   `mscorlib.dll`, `FSharp.Core.dll`, `cecil.dll`.
3. `cargo xtask bless` regenerates goldens (requires dotnet; reruns CecilDump).

The runner reports per-file pass/fail with first-divergence context, and exits
nonzero on any failure. `build_nix.yml` runs `cargo xtask regression` (plus
`cargo test`) after cloning the pinned cecil commit; goldens being committed
means those two steps need no .NET. A third step runs `cargo xtask compare`,
which does need .NET — without it the five no-golden giants would go
unchecked in CI, since they have no committed goldens for `regression` to
diff against. (That gap is not hypothetical: the empty-string-constant bug in
§4/M6 sat undetected in `Mono.Android.dll` while it was excluded.)

## 6. Side-by-side comparator

**`tools/CecilDump/`** — a small C# console app, `net8.0`, with a
`ProjectReference` to `extern/cecil/Mono.Cecil.csproj` (netstandard2.0 target
builds fine on the nix dotnet SDK). It prints a deterministic, line-oriented
dump; **the dump format is the contract**, implemented identically by Rust
`cil-dump`:

- Ordered by metadata token (RID order) throughout; no hash-map iteration
  anywhere; invariant-culture / Rust-default numeric formatting matched
  explicitly (floats printed as bit patterns to dodge culture formatting —
  in constants, CA args, and IL operands).
- Sections: `[module]` (kind, runtime version, architecture, attributes,
  entry point, mvid), `[assembly]` + `[assembly-refs]`, `[module-refs]`,
  `[resources]`, `[exported-types]`, `[types]` (per type: flags, base,
  interfaces, layout, generic params + constraints, nested), `[fields]`
  (sig, offset, RVA data as hex, constant, marshal), `[methods]` (sig,
  pinvoke, overrides, semantics), `[properties]`, `[events]`,
  `[custom-attributes]` (parsed args, rendered like Cecil's test assertions),
  `[il]` (per body: max stack, init-locals, locals, Formatter-style
  instruction lines, EH clauses).
- A `--sections` flag selects subsets so mismatches can be bisected, and
  `--no-il` keeps runs fast during early milestones.

Determinism decisions taken during implementation:

- CecilDump reads with an always-failing `IAssemblyResolver`, so both sides
  see the identical no-resolution universe (matching our M7-less default at
  the time; `!unresolved`/empty-args behavior mirrors Cecil's exception split,
  §4 M5).
- **[deviation]** `[custom-attributes]` is restricted to parent kinds Cecil's
  object model can surface (a CA row may legally hang off e.g. a TypeRef,
  which Cecil cannot reach — `MixedNativeCLI.exe` has one) and omits rids
  (Cecil doesn't expose them; the table's required parent-sorted order makes
  the sequence deterministic anyway).
- Marshal info prints in parsed form on both sides (Cecil has no raw-blob
  accessor); EH/switch labels print `IL_END` where Cecil's instruction
  resolution yields null.
- `*.winmd` files are excluded from comparison: Cecil applies WinRT
  projections (renamed types, virtual assembly refs) to Windows metadata,
  and projections are a declared non-goal. They stay in the sweep.

**`cargo xtask compare [filter]`** — builds CecilDump (`dotnet build`,
cached), runs both dumpers over the corpus, diffs, prints per-file first
diff hunks and a summary. This is both the M6 acceptance test (114/114
identical) and the everyday debugging tool; `xtask bless` is just "compare's
Cecil side, saved".

Environment: `flake.nix` devshell adds `dotnet-sdk_8`. Everything .NET
(CecilDump build, fixture compilation, bless) is xtask-orchestrated and only
needed when regenerating committed artifacts — day-to-day `cargo test` /
`cargo xtask regression` are pure Rust.

## 7. Risks / open questions — all resolved

- ~~**`FullName` parity is a hidden dependency of IL formatting.**~~
  **Resolved.** Implemented in M3 (`model/names.rs`) and battle-tested by the
  M4 expected-IL gates and the M6 byte-parity run, which flushed out the last
  generic-context subtleties (see the M6 entry in §4).
- ~~**ilasm on aarch64-darwin.**~~ **Resolved — verified working** (see §5.2):
  the osx-arm64 ILAsm 10.0.11 binary assembles all 10 `.il` fixtures on this
  machine. Bonus: it also assembles `branch-out.il`, which Mono's ilasm
  cannot (`MethodBodyTests.cs:287` gates that test `OnlyOnWindows` for this
  reason) — so `MethodBodyTests.BranchOutsideMethod` came off the skip list
  and into the M4 gate. Same for `ca-iface-impl.il` (`.interfaceimpl`).
- ~~**Fixture fidelity vs running framework.**~~ **Resolved.** The net8-built
  `.cs` fixtures needed only test-subject adjustments, not expected-value
  ones (e.g. `ResolveTests`' `List`1` lives in `System.Collections` on net8,
  so the generic-instance-resolution test uses an in-module generic type);
  each adaptation is commented in the test files.
- ~~**git and binary fixtures.**~~ **Resolved.** `.gitattributes` marks
  `tests/fixtures/**` binary and `tests/expected/**` `-diff`; fixtures +
  goldens total ~2.5 MB after moving the five giant dumps to no-golden
  status (§5.3). No git-lfs needed.
- ~~**Mixed-mode / odd images.**~~ **Resolved.** `cppcli.dll`, `hello1.exe`
  (1.1 metadata), `hello.textonly.x64.exe`, `MixedNativeCLI.exe` and the
  winmds all sweep clean and (winmds aside, §6) compare byte-identical. The
  surprises they surfaced (TypeRef-parented custom attributes, zero-size
  FieldRVA, empty module-ref names) are handled and covered.

New follow-ups (beyond M8):

- A `cargo-fuzz` target over `Module::read` + full dump remains the cheap
  hardening addition (§3.7); the corpus sweep already runs everything under
  `catch_unwind`.
- `AssemblyCache` (M7) could back a default CA `EnumResolver` so foreign-enum
  attribute arguments resolve without a caller-supplied hook.
- `Module::open(path)` behind the `mmap` feature is still unimplemented
  (§3.2).
