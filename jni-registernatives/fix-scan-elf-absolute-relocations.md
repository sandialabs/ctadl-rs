# Resolving absolute symbol relocations in the `RegisterNatives` scan - DO-NOT-MERGE

## The problem

`scan_elf` reads a `JNINativeMethod` slot one of two ways: it takes the addend of a
*relative* dynamic relocation at that address, or, failing that, the word stored in the
file (`ctadl-ascent/src/languages/jni/registry.rs:457`). The module doc states this as a
rule at line 28.

Neither way reaches a slot that carries an **absolute relocation against a symbol**. Those
appear whenever the implementation a table points at is *exported* — an exported function
in a shared object is preemptible, so the linker cannot resolve the pointer at link time
and must emit `R_X86_64_64` / `R_AARCH64_ABS64` against the symbol instead of
`R_*_RELATIVE`. The file leaves the slot zero. `scan_elf` reads that zero, fails the
`is_executable` test at line 482, and discards the entire table.

`nightly/tests/jni/JniRegister.c` is exactly that shape, deliberately: it gives
`stash_impl` and `fetch_impl` external linkage so Ghidra reliably makes functions of them.
Its table therefore recovers zero entries, which is why `Jni:JniRegister` and its two
packaging variants fail. Libraries built the ordinary Android way — with hidden visibility
— get `RELATIVE` relocations and scan correctly today, which is how the feature measured
28/28 on the reference corpus while failing on this case.

## The fix

Extend the slot rule to a third source: for an absolute pointer-width relocation against a
symbol *defined in this object*, the slot's load-time value is `st_value + addend`.

In `scan_elf`, the loop over relocation sections currently keeps only relative entries:

```rust
if let Ok(Some((relas, _))) = section.rela(endian, data) {
    for rela in relas {
        if !is_relative(machine, rela.r_type(endian, false)) { continue; }
        relative.insert(rela.r_offset(endian).into(), rela.r_addend(endian).into() as u64);
    }
}
```

`rela` already returns the linked symbol table's `SectionIndex` — the discarded second
element. Use it:

1. Resolve the symbol table once per relocation section with
   `sections.symbol_table_by_index(endian, data, link)`. On failure, skip the section
   rather than abandoning the scan; every failure in this module is a quiet no-op.
2. Add a companion to `is_relative` — call it `is_absolute_pointer` — matching the
   machine's pointer-width absolute type: `R_X86_64_64`, `R_AARCH64_ABS64`, `R_ARM_ABS32`,
   `R_386_32`.
3. For such a relocation, look the symbol up by `rela.r_sym(endian, false)` and take
   `sym.st_value(endian) + r_addend`. Require `sym.st_shndx(endian) != SHN_UNDEF`: an
   undefined symbol is resolved by some other library at load time, and `st_value` is zero,
   which would reintroduce the bug it fixes.
4. Write the result into the same `relative` map the relative entries populate. Rename the
   map to something like `resolved`, since it is no longer only relative addends.

Relative entries must keep priority where both somehow apply — they are the linker's final
answer.

Needed imports: `object::read::elf::Sym` and `object::SymbolIndex`.

## Deliberately out of scope

`SHT_REL` sections (32-bit `.rel.dyn`) hold the addend in place rather than in the
relocation. A symbol-based `R_386_32` or `R_ARM_ABS32` there would need `st_value` plus the
in-place word. No corpus library needs it, and the in-place fallback already covers the
relative case, so leave it alone unless evidence turns up.

## Verifying

The case that proves the fix is the one that fails now:

```sh
nix develop .#regression -c cargo xtask regression --frontend jni
```

All fourteen cases must pass, `Jni:JniRegister`, `+apk` and `+split-apks` among them. This
was confirmed by proxy before the fix was written: adding `-fvisibility=hidden` to the
fixture's compile flips the two `fnPtr` relocations to `R_X86_64_RELATIVE` and all fourteen
pass unchanged. That flag is *not* the fix — it hides the gap — but it isolates the
relocation form as the sole cause.

Add unit tests beside the existing ones in `registry/tests.rs`. Its `Elf64` builder already
writes a `.text`, a `.rodata`, a `.data.rel.ro` and an optional `SHT_RELA` section byte by
byte, and `relocated()` is the closest existing case. It ships **no symbol table**, so this
needs one: a `.dynsym` section, and an `sh_link` on the relocation section pointing at it.
Then:

- A `fnPtr` slot carrying `R_AARCH64_ABS64` against a symbol defined at `TEXT_ADDR`, with
  the slot zero in the file. The entry must be recovered, with `fn_addr == TEXT_ADDR`.
- The same against an *undefined* symbol (`st_shndx == SHN_UNDEF`, `st_value == 0`).
  Nothing may be recovered — this is the guard on step 3, and without it the test above
  would pass for the wrong reason.
- The existing relative-relocation and in-place tests must pass unchanged.

## Why this is worth doing rather than patching the fixture

Any library that exports its native implementations currently yields zero recovered
entries, not a partial result — one unreadable `fnPtr` rejects the whole triple. The
fixture is a faithful instance of that shape, so leaving it as it is turns it into the
regression guard for this gap.
