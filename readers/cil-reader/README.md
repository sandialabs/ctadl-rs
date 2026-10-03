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

`cargo xtask regression --frontend cil` (root workspace) runs `cil-dump` and
`tools/CecilDump` (the same dump format on top of Mono.Cecil) over the corpus of 100+ test assemblies:
**114/114 comparable files are byte-identical**. Excluded: `*.winmd` (Cecil
applies WinRT projections).

~175 tests ported from Cecil's read-side suites (ImageRead, Module, Assembly,
Type, Field, Method, Parameter, Property, Event, NestedTypes, MethodBody,
CustomAttributes, SecurityDeclaration, Resolve) are green, comparing against
Cecil's expected values — expected-IL strings verbatim.

## Development

The crates are members of the root ctadl workspace and have no task runner of
their own; everything goes through the root `cargo xtask`. Everyday work needs
only Rust (`nix develop` at the repo root, or any recent toolchain):

```sh
cargo test -p cil-reader                    # unit tests; needs no external inputs
cargo xtask regression --frontend cil       # cil:must-fail, cil:cecil-tests, cil:read, cil:goldens, cil:cecil-compare
cargo dump <assembly> [--sections a,b,c] [--no-il]   # from readers/cil-reader: the cil-dump CLI
```

No test inputs are committed:

- **Cecil** (the sources of truth and the test corpus): the root flake's
  `cecil` input pins it, and the devshell exports it as `$CECIL_SRC`. A
  checkout you clone by hand into `extern/cecil` (gitignored) takes
  precedence.
- **Fixtures** (Cecil's `.il`/`.cs` test sources, compiled): the flake's
  `cil-fixtures` package builds them offline, using a .NET 8 SDK and the
  standalone CoreCLR `ilasm` (fetched by hash). The build is byte-for-byte
  reproducible. The devshell exports the result as `$CIL_READER_FIXTURES`.

Since the ported Cecil test suites read both, they run in `xtask regression`
as `cil:cecil-tests` (sources in `xtask/src/cil/cecil_tests`), not under
`cargo test`; the nightly `regression` flake check supplies both inputs.

`cil:goldens` compares Cecil's own assemblies against the committed CecilDump
output in `tests/expected`; `cil:cecil-compare` runs CecilDump live, head to
head, which also covers what has no golden: the fixtures (build outputs, whose
bytes a golden would pin to one toolchain) and the inputs too large to keep
goldens for. CecilDump
(`tools/CecilDump`) compiles Cecil's sources in directly and needs no NuGet
packages, so the root flake builds it offline as a cached derivation (`nix
build .#cecil-dump`), and the devshell puts it on PATH as `cecil-dump`. The
corpus, golden naming and comparison rules live in `cil_dump::parity`.

Regenerating committed artifacts:

```sh
cargo xtask cil bless [--filter <name>]  # rewrite tests/expected from cecil-dump (after moving the cecil pin)
cargo xtask cil codegen                  # regenerate the opcode table from Cecil's OpCodes.cs
```
