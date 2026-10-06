# cil-reader vs Mono.Cecil: known gaps - DO-NOT-MERGE

Audit of `crates/cil-reader` against Cecil 882ca5e (`extern/cecil`), 2026-10-03.
Paths are relative to `crates/cil-reader/src` unless noted; Cecil paths are
relative to `extern/cecil`.

Evidence (repro DLLs, cil-dump vs cecil-dump diffs, probe crates, and the
long-form per-area reports `raw.md`, `il.md`, `sig.md`, `ca.md`, `model.md`) is
in `/Volumes/Shampoo/cil-reader-gaps-2026-10-03/review/`.

This file is not merged, so nothing that is merged may point at it. Fixtures
(`fixtures/*.il`, `tools/GapGen`, the `cilFixtures` derivation) and tests
describe the behavior they check in their own words. They never cite a gap
number (A1, B3, ...) or `gaps.md`.

What matches (verified): all table column layouts, the 14 coded indices, heap
sizing and decoding, PE32/PE32+ headers, the 219-opcode table, tiny and fat
bodies, EH sections, operand decoding, every signature element type,
`FullName` rendering, custom-attribute and security blobs, constants, marshal
defaults, and the generic-parameter, nesting and member-range maps when no Ptr
tables are present.

Sections:
- **A** — wrong results on valid input, or on input that real tools and
  obfuscators produce.
- **B** — crashes, hangs or runaway allocation on hostile input.
- **C** — the port returns `Err` where Cecil degrades: malformed input only;
  this is a policy choice.
- **D** — places where Cecil is quirky or wrong, and the port is arguably
  right.
- **E** — test-coverage gaps.

---

## A. Wrong results on valid or realistic input

**A1. Ptr tables give members the wrong declaring type.** *Fixed:* `ModuleData::member_list_index` (`model/data.rs`) inverts the Ptr table before the range search; the `ptr-*.cs.dll` fixtures in `cil:cecil-compare` cover it.
- **Rust:** `model/data.rs:165-184` (`member_declaring_type`), and the property
  and event equivalents at `model/handles.rs:1083-1093, 1183-1193`.
- **What's wrong:** list ranges index into FieldPtr/MethodPtr/PropertyPtr/EventPtr,
  but the search key is the real Field/Method/Property/Event rid.
- **Cecil:** `DeclaringType` comes from the collection that read the member, and
  types are completed eagerly when Ptr tables exist
  (`Mono.Cecil/AssemblyReader.cs:817-833`).
- **Effect:** wrong `declaring_type`, and therefore wrong member full names,
  generic `!0`/`!!0` names, and MemberRef rendering.
- **Repro:** `model/synth/swap-Methods.cs.dll`.
- **Fix idea:** invert the Ptr table before the range search.

**A2. `MethodBody::formatted()` prints floats incorrectly.** *Fixed:* `format_r4`/`format_r8` (`model/body.rs`) lay out the shortest round-trip digits as .NET's general format does; `gap-formatter.il.dll` and the corpus's floats cover it in `cil:cecil-compare`.
- **Rust:** `model/body.rs:340` widens f32 to f64; `model/body.rs:511-523`
  (`format_float`) has no exponent form and drops the sign of `-0`.
- **Cecil:** `IConvertible.ToString(InvariantCulture)`
  (`Test/Mono.Cecil.Tests/Formatter.cs:138,185-191`).
- **Example:** `ldc.r4 0.1` prints `0.10000000149011612`; 1e-5 prints `0.00001`
  (Cecil: `1E-05`); 1e17 prints all its digits (Cecil: `1E+17`); `-0` prints `0`.
- **Fix idea:** format each width with its own shortest round-trip form, then
  apply .NET's "R" exponent rules.

**A3. Types are nested whenever a NestedClass row exists, ignoring visibility.** *Fixed:* `ModuleData::declaring_type_rid` (`model/data.rs`) requires a Nested* visibility, for names and `declaring_type` alike; `gap-nested-public.dll` covers it. A NestedClass self-loop on a non-nested type no longer recurses; B3 covers nested ones.
- **Rust:** `model/names.rs:67-75` (`typedef_full_name`) and
  `model/handles.rs:267-274` (`TypeDef::declaring_type`).
- **Cecil:** only sets `DeclaringType` when the visibility is Nested*
  (`Mono.Cecil/AssemblyReader.cs:965-966`).
- **Example:** a Public type that has a NestedClass row prints `Outer/Inner`
  instead of `Inner`.
- **Knock-on:** this is also why a NestedClass self-loop crashes the port but
  not Cecil (B3).

**A4. An empty GenericParam name renders as nothing instead of `!n`/`!!n`.** *Fixed:* full names fall back to the position (`model/names.rs`), and `GenericParam::name` returns a `Cow` with the same fallback; `gap-generic-names.dll` covers it.
- **Rust:** `model/names.rs:121-130`, and the generic-param listing in
  `model/handles.rs`.
- **Cecil:** `Mono.Cecil/GenericParameter.cs:104-110`.
- **Example:** obfuscators blank these names. The port prints
  ``G`1<>::M<>(!!0,)``; Cecil prints ``G`1<!0>::M<!!0>(!!0,!0)``.

