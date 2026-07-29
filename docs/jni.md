# The JNI bridge

An Android app's `native` method is a declaration with no body. Its
implementation lives in a shared library, under a name derived from the Java
class and method by the JNI mangling rules. Nothing in either artifact names the
other, so co-indexing them is not by itself enough: taint entering a `native`
method vanishes, and taint produced by the implementation never comes back.

The **JNI bridge** closes that gap. Whenever a Java or Dex artifact is indexed
alongside native code, CTADL joins each `native` method to the `Java_…` function
implementing it and maps the arguments across the JNI ABI, so taint flows in both
directions. It runs automatically; there is nothing to write.

```bash
ctadl import app.apk           --name app_dex
ctadl import -l pcode libapp.so --name app_native
ctadl index  app app_dex app_native      # <- the bridge fires here
ctadl query  app -m models.json5 -o results.sarif
```

The `index` run logs what it did at `info` level:

```
jni bridge: 14 native method(s): 12 linked, 1 unresolved, 1 ambiguous
```

That line is worth reading. A native method that fails to link produces no flow
*and no error* — the analysis simply comes out quieter than it should. See
[Diagnostics](#diagnostics).

---

## Which symbol implements which method

CTADL resolves a native method exactly as the JNI runtime does, by mangling the
class and method names into a symbol.

```
short = "Java_" + mangle(class-internal-name) + "_" + mangle(method-name)
long  = short + "__" + mangle(parameter-descriptor)
```

The parameter descriptor is the method descriptor with its parentheses and return
type stripped, and `mangle` is:

| character | becomes |
| --- | --- |
| `/` | `_` |
| `_` | `_1` |
| `;` | `_2` |
| `[` | `_3` |
| ASCII alphanumeric | itself |
| anything else | `_0` + four lowercase hex digits of the UTF-16 code unit |

So `Lcom/example/Crypto;->encrypt(Ljava/lang/String;)Ljava/lang/String;` yields
the short name `Java_com_example_Crypto_encrypt` and the long name
`Java_com_example_Crypto_encrypt__Ljava_lang_String_2`.

**Resolution order**, mirroring the runtime: the long name wins when that symbol
exists; otherwise the short name is used, but only when the declaring class has
exactly one native method with that simple name. An overloaded native reached
only by its short name cannot be attributed to one overload, so CTADL warns and
skips it rather than guessing. Give the implementations their long names to
disambiguate.

Matching is against the *simple* name in the native virtual method table, not the
raw IR function name, so a decorated name (Ghidra's uniquing suffixes,
`<EXTERNAL>::sym@addr`) still matches — as does the leading underscore Mach-O
prefixes every C symbol with.

---

## How arguments are mapped

A JNI implementation takes two extra leading parameters before anything the Java
signature declares: the `JNIEnv *`, and then the receiver (`jobject`) for an
instance method or the declaring class (`jclass`) for a static one. A bare call
edge would therefore wire the Java receiver onto `JNIEnv *` and drop every real
argument. The bridge maps ports instead:

| Java side | Native side |
| --- | --- |
| — (nothing) | `0` — `JNIEnv *env` |
| `this`, instance methods only | `1` — `jobject` / `jclass` |
| declared parameter *k* | `2 + k` |
| return value | return value |
| globals | globals |

The Java-side *slot* of parameter *k* is frontend-dependent and is not `k` in
general:

- **Dex** numbers parameters by **register**, and `long`/`double` consume two of
  them. `(JI)V` puts the `int` at slot 2.
- **JVM** numbers parameters by **argument position**, one per declared
  parameter, wide or not. The same `(JI)V` puts the `int` at slot 1.

Both put `this` at slot 0 for an instance method.

Only the *normal* return is mapped: a Java function has return arity 2 (normal
and exception) while a native function has one, and a JNI implementation cannot
throw into the second. Globals are threaded through the bridge exactly as they are
through a real call site, so a native implementation that writes a global is
visible to Java and vice versa. Because ports are bidirectional, by-reference
out-parameters and the return value come back across the boundary with no extra
work.

---

## Diagnostics

The bridge warns, at `warn` level, on the two situations it cannot resolve
silently:

- **Ambiguity.** Either the class has several native overloads of a name whose
  only symbol is the short form, or several native functions carry the matched
  symbol name. The method is skipped.
- **An incomplete native prototype.** The implementation resolved, but the
  disassembler recovered fewer parameters than the port map needs — Ghidra gives
  a function with no recovered prototype zero parameters at all — so some
  arguments have nothing on the far side to flow into. Build the library with
  `-g` (or otherwise give Ghidra the types) and re-import.

A method with no matching symbol at all is reported only in the `LinkStats`
counts, since a Java-only project legitimately has one per `native` declaration.

To reproduce the pre-bridge behaviour — for an A/B measurement of what the bridge
contributes — pass `--no-jni-bridge` to `ctadl index` or `ctadl go`.

---

## Limitations

- **`RegisterNatives` is not handled.** Only the standard mangled-symbol
  convention is linked. An implementation bound dynamically through
  `RegisterNatives` needs the contents of a `JNINativeMethod[]` table, which is a
  separate constant-propagation problem.
- **`JNIEnv` accessor calls are not modelled.** Real native code reaches its
  arguments through the environment vtable — `(*env)->GetStringUTFChars(env, s,
  0)` — an indirect call whose target CTADL cannot currently resolve, so taint
  stops there. The bridge delivers the argument to the native function correctly;
  propagating *through* the accessors additionally needs a default model for the
  `JNINativeInterface` functions and a way to resolve the vtable.
- **Index time only.** Like `propagation` models, the bridge creates facts the
  index fixpoint consumes, so `ctadl query --models` cannot introduce one after
  the fact. Re-run `ctadl index` if you add the native artifact later.
- **One frontend's slot model per method.** If the same method is observed
  through two Java frontends at once (a Dex and a JVM import of the same class),
  the first observation's slot model is used. The two agree except on
  `long`/`double` parameters.
- **SARIF for a multi-import project repeats each result.** Not a property of the
  bridge, but you will meet it as soon as you use one: the formatter resolves each
  result's source span against *every* import's source-info database in turn, and
  those span ids are per-import indices, so a finding in the Java half is emitted
  a second time carrying an unrelated address in the shared library. The dataflow
  is right; only the extra rendering is wrong. Read the copy whose
  `artifactLocation` names the artifact the finding is actually in.

---

## See also

- [Model generators](model-generators.md) — for the code the bridge cannot reach,
  including the `JNIEnv` accessors above.
- `ctadl-ascent/src/languages/jni.rs` — the implementation, and the unit tests
  pinning the mangling table and the port map.
- `nightly/tests/jni/` — the end-to-end regression cases.
