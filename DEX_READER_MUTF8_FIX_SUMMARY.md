# dex-reader: modified UTF-8

Finishes the one substantive item `JVM_FRONTEND_FIX_SUMMARY.md` left open:
dex-reader's modified-UTF-8 decoder had the same defect jvm-reader's did
(defect 4 there), in its own copy of the code. It now gets the same treatment —
a code-unit decoder, a `DexString`, structured errors, and a strict/lossy split
on the string table — plus the two surrogate fixtures that were parked in
`jvm-reader/tests/sample-jvm-only/` waiting for it.

## The defect

`decode_mutf8` decoded each one-, two- or three-byte sequence into a `u32` and
then mapped every one of them through `std::char::from_u32`:

```rust
let s: String = chars
    .into_iter()
    .map(|c| std::char::from_u32(c).ok_or(DexError::InvalidUtf8))
    .collect::<DexResult<String>>()?;
```

Modified UTF-8 is a **UTF-16** transport. Each sequence is one UTF-16 code
unit, so a supplementary character arrives as a CESU-8 *pair* of three-byte
sequences — two surrogate code units, neither of which is a Unicode scalar
value and neither of which `char::from_u32` will accept. The blast radius is
therefore not the exotic case it looks like:

- **any** DEX with an emoji, a CJK extension character or any supplementary
  symbol in a string constant failed to parse, with a bare `InvalidUtf8` that
  named neither the string nor the byte;
- so did the legal packed UTF-16 tables generated lexers keep in string
  constants (`smaliFlexLexer` has 25 deliberately unpaired surrogates).

The committed real-world fixture already contained one: `com.noto_54.apk`'s
string #29511 is `U+DFFFD`, encoded as the pair `D83F DFFD`. `dex:apk` passed
anyway, because nothing in the check ever decoded a string — see *A gap the
mutation test found* below.

## The fix

### 1. A code-unit decoder — `parse_utils.rs`

`decode_mutf8 -> DexResult<String>` is replaced by
`decode_modified_utf8_code_units -> DexResult<Vec<u16>>`, which returns the
code units verbatim, surrogates included, paired or not. What to do about them
is a policy decision, and it now lives with the type that holds them rather
than in the byte loop.

New `read_string_data_item` reads one whole `string_data_item` — ULEB128
UTF-16 length, NUL-terminated payload — and returns `(DexString, utf16_len,
next_offset)`.

Deleted: `read_dex_string`, which was dead and wrong. It ran
`core::str::from_utf8` over modified-UTF-8 bytes, so it would have rejected
even the `C0 80` NUL form, and its `utf16_len` cross-check was an empty `if`
with a comment about what it might have done. `StringTable::get` had its own,
correct copy of the same framing; both now go through `read_string_data_item`.

### 2. `DexString` — `types.rs`

The twin of `jvm_reader::JvmString`, for the same reason and with the same
shape:

```rust
pub enum DexString {
    /// Every code unit is a Unicode scalar value (surrogates, if any, paired).
    Utf8(String),
    /// Holds unpaired surrogates; kept as raw UTF-16 code units.
    Utf16(Box<[u16]>),
}
```

with `as_str`, `as_str_or_err`, `into_string`, `to_string_lossy`,
`to_string_replacing`, `code_units` and `len_utf16`. Pairs recombine for free
via `String::from_utf16`, and the common case stays a plain `String` — only an
entry that actually needs code units pays for them.

### 3. Strict and lossy string-table accessors — `parser.rs`

`StringTable` grows three accessors where it had one:

| Accessor | Unpaired surrogates | For |
| --- | --- | --- |
| `get_dex_string` | kept as code units | the raw entry; the other two are this plus a policy |
| `get` | error | type descriptors, method/field names, source files — none may legally hold one |
| `get_lossy` | `U+FFFD` | string constants and diagnostics, which must not fail over UTF-16 data a `str` cannot hold |

Every existing caller wanted `get`'s behavior and keeps it; the two that
resolve *constants* moved to `get_lossy`.

### 4. Errors that say what and where — `error.rs`

