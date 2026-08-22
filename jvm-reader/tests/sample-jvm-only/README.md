# jvm-reader-only sample sources

Same arrangement as `../sample/` — committed `.java`, no `.class` — but
compiled **only** for the jvm-reader checks:

- `xtask/src/jvm.rs` picks this directory up as a sibling of the shared sample
  dir, so `cargo xtask regression --frontend jvm` covers it. Being a sibling
  makes the *parent* of `--jvm-samples` load-bearing: point that flag at a
  `.../tests/sample` that still sits next to this directory, not at a lone copy
  of `sample`. When there is no sibling the run reports a `jvm:sample-jvm-only`
  Skip rather than quietly leaving these two out.

  The `jvm:utf8-constants` check is the one that reads these two: it asserts a
  CESU-8 pair recombines and that unpaired surrogates survive as code units.
  There is no taint case for them, and there cannot be — an unpaired surrogate
  in a constant is inert data, so mangling it changes no flow.

## Why they are held apart

`../sample/` is shared with the dex-reader checks (`xtask/src/dex.rs`), which
compile every source there down to `.dex`. These two carry UTF-16 surrogate
constants, and dex-reader's `decode_mutf8` still has the defect jvm-reader
fixed: it maps each three-byte sequence through `char::from_u32`
independently, so it rejects a well-formed surrogate *pair* as well as an
unpaired surrogate. Adding these to the shared directory would fail
`dex:samples` for a reason that has nothing to do with the sample.

Move them into `../sample/` once `dex-reader/src/parse_utils.rs` grows the same
treatment.

## The samples

- **PairedOnly.java** – a single emoji and nothing else. The class file encodes
  it as a CESU-8 surrogate pair, so this is the *common* case: every class with
  a supplementary character in a literal, not just deliberately packed data.
- **SurrogateConstants.java** – a well-formed pair, a lone high surrogate, a
  lone low surrogate, and a packed table mixing them, in the style of the
  generated `smaliFlexLexer` table that first surfaced this. Unpaired
  surrogates are legal in a class file and are used on purpose as UTF-16 data;
  Rust's `String` cannot hold them, so they survive as code units in
  `JvmString::Utf16` and are only rendered lossily.
