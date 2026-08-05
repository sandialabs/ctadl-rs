# The JNI bridge

An Android app's `native` method is a declaration with no body. Its
implementation lives in a shared library, bound either by a name derived from the
Java class and method (the JNI mangling rules) or at run time by a call to
`RegisterNatives`. Nothing in either artifact names the other, so co-indexing
them is not by itself enough: taint entering a `native` method vanishes, and
taint produced by the implementation never comes back.

The **JNI bridge** closes that gap. Whenever a Java or Dex artifact is indexed
alongside native code, CTADL joins each `native` method to the function
implementing it and maps the arguments across the JNI ABI, so taint flows in both
directions. It runs automatically; there is nothing to write. Both bindings are
covered: the `Java_…` symbol convention, and the `JNINativeMethod[]` tables a
`RegisterNatives` call reads, which CTADL recovers from the library's data
sections at import time (see [Natives bound through
`RegisterNatives`](#natives-bound-through-registernatives) — for most real
Android apps this is where the majority of the links come from).

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
jni registry: 3 table(s), 28 entr(ies) in app__arm64-v8a__libcrypto: 28 attributed to 3 class(es), 0 unattributed
jni bridge: 14 native method(s): 12 linked (9 registered), 1 unresolved, 1 ambiguous
```

Those lines are worth reading. A native method that fails to link produces no
flow *and no error* — the analysis simply comes out quieter than it should. See
[Diagnostics](#diagnostics). `registered` counts the subset of `linked` that came
from a `RegisterNatives` table rather than from a symbol name; only libraries
that actually carry a table get a `jni registry:` line.

The *per-method* resolution lines (`jni bridge: <method> -> <symbol>`) are logged at `debug`,
so a default run does not show them. Run with `RUST_LOG=warn,ctadl=debug` (or
`RUST_LOG=warn,ctadl_ascent::languages::jni=debug`) to see which symbol each method resolved to
and why an unresolved one did not. See [Logging](debugging.md#logging).

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

**Import the `.xapk` directly.** CTADL unwraps the bundle, imports each split
through the ordinary APK path, and records them all on the bundle's import, so
naming the bundle co-indexes everything in it:

```bash
ctadl import app.xapk
ctadl index  app app                     # <- the bridge fires here
```

```
[INFO] app.xapk: 30 split APK(s): app__myapp, app__config.arm64_v8a, …
[INFO] app.xapk: 7 split(s) imported, 23 resource-only split(s) skipped
```

Dex-bearing splits are imported first, so the Java half of the boundary is
observed before the native half. The resource-only splits — usually most of the
file count — hold no code in either language and are skipped, not fatal.

The bundle's own import contributes no program: everything is in the splits,
named `<bundle>__<split-stem>` and, below those, `<split>__<abi>__<lib>`. All of
them are recorded on the bundle in one flat list, which is what makes naming the
bundle enough.

Splits you have already unzipped work too — import each as what it is, then one
project over both:

```bash
ctadl import Telegram/org.telegram.messenger.apk --name tg_dex
ctadl import Telegram/config.arm64_v8a.apk       --name tg_native
ctadl index  tg tg_dex tg_native                 # <- the bridge fires here
```

`tg_native` has an empty Java half and one sub-import per library, so naming it
pulls in `tg_native__arm64-v8a__libtmessages.49` and the rest. Because the two
halves are ordinary co-indexed imports, the bridge joins them exactly as it does
within a single APK.

Importing a resource-only split *by itself* is an error rather than an empty
program — inside a bundle it is skipped, but on its own it is almost certainly
not the file you meant:

```
$ ctadl import Telegram/config.en.apk
Error: nothing to import: '…/config.en.apk' has no classes*.dex entries and no
native libraries under lib/<abi>/. …
```

### When the preferred ABI holds no code

Some apps ship a placeholder for one ABI and their real code for another — Chrome
ships a **zero-byte** `lib/arm64-v8a/libplaceholder.so` and builds
`lib/armeabi-v7a/libelements.so` for real. Taking the ABI preference literally
there yields an import with no native libraries at all.

So an ABI whose every entry fails the object-file magic check is skipped, and the
next one in the preference order is used. The choice is never silent:

```
[INFO] app.apk: skipping arm64-v8a -- it has no entry there is a loadable object file
       (an empty placeholder, say); pass --native-abi to override
```

An explicit `--native-abi` is still honored as given, including when it names an
ABI with nothing usable in it: that is reported rather than worked around.

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

**Resolution order**, mirroring the runtime: a recovered `RegisterNatives`
binding wins outright — that is what the runtime does, and it names the method
unambiguously. Failing that, the long name wins when that symbol exists;
otherwise the short name is used, but only when the declaring class has exactly
one native method with that simple name. An overloaded native reached only by its
short name cannot be attributed to one overload, so CTADL warns and skips it
rather than guessing. Give the implementations their long names to disambiguate,
or bind them with `RegisterNatives`.

Where a method resolves both ways and the two disagree, CTADL takes the
registration and says so at `warn`.

Matching is against the *simple* name in the native virtual method table, not the
raw IR function name, so a decorated name (Ghidra's uniquing suffixes,
`<EXTERNAL>::sym@addr`) still matches — as does the leading underscore Mach-O
prefixes every C symbol with.

---

## Natives bound through `RegisterNatives`

Most Android apps do not use the symbol convention. They call

```c
env->RegisterNatives(clazz, table, count);
```

from `JNI_OnLoad`, passing an array of

```c
typedef struct { const char *name; const char *signature; void *fnPtr; } JNINativeMethod;
```

and the implementations keep private, unexported names. Under the symbol
convention alone such an app links almost nothing: one real package declares 535
`native` methods in its Dex and exports exactly one `Java_…` symbol across every
library it ships.

Those tables are ordinary initialized data, so CTADL recovers them without
Ghidra and without any dataflow analysis.

**At import time**, each ELF library's writable, non-executable data sections
(`.data.rel.ro`, `.data`) are walked at pointer stride, and a triple is taken as
a `JNINativeMethod` when the first slot points at a valid Java method name, the
second at a well-formed method descriptor, and the third into executable code.
Each recovered `fnPtr` is then resolved to the function the disassembler found at
that address. The result is written beside the import's other artifacts as
`jni-registry.json`:

```bash
ctadl inspect ~/.local/state/ctadl/imports/app__arm64-v8a__libcrypto/jni-registry.json
```

```
28 RegisterNatives entries (entry size 24)
  0x2a000  0x10c20  readBytesNative(JII[BI)V  -> readBytesNative
  ...
```

**Branch veneers are followed.** A `fnPtr` does not always point at the
implementation. When the linker cannot reach it from the table's own range it
emits a *veneer* — a four-byte stub holding one `B` — and the address that
reaches `RegisterNatives` is the stub's. No disassembler makes a function out of
a bare thunk, so such an entry would resolve to nothing at all. CTADL decodes the
branch and resolves its target instead, recording where it went as
`veneer_target`; `fn_addr` stays the address the table holds, so a spot-check
against the library still lines up.

```
  0x46850  0x40e74  writeNative(JLjava/io/OutputStream;)V  -> FUN_0010a02c (via a veneer to 0xa02c)
```

Whether a library is linked this way is not a property of the library: one real
app ships `libsuperpack-jni.so` with every one of its 28 pointers a veneer, and
another ships the same library with none. Only the AArch64 `B` is decoded, and
only one hop. Measured across the reference corpus that is the whole of it —
every veneer found is a single branch straight to its implementation, and the
32-bit libraries hold none in this position.

Scanning is unconditional and costs milliseconds. Anything that is not an ELF
file on disk — a `ghidra://` repository, a `.gpr` project, a Mach-O or PE binary,
or one of the ZIPs and packed containers apps occasionally ship under `lib/` — is
a quiet no-op.

**At index time**, each entry's *declaring class*, which the table does not
carry, is recovered from the Dex side. A `JNINativeMethod[]` is contiguous, and
one class's table begins where the previous one ends, so CTADL walks the entries
in address order keeping the set of Java classes that declare *every* entry so
far, matched on name **and** full descriptor. When that set empties the run
closes and a new one begins; a closed run with exactly one surviving class
attributes all of its entries to it. Runs are also split at an address gap and at
a repeated `(name, descriptor)`, since a table cannot register the same method
twice.

Matching is by containment, never by count: a class that declares 14 natives and
registers 13 of them is ordinary.

Anything that run does not attribute is **counted and reported, never guessed
at**. There is no "the name is globally unique, so it must be this one" tier:
measured across 4280 entries in eleven packages, the number of unattributed
entries whose `(name, descriptor)` is globally unique is **zero**. Every entry
that fails attribution either matches no declared `native` at all or matches
several classes, and a uniqueness rule rescues neither. Do not re-add that tier
without new evidence.

Attributing nothing is often the right answer. A library whose Java classes ship
outside `classes.dex` — VLC's bundled libbluray BD-J bindings, a feature-split
dex — yields well-formed tables that match nothing, each entry closes its own
run, and no link is fabricated.

**Two things to know when using it:**

- The sidecar is written at **import** time, so a library imported before this
  feature existed has none. `ctadl import --skip-existing` reuses an unchanged
  library's import directory and will *not* create one — re-import without
  `--skip-existing` to gain it.
- `ctadl index --no-jni-registry` ignores the sidecar, leaving the bridge with
  the symbol convention alone. That is the clean A/B for what the registry
  contributes, and it needs no re-import. `--no-jni-bridge` implies it.

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

The bridge warns, at `warn` level, on the situations it cannot resolve silently:

- **Ambiguity.** Either the class has several native overloads of a name whose
  only symbol is the short form, or several native functions carry the matched
  symbol name. The method is skipped. A `RegisterNatives` binding resolves this
  case outright, since it names the descriptor.
- **An incomplete native prototype.** The implementation resolved, but the
  disassembler recovered fewer parameters than the port map needs — Ghidra gives
  a function with no recovered prototype zero parameters at all — so some
  arguments have nothing on the far side to flow into. Build the library with
  `-g` (or otherwise give Ghidra the types) and re-import.
- **A registration that disagrees with a symbol.** Both bindings exist and name
  different functions. CTADL takes the registration, as the runtime does.

A method with no matching symbol at all is reported only in the `LinkStats`
counts, since a Java-only project legitimately has one per `native` declaration.
So are unattributed table entries; both are on the `jni registry:` and
`jni bridge:` lines.

To reproduce the pre-bridge behaviour — for an A/B measurement of what the bridge
contributes — pass `--no-jni-bridge` to `ctadl index` or `ctadl go`. For the
narrower question of what the `RegisterNatives` tables contribute, pass
`--no-jni-registry`, which leaves the symbol convention working.

`--no-jni-bridge` is also what you want when joining a pair *by hand* with a
[`bridge` model](model-generators.md#bridge). A declarative bridge over a pair this pass
already links double-bridges it — two sites, duplicated flows — so switch one of the two off.

---

## Limitations

- **A `RegisterNatives` entry whose class cannot be recovered is not linked.**
  The table carries a name, a descriptor and a function pointer, but not the
  declaring class; that comes from the Dex side, by [contiguous-run
  attribution](#natives-bound-through-registernatives). Where the Java half is
  present this attributes 97–100% of entries, but an entry whose class ships
  outside the imported Dex — or one in a run that stays ambiguous — is counted
  unattributed and left alone. Following `FindClass` through the decompiled
  `JNI_OnLoad` would recover the rest, and would need the `JNIEnv` vtable
  offsets; for now a [`bridge` model](model-generators.md#bridge) is how you join
  such a pair by hand.
- **Only ELF libraries are scanned for tables.** The scan reads the library's own
  bytes, so a Mach-O or PE artifact, a `ghidra://` repository, or an existing
  `.gpr` project contributes no registrations — quietly, since none of those is
  an Android shared library.
- **The table has to be in the APK.** A library that is packed, compressed, or
  otherwise not shipped as a loadable `.so` (Facebook Lite's Superpack payloads,
  TikTok's `\x7fKOM` and `SKCL` containers) has no data section to scan until
  something unpacks it. No amount of JNI linking reaches code that is not in the
  file.
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
  boundaries this pass cannot reach: a Lua-to-C `luaL_Reg` entry, a call through a `dlsym`'d
  pointer, a `RegisterNatives` entry whose class stayed unattributed. It takes an explicit
  port map, since nothing derives one for a boundary with no naming convention.
- [Model generators](model-generators.md) — for the code the bridge cannot reach,
  including the `JNIEnv` accessors above.
- `ctadl-ascent/src/languages/jni.rs` — the implementation, and the unit tests
  pinning the mangling table and the port map.
- `ctadl-ascent/src/languages/jni/registry.rs` — the ELF table scan and the run
  attribution, with unit tests over both.
- `nightly/tests/jni/` — the end-to-end regression cases, including
  `JniRegister`, whose boundary no symbol name joins.