**A5. Property and event accessors are associated differently.** *Fixed:* `semantic_methods` (`model/handles.rs`) walks the declaring type's methods in order, each by its last MethodSemantics row with an exact flag value; `gap-semantics.il.dll` covers it. An accessor in another type, or combined flags, attach to nothing; Cecil throws on both when it reads that method's semantics, so they are not in the fixture.
- **Rust:** `model/handles.rs:723-734, 1095-1106, 1195-1206`. The first
  MethodSemantics row whose flags overlap wins, and the method can come from any
  type.
- **Cecil:** keeps one semantics row per method (the last wins), switches on the
  exact flag value, only looks at methods of the declaring type, and orders
  `OtherMethods` by method order (`Mono.Cecil/AssemblyReader.cs:1595-1723`).
- **Example:** a getter shared by P1 and P2 shows up on both in the port, and
  only on P2 in Cecil.

**A6. `PropertyDef::full_name` with no getter or setter prints the signature's parameters.** *Fixed:* the new `PropertyDef::parameters` (`model/handles.rs`) mirrors Cecil's `Parameters`, and `full_name` uses it; `gap-semantics.il.dll` covers it.
- **Rust:** `model/handles.rs:1150-1158`.
- **Cecil:** prints `()` (`Mono.Cecil/PropertyDefinition.cs:111-123,196-214`).