`InvalidUtf8` was a unit variant carrying no context at all. It is replaced by
two that carry the string-table index, filled in by `StringTable` once it knows
which entry it is on:

- `MalformedUtf8 { string_index, offset, byte }`
- `UnpairedSurrogate { string_index, index, code_unit }`

Across a real APK's 32,000 strings, "invalid utf-8" is not a diagnosis.

### 5. Rendering constants that have no `char` — `instructions.rs`

`StringIdx::display` built its output from a `String`, so an unpaired surrogate
had nowhere to go. It now renders from code units: scalar values are escaped as
`char::escape_debug` does, and an unpaired surrogate is written `\uXXXX`, which
is how baksmali writes every non-ASCII code unit. Its `<invalid_type>`
placeholder — a copy-paste from `TypeIdx` — is now `<invalid_string>`.

### 6. The IR path — `ctadl-ascent/src/languages/dex/mod.rs`

`const-string` and `const-string/jumbo` use `get_lossy`. The alternative on an
unpaired surrogate is dropping the constant from the IR entirely, which is
worse for a taint analysis than carrying it with `U+FFFD` in the positions that
cannot be represented. This mirrors the jvm frontend, which keeps `U+FFFD` in
the IR and uses `?` only in the `javap`-comparable disassembly.

### 7. Comparing against baksmali in code units — `xtask/src/baksmali.rs`

`normalize_const_string` canonicalized one dialect into the other, and could
not survive a supplementary character in either. baksmali writes UTF-16
(`"\ud83d\ude00"`); dex-reader writes `escape_debug` output (a literal
`😀`). The old normalizer turned that literal into a five-hex-digit
`\u10000` that matches nothing, and truncated `\u{10ffff}` to `\u10ff`.

Both sides are now decoded back to the UTF-16 code-unit sequence they denote
and re-emitted identically, which is the only representation the two
disassemblers actually agree on. `string_literal_code_units` accepts
`\uXXXX` (one code unit, possibly an unpaired surrogate), `\u{XXXXXX}` (one
scalar value, so one or two units), the shared single-character escapes, and
literal characters. Three unit tests hold it, including that it does **not**
erase a genuine one-code-unit disagreement.

### 8. The two surrogate fixtures moved into `tests/sample/`

`jvm-reader/tests/sample-jvm-only/` existed only because dex-reader could not
read what was in it. It is gone, and with it:

- `jvm_only_dir` and the `jvm:sample-jvm-only` Skip in `xtask/src/jvm.rs`;
- the sibling-directory contract that made the *parent* of `--jvm-samples`
  load-bearing, so `flake.nix` passes `${./jvm-reader/tests/sample}` directly
  again instead of importing the parent to keep the sibling reachable.

`PairedOnly.java` and `SurrogateConstants.java` are now compiled by both
frontends, like every other sample.

## Tests

### `dex:utf8-constants`, the twin of `jvm:utf8-constants`

New check in `xtask/src/dex.rs`, over the same two sources compiled to DEX: a
CESU-8 pair recombines to `U+1F600`; unpaired surrogates survive as code units,
read back lossily as `U+FFFD`, do not disturb the type descriptors, and make
the strict `get` accessor error rather than lose a unit.

No taint case can stand in for this, on either frontend — an unpaired surrogate
in a constant is inert data, so a decoder that mangles it changes no flow. And
covering one frontend says nothing about the other: they have separate decoders,
which is exactly how this defect outlived its jvm-reader twin.

### Hermetic unit tests

`dex-reader/src/parse_utils.rs` — twelve, over raw byte sequences: ASCII, the
`C0 80` NUL form, pair recombination (U+10000 and U+1F600), four-byte UTF-8
rejected, unpaired high, unpaired low, a packed table keeping all seven code
units, first-unpaired reporting, truncated sequences, `string_data_item`
framing (two items back to back, so the second cannot bleed into the first),
and an unterminated item.

`dex-reader/src/error.rs` — two, that the new variants name the string index
and the offending code unit.

`xtask/src/baksmali.rs` — three, on the const-string canonicalization.

