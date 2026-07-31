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

An APK already contains both halves, so importing one imports both:

```bash
ctadl import app.apk                     # Dex, plus every lib/<abi> library in it
ctadl index  app app                     # <- the bridge fires here
ctadl query  app -m models.json5 -o results.sarif
```

`ctadl import` extracts the `.so` files under `lib/<abi>/`, disassembles each
through the pcode frontend, and imports it as its own program named
`<apk>__<abi>__<lib>`. Those names are recorded on the APK's import, so naming
the APK in `ctadl index` co-indexes them — there is nothing extra to type. See
[Native libraries inside an APK](#native-libraries-inside-an-apk).

When the two halves are separate files, import them separately and co-index:

```bash
ctadl import app.dex            --name app_dex
ctadl import -l pcode libapp.so --name app_native
ctadl index  app app_dex app_native
```

The `index` run logs what it did at `info` level:

```
jni bridge: 14 native method(s): 12 linked, 1 unresolved, 1 ambiguous
```

That line is worth reading. A native method that fails to link produces no flow
*and no error* — the analysis simply comes out quieter than it should. See
[Diagnostics](#diagnostics).

The *per-method* resolution lines (`jni bridge: <method> -> <symbol>`) are logged at `debug`,
so a default run does not show them. Run with `RUST_LOG=debug` (or
`RUST_LOG=ctadl_ascent::languages::jni=debug`) to see which symbol each method resolved to and
why an unresolved one did not.

---

## Native libraries inside an APK

`ctadl import app.apk` imports the `.so` files packaged inside it, one program
each, so the bridge has a native half to join without you unzipping anything.

```
$ ctadl import app.apk
[INFO] app.apk: importing native libraries for arm64-v8a (ignoring armeabi-v7a, x86_64; pass --native-abi to choose)
[INFO] app.apk: 2 native libraries ready (2 imported, 0 reused, 0 failed)
$ ctadl index app app
[INFO] jni bridge: 14 native method(s): 12 linked, 1 unresolved, 1 ambiguous
```

Each library becomes an import named `<apk>__<abi>__<lib>` — above, that is
`app__arm64-v8a__libcrypto` and friends. The names are recorded on the APK's own
import, and `ctadl index` expands them, so naming the APK indexes everything that
came out of it. Naming a library explicitly is allowed and does not index it
twice.

**One ABI, not all of them.** An APK usually ships the same library built for
several ABIs. They are copies of one program, so importing more than one would
cost a full disassembly per copy *and* leave several functions carrying each
`Java_…` symbol — which the bridge can only report as ambiguous, and skip. CTADL
imports the first available of `arm64-v8a`, `armeabi-v7a`, `armeabi`, `x86_64`,
`x86`; pass `--native-abi <abi>` to choose another.

**It is never fatal.** Disassembly needs Ghidra, which is a heavy dependency and a
slow step — minutes per library. If Ghidra is not found, or a library fails to
disassemble, CTADL warns and imports the Dex half anyway:

```
[WARN] app.apk: skipping 2 arm64-v8a native libraries -- Ghidra was not found, so they
       cannot be disassembled. Set GHIDRA_HOME or put `ghidra` on PATH to analyze them;
       the Dex half of this APK is imported either way.
```

The cost of that is quiet output, not wrong output: the native methods simply go
unlinked, which the `jni bridge:` counts report. Pass `--no-native-libs` to skip
them deliberately, and `--skip-existing` to reuse the libraries of an unchanged
APK on a re-import rather than disassembling them again.

Only `lib/<abi>/*.so` is searched, and each entry is checked for an object-file
magic before it is handed to the disassembler. A library the app ships elsewhere
(in `assets/`, to extract and `dlopen` at runtime) is not found; import it
separately with `-l pcode`.

### Split APKs (XAPK / app bundles)

An app distributed as an Android App Bundle does not arrive as one APK. It arrives
as several — a base APK holding the Dex, a `config.<abi>.apk` per ABI holding that
ABI's `lib/` directory, and `config.<lang>.apk` / `config.<density>.apk` holding
resources. XAPK downloads (APKPure and the like) are exactly this set, zipped
together. So the Java and native halves land in *different files*, and a
`config.<abi>.apk` has no `classes*.dex` in it at all.

Import them as what they are: each APK on its own, then one project over both.

```bash
ctadl import Telegram/org.telegram.messenger.apk --name tg_dex
ctadl import Telegram/config.arm64_v8a.apk       --name tg_native
ctadl index  tg tg_dex tg_native                 # <- the bridge fires here
```

`tg_native` has an empty Java half and one sub-import per library, so naming it
pulls in `tg_native__arm64-v8a__libtmessages.49` and the rest. Because the two
halves are ordinary co-indexed imports, the bridge joins them exactly as it does
within a single APK.

The resource-only splits have no code in either language, and importing one is an
error rather than an empty program:

```
$ ctadl import Telegram/config.en.apk
Error: nothing to import: '…/config.en.apk' has no classes*.dex entries and no
native libraries under lib/<abi>/. …
```

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

## Reading the results

A bridged project is the ordinary multi-import case, and its SARIF locates every
result in the artifact that result is actually in: the Java half by byte offset
into the `.dex`/`.jar`, the native half by instruction address into the shared
library. Nothing is reported twice, and a location never names the other
artifact.

This works because `ctadl index` records, per instruction, which import its
source span came from. Span ids are *per-import* indices — each artifact's
source-info database numbers its spans from zero, while function and instruction
ids are project-global — so a span read against the wrong import's database still
resolves, to an unrelated line in an unrelated file. That is what used to happen:
every result was rendered once per import, and a Java finding reappeared carrying
an address in the `.so`.

A note on what the native half contributes: CTADL reports a tainted *instruction*
at a call whose argument is tainted, so native code shows up in the log where it
passes tainted data to a function. A native body that only assigns (`g = data;`)
carries the taint just as far — the flow is there and crosses back to Java — but
has no call site to report it at.

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

`--no-jni-bridge` is also what you want when joining a pair *by hand* with a
[`bridge` model](model-generators.md#bridge). A declarative bridge over a pair this pass
already links double-bridges it — two sites, duplicated flows — so switch one of the two off.

---

## Limitations

- **`RegisterNatives` is not handled.** Only the standard mangled-symbol
  convention is linked. An implementation bound dynamically through
  `RegisterNatives` needs the contents of a `JNINativeMethod[]` table, which is a
  separate constant-propagation problem. The correspondence is one a human can read out of that
  table, though, so this is exactly what a hand-written
  [`bridge` model](model-generators.md#bridge) is for.
- **`JNIEnv` accessor calls are not modelled.** Real native code reaches its
  arguments through the environment vtable — `(*env)->GetStringUTFChars(env, s,
  0)` — an indirect call whose target CTADL cannot currently resolve, so taint
  stops there. The bridge delivers the argument to the native function correctly;
  propagating *through* the accessors additionally needs a default model for the
  `JNINativeInterface` functions and a way to resolve the vtable.
- **A library extracted from an APK is located by its extracted path.** SARIF
  results in the native half name the copy CTADL wrote under the store's
  `imports/<apk>/native/<abi>/`, not `app.apk!lib/<abi>/libfoo.so`. The addresses
  are right and the file is byte-identical to the one in the APK; only the path is
  indirect. Locating a result *inside* an archive would need source-info to
  understand intra-archive paths.
- **Index time only.** Like `propagation` models, the bridge creates facts the
  index fixpoint consumes, so `ctadl query --models` cannot introduce one after
  the fact. Re-run `ctadl index` if you add the native artifact later.
- **One frontend's slot model per method.** If the same method is observed
  through two Java frontends at once (a Dex and a JVM import of the same class),
  the first observation's slot model is used. The two agree except on
  `long`/`double` parameters.
- **An index written before this feature cannot be queried.** The index records
  which import each source span belongs to, without which a multi-import project's
  results could not be located (see [Reading the
  results](#reading-the-results)). That is an index format change, so `ctadl
  query` on an older index says so and asks for a re-`index`.

---

## See also

- [`model.bridge`](model-generators.md#bridge) — the declarative construct for the
  boundaries this pass cannot reach: a `RegisterNatives`-bound implementation, a Lua-to-C
  `luaL_Reg` entry, a call through a `dlsym`'d pointer. It takes an explicit port map, since
  nothing derives one for a boundary with no naming convention.
- [Model generators](model-generators.md) — for the code the bridge cannot reach,
  including the `JNIEnv` accessors above.
- `ctadl-ascent/src/languages/jni.rs` — the implementation, and the unit tests
  pinning the mangling table and the port map.
- `nightly/tests/jni/` — the end-to-end regression cases.