**A7. Primitives written as type references in an attribute constructor signature are treated as enums.** *Fixed:* `TypeRef::primitive` (`model/handles.rs`) applies Cecil's mapping, and attribute decoding and `field_type_size` use it; `gap-attributes.il.dll` covers both.
- **Rust:** `model/custom_attributes.rs:352-364`. A signature like
  `valuetype [mscorlib]System.Int32` or `class System.String` fails ("enum
  argument requires assembly resolution").
- **Cecil:** maps a TypeRef in namespace `System` with an assembly scope to its
  primitive type (`Mono.Cecil/MetadataSystem.cs:96-111`).
- **Same gap elsewhere:** `field_type_size` (`model/handles.rs:612-633`) returns
  0 for such fields, so they get no `initial_value`.
- **Who emits this:** ilasm and other non-Roslyn producers.

**A8. Attributes with framework-enum arguments always fail through the public API.** *Fixed:* `values_with` and `value_with` (`model/custom_attributes.rs`) take an `AssemblyCache`, and enum types resolve as `ReadCustomAttributeEnum` resolves them: signature types through `Module::resolve_type`, 0x55 names with `GetMetadataScope`'s scope (`Module::resolve_parsed_reference`), the underlying type in the enum's own module. `AssemblyCache::throwing` models a resolver that throws; `values()` uses one that loads nothing. Both dumps' `[custom-attributes-resolved]` section reads attributes with the fixtures directory's assemblies; `gap-enums.dll`, `gap-enum-lib.dll` and `gap-enum-moved.dll` cover it, as do the corpus's FSharp.Core and mdb images.
- **Rust:** `EnumResolver` (`model/custom_attributes.rs:17-20`) receives only a
  type name, without the AssemblyRef scope or the 0x55 assembly qualifier, so it
  can't pick the right assembly.
- **Not wired:** `values()` and `value()` always pass `None`, and nothing builds
  a resolver from `AssemblyCache`. The "until M7" comment is stale.
- **Also:** `enum_underlying_of_def` drops the resolver when it recurses (`:471`).
- **Example:** `AttributeUsage(AttributeTargets)`, `Debuggable(DebuggingModes)`,
  `EditorBrowsable`, and `SecurityPermission Flags=` all fail as a whole.
  Parity holds only because CecilDump turns resolution off.

**A9. Enum resolution in attributes ignores ExportedType forwarding.** *Fixed:* `enum_underlying_of_token` (`model/custom_attributes.rs`) falls back to the exported types, resolves a ModuleRef scope only when it names this module, and asks the resolver only for assembly-scoped names; `gap-enum-scopes.dll` covers it. A ModuleRef naming a sibling netmodule is an error, where Cecil would load that netmodule from disk.
- **Rust:** `model/custom_attributes.rs:405-464`.
- **Cecil:** falls back to `module.ExportedTypes` (`Mono.Cecil/MetadataResolver.cs:134-160`).
- **Effect:** Rust prints `args=()`; Cecil prints `args=!unresolved`.
- **Also:** a ModuleRef scope is searched in the current module, where Cecil
  looks in the named netmodule. The resolver is also consulted before the
  module-scope check (`:402`).

**A10. A 0x55 enum name qualified with the current assembly matches on simple name only.** *Fixed:* `resolve_parsed_in_module` (`model/custom_attributes.rs`) compares the assembly's full name exactly, as `ParsedType::resolve_in_module` already did; `gap-attributes.il.dll` covers it.
- **Rust:** `model/custom_attributes.rs:524-540`.
- **Cecil:** requires an exact full-name match (`Mono.Cecil/TypeParser.cs:394-403`).
- **Inconsistent:** the port's own `model/typeparser.rs:94-103` compares full
  names.

**A11. `FieldDef::rva()` returns the raw table value.** *Fixed:* `rva()` is `None` where Cecil's RVA is 0, and `field_type_size` sizes in-module generic instances; `gap-attributes.il.dll` and the C++/CLI corpus images cover it. Still open: sizing TypeRef'd valuetypes, which needs an assembly resolver (CecilDump runs without one).
- **Rust:** `model/handles.rs:589-591`.
- **Cecil:** reports 0 when it can't determine the field type's size
  (`Mono.Cecil/AssemblyReader.cs:1328-1345`).
- **Also:** `field_type_size` only sizes TypeDefs in the same module, where Cecil
  resolves TypeRef'd and generic valuetypes too.

**A12. Property signatures decode parameters Cecil never reads.** *Fixed:* `SigReader::property_sig` (`model/sig.rs`) reads the count and the type only, and `PropertySig` drops `params` (use `PropertyDef::parameters`); `gap-property-sigs.dll` covers it.
- **Rust:** `model/sig.rs:407-425`.
- **Cecil:** reads the count, discards it, and reads only the property type
  (`Mono.Cecil/AssemblyReader.cs:1552-1571`).
- **Effect:** junk parameter bytes, which obfuscators emit, make
  `PropertyDef::signature()` fail.

**A13. The custom-marshaler GUID is kept as raw text.** *Fixed:* `Guid::parse` (`raw/heaps.rs`) mirrors `new Guid(string)`, checked against .NET 8 on 85 cases.
- **Rust:** `model/marshal.rs:170-180`; `crates/cil-dump/src/lib.rs:341-345`
  only lowercases it.
- **Cecil:** parses it with `new Guid(...)` (`Mono.Cecil/AssemblyReader.cs:3772-3777`).
- **Effect:** braced `{...}` and undashed "N"-format GUIDs print differently.

**A14. `Module::characteristics()` returns the COFF Characteristics.** *Fixed;* the ported `MetroAssembly` test now covers it.
- **Rust:** `model/module.rs:229-231`.
- **Cecil:** `ModuleDefinition.Characteristics` is DllCharacteristics
  (`Mono.Cecil/ModuleDefinition.cs:605`).

**A15. The HasPublicKey flag is not normalized.** *Fixed:* `with_public_key_flag` (`model/module.rs`) derives it from the key, for the assembly and for references with the flag; `gap-key-no-flag.dll` and `gap-flag-no-key.dll` cover it.
- **Rust:** `model/module.rs:258, 287` report the raw flag.
- **Cecil:** sets `HasPublicKey = !public_key.IsNullOrEmpty()`
  (`Mono.Cecil/AssemblyNameReference.cs:83-91`).

**A16. Field and method resolution walks base types even when the declaring type has no members of that kind.** *Fixed:* `resolve_member` (`model/resolver.rs`) returns `None` first when the new `TypeDef::has_fields`/`has_methods` do not hold. Like Cecil's for a type not yet read, they count the raw range, deleted members (A28) included. `gap-resolve.dll` covers it.
- **Rust:** `model/resolver.rs:407-429, 462-497`.
- **Cecil:** returns null as soon as the declaring type has no fields or methods
  (`Mono.Cecil/MetadataResolver.cs:179-180, 228-229`).

**A17. The resolver matches array types by full name.** *Fixed:* members match on `type_shape` (`model/resolver.rs`), a key equal exactly when Cecil's `AreSame` holds; an array's is its rank. Cecil does not tell SZARRAY from a rank-1 ARRAY either, since both read as an `ArrayType` of `ElementType.Array`: `T[]` and `T[0...]` match. `gap-resolve.dll` covers it.
- **Rust:** `model/resolver.rs:302-304, 441-458`.
- **Cecil:** compares rank only, and uses the element type to tell SZARRAY from
  ARRAY (`Mono.Cecil/MetadataResolver.cs:385-393`).
- **Effect:** the port misses `int32[0...,0...]` vs `int32[,]`, and wrongly
  equates `T[]` with a rank-1 ARRAY that has no bounds.

**A18. The resolver ignores function-pointer calling conventions and HasThis.** *Fixed:* a function pointer's `type_shape` (`model/resolver.rs`) includes both, and its return and parameter types' shapes; `gap-resolve.dll` covers it.
- **Cecil:** compares both (`Mono.Cecil/MetadataResolver.cs:359-383`).

**A19. The resolver doesn't resolve primitive, string or object types, or arrays of them.** *Fixed:* `resolve_primitive` (`model/resolver.rs`) follows Cecil's `TypeSystem`: a core library's own `System` type (or exported type), else `System.<name>` in the first core-library reference, or in `mscorlib` when there is none. The corpus's `mscorlib.dll` covers a core library, and the `primitive_resolves_to_corlib` test (`xtask/src/cil/cecil_tests/resolve.rs`) another module.
- **Rust:** `model/resolver.rs:314-329` (`element_token`). The code comments
  acknowledge this.
- **Cecil:** resolves them to the corlib definitions.

**A20. Nested types are matched by name only.** *Fixed:* `TypeDef::nested_type` (`model/handles.rs`) ports `GetNestedType`, and resolution, `get_type`, `ParsedType::resolve_in_module` and attribute enum lookup use it; `get_type` takes a leading `/` as part of a top-level name. `gap-resolve.dll` covers resolution, and `gap-resolve-lib.dll`'s attributes the enum lookups, where a reflection name `Outer+NsA.E` was unresolved before.
- **Rust:** `model/resolver.rs:346-351`, `model/module.rs:445-453`,
  `model/typeparser.rs:107-115`, `model/custom_attributes.rs:447-449, 555-557`.
- **Cecil:** compares namespace plus name (`Mono.Cecil/TypeDefinition.cs:619-634`).
- **Also:** `get_type` splits on a `/` at position 0, where Cecil requires
  `IndexOf('/') > 0`.

**A21. Duplicate top-level type names resolve to the first match.** *Fixed:* `get_type_by` (`model/module.rs`) takes the last, and attribute enum lookup goes through it; resolving a TypeDef goes by name too, as Cecil's `Resolve(TypeDefinition)` does, so a reference to the first of two resolves to the second. `gap-resolve.dll` and `gap-resolve-lib.dll` cover it.
- **Rust:** `model/module.rs:464-471`.
- **Cecil:** its name cache keeps the last one added
  (`Mono.Cecil/TypeDefinitionCollection.cs:73,91-97`). Obfuscated modules do
  have duplicates.

**A22. There is no exported-type fallback for nested types.** *Fixed:* type resolution (`TypeName` in `model/resolver.rs`) follows Cecil's `Resolve`, `GetType` and `GetTypeDefinition`: each level falls back to the exported type with its own name, and a nested reference resolves its declaring type first. `gap-resolve.dll` covers it. `ExportedType.Resolve`'s reentrancy guard now covers File implementations too, which B5 says were unguarded; no fixture has such a loop.
- **Rust:** `model/resolver.rs:355-360`.
- **Cecil:** scans exported types (`Mono.Cecil/MetadataResolver.cs:134-157`).

**A23. `DirectoryLoader` stops probing at the first file with the right name.** *Fixed:* it passes over a file that does not read as a CLI image (`model/resolver.rs`); the `directory_loader_skips_bad_images` test covers it, and a probe of both dumps over that layout confirmed Cecil's side.
- **Rust:** `model/resolver.rs:38-126`. If that file is a native DLL, the miss is
  cached.
- **Cecil:** catches `BadImageFormatException` and keeps probing
  (`Mono.Cecil/BaseAssemblyResolver.cs:210-225`).

**A24. `DirectoryLoader` accepts netmodules as assemblies.** *Fixed:* `AssemblyCache::assembly` (`model/resolver.rs`) gives `None` for a module without an assembly, and probing stops there, as Cecil's does; the `directory_loader_rejects_netmodules` test covers it, and a probe of both dumps confirmed Cecil's side.
- **Cecil:** `ReadModule(file).Assembly` is null for a netmodule.

**A25. `AssemblyCache` is keyed by simple name, and misses are not cached.** *Fixed:* `AssemblyLoader::load_assembly` and `AssemblyCache::assembly` (`model/resolver.rs`) take the reference's `AssemblyName`, and the cache keys by its full name; the `assembly_cache_keys_by_full_name` test covers it. Not caching a miss matches Cecil, whose `BaseAssemblyResolver` throws on one before `DefaultAssemblyResolver` can cache it. `DirectoryLoader` still probes by simple name, as `SearchDirectory` does, and now also probes `.winmd` for a Windows Runtime reference.
- **Cecil:** keys by full name (`Mono.Cecil/DefaultAssemblyResolver.cs:30-34`).
- **Effect:** two versions with the same simple name collapse into one entry,
  and every miss probes the filesystem again.

**A26. A ModuleRef scope is loaded through the loader.** *Fixed:* `TypeName::assembly_module` (`model/resolver.rs`) looks only in the assembly's modules, as `Assembly.Modules` lists them: none outside an assembly, an error when a module file is missing or none has the name. The corpus's `modb.netmodule` and `gap-enum-scopes.dll` cover it. Still open: a netmodule read through a File has no link to its assembly, so its own ModuleRef scopes resolve to nothing, where Cecil searches the assembly.
- **Rust:** `model/resolver.rs:208-212`.
- **Cecil:** looks only in the assembly's own modules, so a ModuleRef naming
  the current module resolves to itself (`Mono.Cecil/MetadataResolver.cs:117-128`).

**A27. `is_value_type` and `is_enum` return false when the base type is a TypeSpec.** *Fixed:* both compare `TypeDefOrRef::type_of_name` (`model/handles.rs`), the namespace and name Cecil's `IsTypeOf` sees, which a TypeSpec takes from the type it names, a generic instance from its element type; `gap-base-specs.dll` covers it.
- **Rust:** `model/handles.rs:242, 258`.
- **Cecil:** a TypeSpec wrapping a plain class or valuetype TypeRef still counts
  (`Mono.Cecil/AssemblyReader.cs:1198-1208`).

**A28. Edit-and-continue `_Deleted` members are not filtered out.** *Fixed:* `TypeDef::fields`, `methods`, `properties` and `events` (`model/handles.rs`) leave out members whose `is_deleted` holds; `has_*` still count the raw ranges, as Cecil's reader does for a type not yet read. `gap-deleted.dll` covers it; see D9 for where Cecil goes wrong.
- **Cecil:** skips SpecialName members named `_Deleted` (`IsDeleted`,
  `Mono.Cecil/AssemblyReader.cs:1757-1760`).

**A29. Out-of-range `ldarg`/`ldloc` operands print a value.** *Fixed:* `MethodBody::names_arg_or_local` (`model/body.rs`) drops the operand as Cecil's null one is; `gap-formatter.il.dll` covers it.
- **Rust:** `model/body.rs:359-375` prints `ldarg.s 5` or `ldloc.s V_3`.
- **Cecil:** prints no operand (`Mono.Cecil/MethodDefinition.cs:522-549`).
- **Affects:** both `formatted()` and cil-dump.

**A30. Type-name arity parsing doesn't match C# `int.TryParse`.** *Fixed:* `parse_int32` (`model/typeparser.rs`) ports it (checked against .NET 8), and `ParsedType::arity` is an `i32` summed with wrapping, as Cecil's `int`; `gap-attributes.il.dll` covers it. This also removes B10's add-overflow panic.
- **Rust:** `model/typeparser.rs:130-143`.
- **Cecil:** accepts surrounding whitespace and a leading minus sign.
- **Example:** ``A` 1[[System.Int32]]`` gives ``A` 1<System.Int32>`` in Cecil and
  ``A` 1[][]`` in the port.

**A31. A vararg MemberRef with a MethodDef parent resolves `!!n` to the method's declared names.** *Fixed:* `MemberRef::sig_context` (`model/handles.rs`) takes the declaring type's context only, as Cecil reads the signature against a new `MethodReference`; `gap-methoddef-parent.dll` covers it. Only a generic MethodDef shows it, which C# rejects (CS0224) and ilasm 10.0.11 cannot emit a call to (it writes a TypeRef parent and a truncated signature); evidence is in `/Volumes/Shampoo/a31-2026-10-06/`. See D10 for how the same reading changes Cecil's resolution.
- **Rust:** `model/handles.rs:1363-1367`.
- **Cecil:** prints unbound `!!n`. Very rare.

---

## B. Crashes, hangs and runaway allocation

`error.rs` promises that malformed input never panics. Cecil crashes on many
of these too, but in .NET that is a catchable exception or a contained crash.
In Rust it aborts the host process.

**B1. `unreachable!()` panic in attribute decoding.** *Fixed:* `enum_underlying_of_def` (`model/custom_attributes.rs`) checks `IsEnum`, and rejects an object or array underlying type, which Cecil's `ReadPrimitiveValue` throws on; `gap-attributes.il.dll` covers both.
- **Rust:** `model/custom_attributes.rs:658`, reached via `:645`.
- **Cause:** the "enum" is never checked to have base type `System.Enum`, so its
  first instance field is taken as the underlying type, and that can be
  `object` or `T[]`.
- **Cecil:** checks `IsEnum` and throws an ordinary exception
  (`Mono.Cecil/AssemblyReader.cs:3713-3720`).
- **Fix:** add the `IsEnum` check. That also stops the port from silently
  accepting non-enum structs that Cecil rejects.

**B2. Deeply nested signatures overflow the stack.** *Fixed:* `SigReader::type_sig` (`model/sig.rs`) fails past `MAX_TYPE_SIG_DEPTH` = 64 levels, which bounds every walk over a `TypeSig` too; the `sig::tests` unit tests cover each nesting kind. Release builds use 0.8-1.1 KB of stack per level and debug builds 5-6.6 KB, almost all in the decoder; the deepest real signature in the corpus and the .NET 8 framework is 7 levels. Cecil reads past 64 until its own stack overflows, so between the two the port errs where Cecil does not; no fixture covers that, as `cil:cecil-compare` would differ. Measurements are in `/Volumes/Shampoo/b2-depth-2026-10-04/`. cil-dump's `[methods]` used to lose the whole dump when a method's signature failed to decode; that method now prints `params=!error`. A TypeSpec token's signature is read one level below the token, as Cecil reads it in place (`SigReader::below`, `ModuleData::type_spec_sig`), so rendering, member shapes and `resolve_type` count a chain of TypeSpecs toward the same 64 levels; `resolve_type` follows the chain in a loop. No signature in the corpus or the .NET 8 framework names a TypeSpec (`/Volumes/Shampoo/b3-2026-10-04/logs/specprobe.txt`). `malformed::type_spec_chains` (`cil:cecil-tests`) covers 64- and 65-long chains.
- **Rust:** `model/sig.rs:255-331` and `model/names.rs:141-248`, plus the
  recursive `Drop` of the `Box<TypeSig>` chain.
- **Example:** a 10 KB blob of `0x0f` bytes aborts the process.
- **Fix:** a depth limit.

**B3. Cycles in name rendering overflow the stack.** *Fixed:* `typedef_full_name`, `typeref_full_name` (`model/names.rs`), `TypeRef::scope_name` and `ExportedType::full_name`/`scope_name` (`model/handles.rs`) walk their chains in a loop, and a NestedClass or TypeRef chain longer than its table is an error; `ExportedType::declaring_type` errs on a self or forward reference, as Cecil throws, so its chains are acyclic. A self-referential TypeSpec ends at the depth limit (B2). Cecil overflows its stack on the other cycles, so no fixture can cover them: the `malformed` suite of `cil:cecil-tests` patches corpus images in memory, and the evidence repros `nestcycle.dll`, `trefcycle.dll` and `ca/typeref-cycle.dll` now dump in full (`/Volumes/Shampoo/b3-2026-10-04/logs/repro-*.txt`).
- NestedClass self-loops or cycles: `model/names.rs:67-85`. Cecil survives
  these because of A3.
- TypeRef scope cycles: `model/names.rs:87-103` and
  `model/handles.rs:460-481` (`TypeRef::scope_name`).
- Self-referential TypeSpecs: `model/names.rs:54-58`.
- ExportedType self or forward references: `model/handles.rs:1514-1562`. Cecil
  throws instead.

**B4. A TypeRef scope cycle makes the resolver loop forever and use unbounded memory.** *Fixed:* `TypeName::of_type_ref` (`model/resolver.rs`) treats a TypeRef chain longer than its table as a cycle and errs, and attribute enum lookup now resolves through `Module::resolve_type`, so it stops at the same check. `malformed::type_ref_scope_cycles` and `malformed::attribute_enum_scope_cycle` (`cil:cecil-tests`) cover both paths. The evidence repro `trefcycle.dll` now fails at once with a 1.5 MB peak, and a run with the check removed passes 2 GiB (`/Volumes/Shampoo/b4-2026-10-06/logs/`). Cecil overflows its stack here instead.
- **Rust:** `model/resolver.rs:189-199` guards only against direct self-loops.
- **Same loop:** `enum_underlying_of_token` (`model/custom_attributes.rs:410-425`).

**B5. File-forwarding loops in the resolver are unguarded.**
- **Rust:** `model/resolver.rs:392-398`.

**B6. A type that inherits from itself hangs member lookup.** *Reproduced; Cecil hangs too.*
- **Rust:** `model/resolver.rs:414-427, 469-495`.

**B7. Attribute decoding recurses without bound.**
- An enum whose `value__` field has the enum's own type recurses forever
  (`model/custom_attributes.rs:358,471`). Reproduced.
- Long runs of `0x1d`/`0x51` tags recurse once per byte (`:486-488, 613-616`).

**B8. Each MoreSects section recurses once.** *Fixed:* `read_sections` (`model/body.rs`) loops; the `many_sections_read_in_turn` unit test reads 200,000 sections, and the evidence repros `ms_*.dll` and `moresects_deep.dll` now dump as CecilDump does, which overflows its stack at 200,000 (`/Volumes/Shampoo/b8-b12-b13-2026-10-06/logs/`).
- **Rust:** `model/body.rs:185-187`.
- **Effect:** a debug build aborts at about 20k empty sections, i.e. 80 KB of
  input.
- **Fix:** make it a loop; output doesn't change.

**B9. An 8-byte array signature can allocate gigabytes.** *Reproduced: 8 GB.*
- **Rust:** `model/sig.rs:347-357` pushes `rank` dimensions, and rank can be up
  to 0x3FFFFFFF.

**B10. Type-name arity overflows or over-allocates.** *Reproduced.*
- **Rust:** `model/typeparser.rs:130-143, 227-244`.
- ``A`4294967295+B`1`` panics on add overflow in debug builds.
- ``A`2000000000[`` tries to allocate about 256 GB.
- **Fix:** cap the arity at the remaining input length.

**B11. Table row counts are never validated against the `#~` stream.**
- **Rust:** `raw/metadata.rs:100-102` uses `wrapping_mul`, which defeats the
  overflow check, and there is no check that each table ends inside the stream.
- **Effect:** the model sizes allocations from raw row counts
  (`model/data.rs:124, 315`).
- **Fix:** use `checked_mul` and validate each table's end.

**B12. RVA resolution can overflow `u32`.** *Fixed:* `Section::contains_rva` and `Section::file_offset` (`raw/pe.rs`) wrap, as Cecil's unchecked uints do, so a section whose end wraps contains no RVA; the `raw::pe` unit tests cover both.
- **Rust:** `raw/pe.rs:179, 185`.
- **Reachable from:** method RVAs, FieldRVA, and the resources directory.

**B13. EH clause end offsets can overflow `u32`.** *Fixed:* `read_handler` (`model/body.rs`) wraps the sum, as Cecil's int addition does; the `handler_ends_wrap` unit test covers it. The evidence repros `eh_fat_overflow.dll` and `eh_handler_fat_overflow.dll` now print the clause with `IL_END` for the start that is not an instruction, where Cecil fails the body; C12 (c) covers that.
- **Rust:** `model/body.rs:210, 212` (`try_start + try_len`,
  `handler_start + handler_len`).

**B14. Rid-0 and out-of-range handles.** *Reproduced.*
- A NestedClass row with index 0 produces `TypeDef{rid:0}`. Its `fields()` and
  `methods()` then panic in `model/data.rs:148,158` with a subtraction overflow.
- `nested_types()`, `declaring_type()` and `resolve_member_range`
  (`model/handles.rs:386-410`) hand out handles past the end of their table.
- **Cecil:** drops nulls, and returns empty lists when the start is out of
  range.

---

## C. Errors where Cecil degrades (malformed input; pick a policy)

For each of these, Cecil returns an empty, zero or lossy value, or skips the
bad part, where the port returns `Err`. Sometimes that fails the whole image.

**C1. Invalid UTF-8 in `#Strings` is an error.**
- **Rust:** `raw/heaps.rs:286`.
- **Cecil:** substitutes U+FFFD (`Mono.Cecil.Metadata/StringHeap.cs:57`).
- **Fix needs:** a `Cow` return type.

**C2. Invalid UTF-8 in blob strings is an error.** *Reproduced.*
- **Rust:** `model/sig.rs:456-470`.
- **Affects:** attribute strings, `typeof` names, enum names, named-argument
  names, security attribute names, and marshaler strings. One bad string fails
  every argument of that attribute.
- **Possibly realistic:** Roslyn may write lone surrogates such as `"\uD800"`
  as `ED A0 80`. That encoding is unverified.

**C3. A stream header pointing past the end of the file rejects the image.** *Reproduced.*
- **Rust:** `raw/pe.rs:328-331`; this applies to unknown stream names too.
- **Cecil:** truncates (`Mono.Cecil.PE/ImageReader.cs:409-417`).

**C4. A non-UTF-8 stream name or metadata version string rejects the image.** *Reproduced.*
- **Rust:** `raw/pe.rs:316, 418`.
- **Cecil:** decodes them as Latin-1.

**C5. An out-of-range `#GUID` index is an error.** *Reproduced.*
- **Rust:** `raw/heaps.rs:385`.
- **Cecil:** returns a zero GUID. Note that Cecil's own bounds check is buggy.

**C6. An out-of-range `#Strings` index and an overlong `#Blob` length are errors.**
- **Rust:** `raw/heaps.rs:277-283, 305-306`.
- **Cecil:** returns empty values, and its signature readers read across the
  whole heap.

**C7. Undefined opcodes fail the whole method body.** *Reproduced.*
- **Rust:** `cil/decoder.rs:89-102`.
- **Cecil:** decodes the byte as `nop` with a 4-byte branch operand and
  continues. Obfuscators rely on this.

**C8. An operand that runs past `code_size` fails the body.**
- **Cecil:** reads past the end and continues.

**C9. Code that ends exactly at end of file.**
- **Rust:** `model/body.rs:122` uses `>`, so it decodes the code.
- **Cecil:** uses `<=`, so the code is clamped to empty.

**C10. Unresolvable operand tokens print `!error`.** *Reproduced.*
- **Cases:** rid 0, a rid out of range, or an unsupported table.
- **Rust:** `model/body.rs:376-450`. In `formatted()` the whole rendering fails.
- **Cecil:** prints no operand.
- **Also:** `resolve_operand` returns `Some` for a rid past the end of the table.

**C11. The local-variable token is handled more strictly.**
- **Rust:** `model/body.rs:96-104` errors on a token that isn't a signature
  token, or a rid out of range.
- **Cecil:** ignores the token type, and treats an out-of-range rid as having
  no locals.

**C12. Malformed EH clauses.** *Reproduced.*
- (a) Kinds 3, 5, 6 or 7: Rust errors; Cecil labels them `fault`.
- (b) A fat section size ≥ 0x800000: Rust computes a huge count; Cecil reads 0
  handlers.
- (c) A start that isn't on an instruction: Rust prints `IL_END`; Cecil errors.
- (d) An EH section on an empty body: Rust prints `IL_END` labels; Cecil errors.
- (e) Catch token 0 or out of range: Rust prints the raw token; Cecil prints
  `catch -`. Catch token that isn't a type: Rust prints the member; Cecil
  errors.

**C13. Resource data edge cases.** *Reproduced.*
- **Rust:** `model/module.rs:330-363`.
- **No resources directory:** Rust errors (`size=-1`); Cecil returns 0 bytes.
- **Length past end of file:** Rust errors; Cecil returns the truncated bytes.
- **Out-of-range AssemblyRef:** Rust skips the resource; Cecil keeps it with a
  null assembly.
- **Same shape:** `FieldDef::initial_value` (`model/handles.rs:609`) truncates
  the same way.

**C14. Null tokens in interfaces and constraints are errors.**
- **Rust:** `interfaces()` and `constraints()` (`model/handles.rs:307-308, 1271-1272`).
- **Cecil:** yields a null type.

**C15. A TypeRef whose scope is TypeRef rid 0 fails the whole name.**
- **Rust:** `model/names.rs:91-94`.
- **Cecil:** prints the plain name.

**C16. Small attribute and constant cases.**
- An empty (not null) `typeof` name or security attribute type name errors;
  Cecil returns null.
- A null named-argument name errors; Cecil returns null.
- An odd-length legacy XML security blob drops its last byte; Cecil appends
  U+FFFD.
- The Constant type is masked with `& 0xff` (`model/data.rs:261`).
- Duplicate ClassLayout, FieldRVA, FieldLayout or Constant rows keep the last
  one (`model/data.rs:217-261`); Cecil throws.

**C17. Truncated blob strings are errors.**
- **Cecil:** reads past the end into the next blob.
- **Recommendation:** keep the port's behavior; matching Cecil means emulating
  out-of-bounds reads.

**C18. Member list ranges with Ptr tables.**
- **Rust:** `model/data.rs:113-122, 310-314` and `model/handles.rs:899-905` end
  the last range at the Ptr table's length.
- **Cecil:** uses the target table's length (`Mono.Cecil/AssemblyReader.cs:991-992`).
- **Also:** a start beyond the Ptr table is an error in Rust; Cecil falls back
  to direct table reads.

**C19. Small module-level differences.**
- **Param rows:** a sequence number past the parameter count is ignored (Cecil
  throws), and duplicate sequence-0 rows keep the first (Cecil keeps the last).
- **Entry point:** `entry_point` requires a MethodDef token; Cecil uses the rid
  whatever the token type.
- **`has_type_reference(Some(""))`:** Rust requires an empty scope; Cecil
  treats an empty scope as "any".

---

## D. Cecil quirks (the port is arguably right; emulate or document)

**D1. `[*]` in type-name strings.**
- **Rust:** `model/typeparser.rs:206-210` consumes the following `]`.
- **Cecil:** does not (`Mono.Cecil/TypeParser.cs:171-174`), so the rest of the
  name parses wrongly.
- **Example:** ``Dictionary`2[[System.Int32[*], mscorlib],[System.String, mscorlib]]``
  gives ``Dictionary`2<System.Int32[],mscorlib>`` in Cecil.

**D2. MD5 public key token.**
- **Cecil:** uses MD5 when the hash algorithm is 0x8003
  (`Mono.Cecil/AssemblyNameReference.cs:111-127`).
- **Rust:** always uses SHA-1 (`model/module.rs:61-72`), which matches the CLR.

**D3. HeapSizes flag 0x40 and the `#JTD` stream are ignored.**
- **Rust:** `raw/metadata.rs:63-73`.
- **Both readers are wrong:** the CLR reads an extra u32 when 0x40 is set, so
  Cecil and the port both misplace every table by 4 bytes. This is a known
  anti-Cecil trick.

**D4. Formatter labels at offset ≥ 0x10000.**
- **Cecil:** keeps only the low 4 hex digits (`IL_0003`).
- **Rust:** `formatted()` prints `IL_10003`. cil-dump already matches CecilDump.

**D5. Cases where Cecil's Formatter throws and the port prints something.**
- An EH end equal to the code size.
- Filter handlers: the port never prints the filter start.
- Switch targets that aren't instructions.

**D6. Raw-layer nits.**
- An absent heap still uses its large-index flag; Cecil uses 2.
- Stream names are capped at 16 bytes, where ECMA allows 32.
- RVA lookup uses SizeOfRawData, as Cecil does.
- Debug data past end of file comes back empty; Cecil truncates it.
- Section names are decoded as lossy UTF-8; Cecil uses Latin-1.
- `parse_runtime("v1")` returns 1.1; Cecil throws.
- Lone surrogates in `#US` display as U+FFFD.
- `#Pdb` row counts are ignored.
- Some arithmetic can overflow `usize` on 32-bit targets only.

**D7. IL nits.**
- `code_size` is `u32` where Cecil's is `int`, so dumps differ for sizes
  ≥ 0x80000000.
- `RawOperand::Target(u32)` loses the sign of a negative branch target.

**D8. Type-name parser nits.**
- A rank-0 ARRAY renders `T[]`; Cecil throws.
- `Foo[` and a trailing `\` parse quietly; Cecil throws.
- An empty type-name string gives `None`; Cecil returns null.

**D9. A deleted method misreads the rest of its type's methods in Cecil.**
- **Cecil:** `ReadMethod` returns on a deleted method before reading its
  signature and parameter-list columns (`Mono.Cecil/AssemblyReader.cs:1779`),
  so each later method of the type is read from the wrong offset.
- **Rust:** reads each row in place and only leaves the method out.
- **Fixture:** `gap-deleted.dll` puts its deleted method last in its type.

**D10. Cecil does not resolve a vararg MemberRef whose signature has `!!n`.**
- **Cecil:** reading `!!n` against the new `MethodReference` adds generic
  parameters to it (`CheckGenericContext`), so `GetMethod` skips the
  non-generic vararg method on `HasGenericParameters`
  (`Mono.Cecil/MetadataResolver.cs:276-280`). A generic method passing a `T`
  through `__arglist` makes Roslyn emit this, e.g.
  `R::V(System.Int32,...,!!0)`. Cecil also never resolves a call to a generic
  vararg method, whose calling convention `IsVarArg` does not match.
- **Rust:** resolves both to the method.
- **Fixture:** none; `cil:cecil-compare` would differ. Repros are
  `roslyn.dll` and `a31c.dll` in `/Volumes/Shampoo/a31-2026-10-06/`.

---

## E. Test-coverage gaps

**E1. No corpus file has Ptr tables.** That is how A1 went unnoticed. Add a
fixture: `addptr.py` in the evidence directory can generate one.

**E2. cil-dump prints floats as bit patterns, and no ported Cecil test has an
`ldc.r4` or `ldc.r8` expected-IL string.** That is how A2 went unnoticed.
*Fixed:* both dumps print IL float operands as the Formatter renders them,
then as bits.

**E3. cil-dump has no security-declaration section.** Security parsing is
covered only by 4 ported tests.

**E4. Nothing exercises the public API with an enum resolver (A8), or
`FieldDef::rva()` (A11).** *Fixed:* `[custom-attributes-resolved]` and the ported `TypeReferenceEnum` and `BoxedEnumReference` tests cover the first, `gap-attributes.il.dll` the second.
