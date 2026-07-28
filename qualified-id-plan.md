# Fix fail-open `where` constraints; add `qualified-id`; drop `unqualified-id` - DO-NOT-MERGE

## Context

The model-generator loader has a class of **fail-open** bugs: a `where` constraint the
loader cannot act on becomes a **no-op**, and because the working set starts as
`UniverseSet::all()` (`ctadl-ascent/src/models/json.rs:664`), a no-op constraint means the
generator matches **every function in the program**. A model meant to mark one method as a
source silently becomes a global source.

Nothing catches this. `CTADL0004` (`query_engine/formatter.rs:129`) only diagnoses
generators that matched *nothing*; there is no "matched everything" diagnostic. It also
contradicts the loader's own policy — an unrecognized `constraint` *discriminator* is
already a hard error, precisely because silent skips masked real bugs (e.g. `any_of`
behaving as AND) — yet a recognized constraint with an unusable *field* still fails open.

This is not hypothetical. `ctadl-ascent/tests/c/xfer.json:24-26` writes
`{"constraint":"signature","name":".*sink.*"}` — `name` where `pattern` was meant. That
generator matches every function in the program today. (The file is orphaned test data, so
nothing currently depends on the wrong behavior.)

The trigger was `unqualified-id`: documented for `signature_match`, implemented only for
`uses_field`. `visit_signature_match_constraint` (`json.rs:700`) honors exactly
`{name, names, parent, parents}`, so a `signature_match` keyed only on `unqualified-id`
matches everything. `signature_match.extends` (schema `:51-55`) is dead in the same way.

Three changes:

1. **Make the fail-open paths hard errors**, and reject unknown fields on the structured
   constraints so this whole class stops being silent.
2. **Add `qualified-id` / `qualified-ids`** — exact whole-string match on a method's
   fully-qualified id, on every frontend. This is the genuinely missing capability: on
   non-Java frontends there is no way to disambiguate two same-named methods, because
   `parent`/`parents` is populated only for the Java VMT (`json.rs:213`). The only lever
   today is that `signature_pattern` incidentally also regexes the fq name
   (`json.rs:233-235`) — undocumented, regex-not-equality, and un-anchorable at the tail.
3. **Drop `unqualified-id`** from `signature_match` and `uses_field`. Per the C++ grammar it
   borrows from, an *unqualified-id* is the bare name (`bar`) and a *qualified-id* is
   `Foo::bar`; the key as specified is a synonym for `name`, which is exactly how
   `uses_field` implements it (`json.rs:1035` appends to the same `wanted` list). Nothing in
   the repo uses it — no model file, fixture, or test — so there is no data migration.

Outcome: a model that cannot be honored fails loudly at load time, and every frontend gets
one uniform, exact lever for naming a specific method.

---

## Change 1 — hard-error the fail-open paths

Five sites in `ctadl-ascent/src/models/json.rs`. Reuse the existing
`JsonModelError::UnexpectedField { index, field_name, message }` and
`MissingField { index, field_name }` (`ctadl-ascent/src/error.rs:8-43`) — **no new error
variant is needed**. Match the existing message convention: lowercase, no trailing period,
`Display` already appends `in model generator at index {index}`.

| Site | Today | Change |
| --- | --- | --- |
| `visit_signature_match_constraint` `:700` | honors `{name,names,parent,parents}`; if none present, no-op → matches all | error if no honored key is present, **and** error on any unrecognized key |
| `visit_signature_constraint` `:792` | `&& let Some(pattern) = …` with no `else` → no-op | `MissingField{field_name:"pattern"}`, matching how `visit_name_constraint` `:955` already handles it |
| `visit_uses_field_constraint` `:1038` | `if wanted.is_empty() { return; }` → no-op | error; also reject unrecognized keys |
| `visit_in_function_constraint` `:826` | silently skipped unless `find: callsites` | error — `in_function` under `find: methods` is meaningless, not harmless |
| `eval_predicate`'s `signature_match` arm `:423` | honors only `{name,names}`; returns `false` otherwise | same key validation. Fail-*closed*, so lower severity, but a typo silently matches nothing |

**Rejecting unknown keys is the important half.** Erroring only on "no honored key" still
lets `{"constraint":"signature_match","name":"x","extends":"Y"}` silently ignore `extends`.
Validating the key set mirrors the schema's `additionalProperties: false` and makes changes
2 and 3 self-enforcing: dropped keys become loud errors automatically.

Regression check (already done): every shipped model file — the jadx and pcode
`default-index.jsonl`, `nightly/tests/**`, and the `signature_match`+`name` generators that
`tests/models_loading.rs:16,36,57` depend on — uses only honored keys and stays valid. The
one casualty is the `xfer.json` typo above; fix it to `"pattern"` as part of this change.

## Change 2 — add `qualified-id` / `qualified-ids`

Semantics: exact, whole-string equality (no regex, no wildcard) against the method's
fully-qualified id. Multiple keys within one `signature_match` AND together, consistent with
today's `name`+`parent` behavior. `qualified-ids` is a list ORed within itself, matching the
`names`/`parents` convention.

**This needs a frontend change for pcode — it is not a pure loader change.** Ghidra's
exporter writes `HFUNC_NAME` from `hfn.getFunction().getName()`
(`pcode-reader/ExportPcode.java:858`) — the **bare** name. A C++ `Foo::bar` arrives as
`bar`, so `NativeSimpleName` is never qualified. The qualified name exists only in the
exporter's function id, `funcID = fn.getName(true) + "@" + fn.getEntryPoint()`
(`ExportPcode.java:1060`), and that string is not retained in `ProgramInfo`. The IR
function name is no substitute: `pcode/mod.rs:315-336` uses the bare name when it is
globally unique, so for a *unique* `Foo::bar` the IR name is just `bar` and the namespace
is lost; when it is not unique the IR name is the address-bearing
`<EXTERNAL>::system@00008d90`, which is unstable across binaries and non-unique per libc
symbol (one binary had three `system` thunks).