### A gap the mutation test found

Reintroducing the defect (rejecting any surrogate in `read_string_data_item`)
killed `dex:utf8-constants` and `dex:baksmali` — but **not** `dex:samples` or
`dex:apk`. Those two parse `string_ids`, which is only the offset table;
neither ever decoded a string. A DEX whose entire string table was
undecodable would have passed both.

`parse_dex_fully` now decodes every entry, via `get_dex_string` so that a legal
unpaired surrogate is decoded rather than rejected. That is what turns the
committed real-world APK into coverage of this fix: with the defect
reintroduced, `dex:apk` fails on string #29511.

### Mutation-tested

| Reintroduced | What failed |
| --- | --- |
| `read_string_data_item` rejects any surrogate, as `char::from_u32` did | `dex:samples`, `dex:utf8-constants`, `dex:baksmali`, `dex:apk` |

`dex:baksmali` fails with `const-string v0, <invalid_string>` against
baksmali's `const-string v0, "\ud83d\ude00"`; `dex:apk` fails on
`com.noto_54.apk`'s string #29511.

## Verification

```
cargo test --workspace                                  782 tests, 0 failed
cargo xtask regression --frontend jvm,dex                65 passed, 0 skipped,
                                                          0 failed, 0 xfail
nix build .#checks.aarch64-darwin.dex-reader-tests      ok (no JDK needed)
nix build .#checks.aarch64-darwin.jvm-reader-tests      ok
nix build .#checks.aarch64-darwin.regression            ok (the sandboxed CI run,
                                                          which is what exercises
                                                          the --jvm-samples change)
cargo fmt --all -- --check                              clean
cargo clippy --workspace --all-targets                  no new warnings
```

The two clippy warnings that remain are pre-existing and in files this change
does not touch: `items after a test module` in `jvm-reader/src/flow.rs` and
`wrong_self_convention` in `rustc_graphviz/src/tests.rs`.

The 65-case regression run is the unfiltered one. Note that
`--filter :` — the invocation `JVM_FRONTEND_FIX_SUMMARY.md` quotes — selects
only case names containing a colon, which excludes every `Dex:` taint case,
since those keep the bare stem (`SwitchFlow`, not `Dex:SwitchFlow`). All 23 of
them pass here.

## Files changed

```
ctadl-ascent/src/languages/dex/mod.rs      const-string via get_lossy
dex-reader/src/error.rs                    MalformedUtf8 / UnpairedSurrogate + tests
dex-reader/src/instructions.rs             string constants rendered from code units
dex-reader/src/lib.rs                      exports DexString
dex-reader/src/parse_utils.rs              code-unit decoder, string_data_item reader, tests
dex-reader/src/parser.rs                   get_dex_string / get / get_lossy
dex-reader/src/types.rs                    DexString
flake.nix                                  --jvm-samples no longer needs the parent dir
jvm-reader/tests/sample/{PairedOnly,SurrogateConstants}.java   moved from sample-jvm-only/
jvm-reader/tests/sample/README.md          documents them here
jvm-reader/tests/sample-jvm-only/          removed
xtask/src/baksmali.rs                      const-string canonicalized in code units + tests
xtask/src/dex.rs                           dex:utf8-constants; parse_dex_fully decodes strings
xtask/src/jvm.rs                           jvm-only directory plumbing removed
JVM_FRONTEND_FIX_SUMMARY.md                the stale forward-looking notes
```

## Not done

- The plan's Apktool end-to-end fixtures (`BinaryResourceParser`,
  `ResFileDecoder`, ordinary and R8 builds). Still not on this machine, and
  still not needed to reproduce any root cause — the fixtures here do it from
  first principles.
- **The `utf16_size` prefix is read but not enforced.** `read_string_data_item`
  returns it; nothing compares it to the decoded length. The terminator is what
  bounds the scan, and the two agree in every well-formed file, but a DEX where
  they disagree is currently accepted silently. Enforcing it is a separate
  question — dexlib2 tolerates the mismatch, and obfuscators are the reason —
  and it should not ride along with a decoder fix.
