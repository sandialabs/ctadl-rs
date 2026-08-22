# cil-reader

A read-only CIL (ECMA-335) metadata + bytecode reader in Rust, modeled on
Mono.Cecil's reading API. See [PLAN.md](PLAN.md) for the design and
[recon.md](recon.md) for the survey of Cecil's reading surface.

The reader takes a single `&[u8]` for the whole image and returns
subslice-backed values wherever the format allows. Two layers are exposed:

```rust
// Layer 0 — raw: zero-allocation access to streams, heaps and tables.
let image = cil_reader::raw::PeImage::parse(&data)?;
let md = image.metadata()?;
for (rid, row) in md.table::<cil_reader::raw::tables::TypeDefRaw>().rows() {
    let name = md.strings().get(row?.name)?;
}

// Layer 1 — Cecil-shaped model: lazy, borrow-based, `Copy` handles.
let module = cil_reader::Module::read(&data)?;
for ty in module.types()? {
    println!("{}", ty.full_name()?);
    for method in ty.methods()? {
        if method.has_body()? {
            let body = method.body()?;
            println!("{}", body.formatted()?); // Cecil-test-Formatter format
        }
    }
}
```

## Status

Milestones M0–M7 of PLAN.md are complete:

- **PE + metadata**: headers (PE32/PE32+), sections, CLI header, BSJB root,
  all 58 table kinds, heaps, coded indices, per-image row sizing.
- **Type system**: types (incl. nested/generic), members, signatures,
  constants, field RVA data, layouts, semantics, pinvoke, marshal info,
  `FullName` parity with Cecil (generic contexts included).
- **IL**: streaming decoder ([`cil::InstructionDecoder`]) and decoded
  bodies with exception handlers; a formatter byte-compatible with Cecil's
  test `Formatter`.
- **Blobs**: custom-attribute values (typeof/boxed/enum/array forms),
  security declarations (2.0 + legacy XML encodings), the reflection
  type-name grammar (`model::ParsedType`).
- **Resolution** (M7): `AssemblyCache` + `AssemblyLoader`/`DirectoryLoader`,
  `Module::resolve_type/resolve_member` with type forwarders and base-type
  walks.

Not implemented: portable PDB (optional M8), everything write-side (out of
scope by design), WinRT projections (out of scope).

### Cecil parity

`cargo xtask compare` runs `cil-dump` and `tools/CecilDump` (the same dump
format on top of Mono.Cecil) over the corpus of 100+ test assemblies:
**114/114 comparable files are byte-identical**. Excluded: `*.winmd` (Cecil
applies WinRT projections).

~175 tests ported from Cecil's read-side suites (ImageRead, Module, Assembly,
Type, Field, Method, Parameter, Property, Event, NestedTypes, MethodBody,
CustomAttributes, SecurityDeclaration, Resolve) are green, comparing against
Cecil's expected values — expected-IL strings verbatim.

## Development

Everyday work needs only Rust (`nix develop`, or any recent toolchain):

```sh
cargo test                    # ported Cecil test suites
cargo xtask regression        # full-read sweep + committed goldens over the corpus
cargo run -p cil-dump -- <assembly> [--sections a,b,c] [--no-il]
```

Vendored Cecil (`extern/cecil`, gitignored) provides the sources of truth and
test resources. The commit is pinned in `flake.lock`, not by hand: the
devshell exports `$CECIL_SRC` (a read-only store path) and `cargo xtask
vendor` copies it into place — a copy rather than a symlink because
`dotnet build` writes `obj/` next to `Mono.Cecil.csproj`. Every other xtask
command runs it for you; `cargo test` needs it done first. A checkout you
cloned yourself is detected and left alone.

Commands that regenerate committed artifacts need the .NET SDK from the
devshell:

```sh
cargo xtask fixtures          # compile Cecil's .il/.cs test fixtures (ilasm + dotnet)
cargo xtask compare [filter]  # cil-dump vs CecilDump, per-file diff
cargo xtask bless             # regenerate tests/expected goldens from CecilDump
cargo xtask codegen           # regenerate the opcode table from Cecil's OpCodes.cs
```