Steps:

- **`ctadl-ir/src/mir/call.rs:142-148`** — add a `NativeQualifiedName` newtype beside the
  existing `NativeSimpleName`/`NativeSignature`/`NativeFunction`, and widen
  `VirtualMethodTable::Native`'s `methods` from a 3-tuple to a 4-tuple. Mechanical; the only
  producers are `pcode/mod.rs:378` and the `native_program()` test fixture.
- **`ctadl-ascent/src/languages/pcode/mod.rs:378-384`** — populate it with `func_id` minus
  the trailing `@<entry point>` (split on the last `@`; entry points contain no `@`, and
  `<EXTERNAL>::system@EXTERNAL:00000007` splits correctly). Compute it unconditionally, on
  both branches of the `base_name` selection at `:315-336`, so the value does not depend on
  whether the bare name happened to be unique.
- **`ctadl-ascent/src/models/json.rs:201-250`** — build a fourth index,
  `program_method_qualified_ids`, alongside the three existing ones:
  - Java (`:207-221`): key on the `JavaMethod` fq id, e.g. `Lcom/example/Foo;->bar(I)V`.
    Stable and descriptor-bearing. It is currently only ever a *value*, never a key — this
    is the gap that makes exact fq matching impossible on jvm/dex today.
  - Native (`:222-236`): key on the new qualified name, e.g. `<EXTERNAL>::system`,
    `Foo::bar`. Also keep the fq id as a key so existing verbatim-id models still resolve.
  - Fallback (`:238-247`): key on the IR function name, as that branch already does.
- **`visit_signature_match_constraint` `:700`** — handle the new keys, following the
  existing `has_names` / `has_parents` shape: collect the requested ids, look each up, and
  `intersect_with` the result via `target_set_mut(n)`.

## Change 3 — drop `unqualified-id`

- `json.rs:1018` (comment) and `json.rs:1035-1037` (the only code) — remove from
  `uses_field`. Behavior-preserving: it is a pure alias for `name`.
- `ctadl-ascent/src/models/ctadl-model-generator.schema.json:56-59` and `:218-221` — remove
  both properties; add `qualified-id` / `qualified-ids` to the `signature_match` branch.
- While there: remove the unimplemented `signature_match.extends` (schema `:51-55`). The
  standalone `{"constraint":"extends","inner":…}` constraint is the real, implemented one
  (`json.rs:1117`) and is unaffected.
- `docs/model-generators.md` — `:149` (signature_match field list), `:151` (delete the bogus
  standalone `unqualified-id` table row), `:159` (uses_field row). Document `qualified-id`,
  including that it is exact-match, what the id looks like per frontend, and that on pcode it
  is namespace-qualified but **not** address-qualified.
- Optionally advertise `qualified-id` in the `init-model` template
  (`ctadl-ascent/src/main.rs:461-467`), which today shows only `signature_match` + `name`.

The schema is pure documentation — no runtime validation, no codegen (`models/codegen.rs` is
an unrelated Ascent rule for `AsyncTask`), no `include_str!`. It is consumed only by editors
via the `$schema` URL in the `init-model` template (`main.rs:435`). Because each branch sets
`additionalProperties: false`, dropping the keys makes IDEs flag them immediately, and
change 1 makes the loader reject them — a clean two-layer migration signal.

---

## Verification

**Unit tests** — `ctadl-ascent/tests/json_error_handling.rs`, which already has everything
needed: `native_program()` `:262`, `java_program()` `:285`, `matched_functions()` `:314`,
`set()` `:335`, and `assert_unexpected_constraint()` `:454` as the template for a new
`assert_unexpected_field()` helper.

- One hard-error test per site in change 1, following `unknown_constraint_is_hard_error`
  `:485`. Include `{"constraint":"signature","name":"x"}` — the real `xfer.json` typo.
- **A regression test asserting the fail-open bug is gone**: `matched_functions` with a
  now-invalid `where` must *fail to load*, not return all of `{a,b,c}`. This is the assertion
  that would have caught the original bug.
- `qualified-id` positive/negative tests. The VMT is hand-built in these fixtures, so add a
  `native_cpp_program()` with two same-named methods in different namespaces (`Foo::bar`,
  `Baz::bar`, both simple name `bar`) and assert `qualified-id` selects exactly one while
  `name` selects both — the disambiguation this feature exists for. No Ghidra binary needed.
- A `qualified-id` test on `java_program()` against the `JavaMethod` id.

**Suite** — `cargo test -p ctadl-ascent` (covers `models_loading.rs`, whose canonical
`signature_match`+`name` generators must keep loading) and `cargo test --workspace` for the
IR tuple widening.

**End-to-end** — `cargo xtask regression` and `cargo xtask regression --frontend pcode`.
These exercise the bundled `default-index.jsonl` files through a real import→index→query,
confirming the stricter loader still accepts every shipped default model.

**Gap to flag:** there is no C++ or colon-named fixture anywhere in the tree (all
`nightly/tests/c/*.c` are plain C, and no `*.facts`/pcode fixture DBs are checked in), so
`qualified-id` on pcode gets unit coverage against a hand-built VMT but **no** end-to-end
coverage. Confirming it against a real Ghidra-imported C++ binary is a manual step:
`ctadl index <binary>` then `ctadl query --models <model using qualified-id>`, checking the
`fullyQualifiedName` values in the SARIF against the qualified names the model targets.
