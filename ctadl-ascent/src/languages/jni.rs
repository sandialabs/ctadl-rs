/*! JNI bridge: links Java `native` method stubs to their native implementations.

An Android app's `native` method is a bodyless stub in the Dex (or JVM) program; its
implementation is a `Java_…` symbol in a shared library, imported through the pcode frontend.
Functions are interned by name (`IdMap::get_or_add_function`), so co-indexing two artifacts already
makes identically-spelled functions share a [`FunctionId`] -- but the two sides of a JNI boundary
are *not* spelled identically, and nothing joins them. Taint entering a native method vanishes and
taint produced by the implementation never comes back.

Name coincidence would not be enough even if it happened: the JNI ABI shifts every argument by two
(`JNIEnv *`, then `jobject`/`jclass`), so a bare call edge would wire the Java receiver to `JNIEnv *`
and drop every real argument -- silently, with no flow and no diagnostic.

The bridge closes that gap wherever a Java artifact is co-indexed with native code, without any
user input. A method it fails to link produces no flow *and no error*, so [`LinkStats`] and the
`info` line it prints are the only signal the pass fired at all; the per-method pairings are logged
at `debug`. The README covers running it and reading those counts.

# What the bridge emits

No new relation and no new inference rule. The index engine already turns a call into dataflow with
two rules that meet at the argument index `n`: `actual_param` binds caller vertices to per-site
call-arg pseudo-variables in *both* directions, and a callee's `summary` is replayed between the
call-arg pseudo-variables of any site that targets it. So a bridge is **one `call` row plus one
`actual_param` row per mapped port**, synthesized *inside* the bodyless Java stub. The stub thereby
acquires a real summary of its own, and every call site of that native method anywhere in the
program composes with it for free -- one edge per native method, not one per call site.

Two constraints follow from that rule set:

- The site must be **fresh**. Call-arg pseudo-variables key on the instruction id, so reusing an
  existing site would alias its argument *n* to the bridge's argument *n*.
- The Java stub needs **`formal_param` rows**: the summary rule joins on them and `locals` is seeded
  from them. A Dex `native` method has zero declared parameters (the dex frontend sets parameters up
  only when it finds a code item), so the bridge emits the rows itself, exactly as
  [`crate::codegen::model_matches::codegen_model_matches`] does for modelled functions.

# Which symbol implements which method

CTADL resolves a native method exactly as the JNI runtime does, by mangling the class and method
names into a symbol ([`short_name`], [`long_name`], [`mangle_component`]):

```text
short = "Java_" + mangle(class-internal-name) + "_" + mangle(method-name)
long  = short + "__" + mangle(parameter-descriptor)
```

| character | becomes |
| --- | --- |
| `/` | `_` |
| `_` | `_1` |
| `;` | `_2` |
| `[` | `_3` |
| ASCII alphanumeric | itself |
| anything else | `_0` + four lowercase hex digits of the UTF-16 code unit |

So `Lcom/example/Crypto;->encrypt(Ljava/lang/String;)Ljava/lang/String;` yields the short name
`Java_com_example_Crypto_encrypt` and the long name
`Java_com_example_Crypto_encrypt__Ljava_lang_String_2`.

**Resolution order**, mirroring the runtime (see [`resolve`]): a recovered `RegisterNatives`
binding wins outright. Failing that, the long name wins when that symbol exists; otherwise the
short name is used, but only when the declaring class has exactly one native method with that
simple name. An overloaded native reached only by its short name cannot be attributed to one
overload, so the pass warns and skips it rather than guessing. Where a method resolves both ways
and the two disagree, the registration wins and the disagreement is logged at `warn`.

Matching is against the *simple* name in the native VMT, not the raw IR function name, so a
decorated name (Ghidra's uniquing suffixes, `<EXTERNAL>::sym@addr`) still matches -- as does the
leading underscore Mach-O prefixes every C symbol with.

# Two ways a native method finds its implementation

The symbol convention above is one of them, and the only one a JVM applies on its own. The other
is `env->RegisterNatives(clazz, table, n)`, which an app calls from `JNI_OnLoad` to bind a
`JNINativeMethod[]` at run time -- name, descriptor and function pointer, with no exported symbol
anywhere. Most Android apps use it for most of their natives: one real package declares 535
`native` methods in its Dex and exports exactly one `Java_…` symbol across every library it ships.

[`registry`] recovers those tables straight out of the library's data sections at import time,
writing them beside the import's other artifacts as `jni-registry.json`, and recovers each entry's
declaring class -- which the table does not carry -- from the Dex side at index time.
[`resolve`] consults that result *first*, because it is what the runtime does: a method bound by
`RegisterNatives` runs the registered function even when a matching `Java_…` symbol also exists.

Attribution never guesses. There is no "the name is globally unique, so it must be this one" tier:
measured across 4280 entries in eleven packages, the number of unattributed entries whose
`(name, descriptor)` is globally unique is **zero**. Every entry that fails attribution either
matches no declared `native` at all or matches several classes, and a uniqueness rule rescues
neither. Do not re-add that tier without new evidence. Attributing nothing is often the right
answer: a library whose Java classes ship outside `classes.dex` (a bundled BD-J stack, a
feature-split dex) yields well-formed tables that match nothing, and no link is fabricated.

Because the scan runs at import time, a library imported before this feature existed has no
sidecar, and a re-import reuses it unchanged without creating one unless given `--force`. `--no-jni-registry` ignores the
sidecar at index time, leaving the symbol convention alone.

# How arguments are mapped

A JNI implementation takes two extra leading parameters before anything the Java signature
declares. [`port_map`] maps ports across that shift:

| Java side | Native side |
| --- | --- |
| -- (nothing) | `0` -- `JNIEnv *env` |
| `this`, instance methods only | `1` -- `jobject` / `jclass` |
| declared parameter *k* | the next native index, or the next two for a split `long`/`double` |
| return value | return value |
| globals | globals |

Where the declared parameters land depends on the [`NativeSlotModel`]:

| Layout | Used for | Declared `J`/`D` | Everything else |
| --- | --- | --- | --- |
| `Typed` | every ABI when the recovered count matches it; the default | 1 native index | 1 native index |
| `SplitWide` | 32-bit ABIs (`armeabi-v7a`, `x86`) only | 2 consecutive native indices (low, high) | 1 native index |

A 32-bit ABI passes a `long` or `double` in two registers or stack words, and a disassembler that
recovered no types shows two parameters. [`choose_layout`] picks the layout per method, from the
library's ABI ([`NativeAbi`]) and the parameter count Ghidra recovered ([`NativeProto`]).

The Java-side *slot* of parameter *k* is frontend-dependent and is not `k` in general, which is
what [`SlotModel`] captures: Dex numbers parameters by *register*, so `long`/`double` consume two
and `(JI)V` puts the `int` at slot 2, while the JVM numbers them by *argument position* and puts
the same `int` at slot 1. Both put `this` at slot 0 for an instance method.

Only the *normal* return is mapped: a Java function has return arity 2 (normal and exception) while
a native function has one, and a JNI implementation cannot throw into the second. Globals are
threaded through exactly as they are at a real call site. Because ports are bidirectional,
by-reference out-parameters and the return value come back across the boundary with no extra work.

# Linking against a summary project

The native half does not have to be co-indexed. `ctadl index app --no-native-libs --summary xproj`
indexes the app's Java half alone, against a project `xproj` that indexed one of its libraries on
its own:

```sh
ctadl import app.apk
ctadl index xproj app__arm64-v8a__libX
ctadl index appproj app --no-native-libs --summary xproj
ctadl query appproj -m models.json
```

`cli::index` feeds the observer the symbol and `RegisterNatives` tables of each summary project's
imports, never their IR, as native targets with [`Origin::Summary`]. [`link`] links a Java
`native` to such a target as it would to a co-indexed one, and the summaries `--summary` maps in
carry the flow across the bridge. [`LinkStats::from_summary`] counts these links.

`--no-jni-bridge` skips all of this, and `--no-jni-registry` skips the summary projects' tables
as it does the current project's.

# Reading the results

A bridged project is the ordinary multi-import case, and its SARIF locates every result in the
artifact that result is actually in: the Java half by byte offset into the `.dex`/`.jar`, the native
half by instruction address into the shared library. This works because `ctadl index` records, per
instruction, which import its source span came from. Span ids are *per-import* indices -- each
artifact's source-info database numbers its spans from zero, while function and instruction ids are
project-global -- so a span read against the wrong import's database still resolves, to an
unrelated line in an unrelated file. That is what used to happen: every result was rendered once per
import, and a Java finding reappeared carrying an address in the `.so`.

# Diagnostics

The pass warns, at `warn` level, on what it cannot resolve silently:

- **Ambiguity.** Either the class has several native overloads of a name whose only symbol is the
  short form, or several native functions carry the matched symbol name. The method is skipped. A
  `RegisterNatives` binding resolves this case outright, since it names the descriptor.
- **A native prototype that does not fit the descriptor.** The implementation resolved, but the
  parameter count Ghidra recovered is not what the Dex descriptor implies under any layout: none
  recovered, too few, or too many (see [`ProtoCheck`]). One warning per method, each counted in
  [`LinkStats::prototype_mismatch`]; the link is emitted either way. Build the library with `-g`
  (or otherwise give Ghidra the types) and re-import.
- **A registration that disagrees with a symbol.** Both bindings exist and name different
  functions; the registration wins, as it does at run time.

A method with no matching symbol at all appears only in [`LinkStats`], since a Java-only project
legitimately has one per `native` declaration. So do unattributed table entries.

# Limitations

- **A `RegisterNatives` entry whose class cannot be recovered is not linked.** Where the Java half
  is present, run attribution recovers 97-100% of entries, but an entry whose class ships outside
  the imported Dex -- or one in a run that stays ambiguous -- is counted unattributed and left
  alone. Following `FindClass` through the decompiled `JNI_OnLoad` would recover the rest, and
  would need the `JNIEnv` vtable offsets.
- **Only ELF libraries are scanned for tables**, and only a library actually shipped as a loadable
  `.so`. A packed or compressed payload has no data section to scan until something unpacks it.
- **`JNIEnv` accessor calls are not modelled.** Real native code reaches its arguments through the
  environment vtable -- `(*env)->GetStringUTFChars(env, s, 0)` -- an indirect call whose target
  CTADL cannot currently resolve, so taint stops there. The bridge delivers the argument to the
  native function correctly; propagating *through* the accessors additionally needs a default model
  for the `JNINativeInterface` functions and a way to resolve the vtable.
- **Index time only.** Like `propagation` models, the bridge creates facts the index fixpoint
  consumes, so `ctadl query --models` cannot introduce one after the fact. Re-run `ctadl index` if
  you add the native artifact later.
- **One frontend's slot model per method.** If the same method is observed through two Java
  frontends at once (a Dex and a JVM import of the same class), the first observation's slot model
  is used. The two agree except on `long`/`double` parameters.
- **An index written before this feature cannot be queried**, since the per-import span provenance
  above is an index format change. `ctadl query` on an older index says so and asks for a
  re-`index`.
- **Floating-point parameters may be mis-slotted.** On `arm64-v8a`, `x86_64` and hard-float
  targets, `float` and `double` arrive in FP registers. Without a recovered prototype, Ghidra may
  list them after the integer parameters rather than in declaration order, and `Typed` then maps
  them to the wrong native indices. The count check cannot see this.
- **The prototype check compares counts only.** The pcode frontend records `_` for every
  parameter type, so a mis-slotting that keeps the count right passes silently. Checking types
  needs the frontend to record them.
- **`armeabi-v7a` register-pair padding is not modelled.** AAPCS passes a 64-bit argument in an
  even/odd register pair, so for `(IJ)V` it skips `r3`. If Ghidra, without a prototype, shows the
  skipped register as a parameter, the count matches neither layout and the method gets the
  *too many* warning. The 32-bit regression fixture is `x86`, whose cdecl ABI has no padding.
- **A stripped 32-bit x86 export may show no parameters, and has no return value.** For a
  function nothing in the library calls, which is every JNI entry point, Ghidra recovers cdecl
  stack parameters only when the function reads the lowest slots too: one that ignores `env` and
  `jobject` gets no parameters at all, and the *no prototype* warning. It never infers a return
  value for such a function, so taint cannot come back to Java through one. Separately, the pcode
  frontend does not model a 64-bit return (`EDX:EAX`) on x86 as one value, even with DWARF. The
  `JniWide` regression case is shaped around all three.
- **Only context-free summaries cross from a summary project.** `--summary` maps `summary` rows,
  not `context_summary` or `critical_summary`, so a flow in the library that depends on resolving
  an indirect call is lost.
- **A result through a summary-sourced native is located on the Java side only.** The library's
  instructions are not in the app's index, only its summaries, so the SARIF has no location
  inside it. Co-index the library to see the native half of a flow.

# See also

- [`registry`] -- the ELF table scan and the run attribution, with unit tests over both.
- `docs/model-generators.md` -- the declarative `bridge` construct, for the boundaries this pass
  cannot reach: a Lua-to-C `luaL_Reg` entry, a call through a `dlsym`'d pointer, a
  `RegisterNatives` entry whose class stayed unattributed. It takes an explicit port map, since
  nothing derives one for a boundary with no naming convention.
- `nightly/tests/jni/` -- the end-to-end regression cases, including `JniRegister`, whose boundary
  no symbol name joins.
*/

use hashbrown::hash_map::HashMap;
use hashbrown::hash_set::HashSet;
use std::collections::BTreeMap;

use ctadl_ir::ProgramInfo;
use ctadl_ir::mir::call::VirtualMethodTable;

/// Scans an ELF file for `RegisterNatives` calls. The code lives in [`ctadl_pcode`] because it
/// needs Ghidra's image base and its map of entry points, and those exist only while
/// `import_pcode` is running. This module uses the file that the scan wrote, not the scanner
/// itself, so all it needs from that crate is the types. Re-exported so the name
/// `jni::registry::…` works here.
pub use ctadl_pcode::jni_registry as registry;

use crate::codegen::{GLOBALS_INDEX, RETURN_INDEX};
use crate::error::Error;
use crate::facts::{
    self, FlowVariable, FlowVertex, FormalIndex, FormalType, FunctionId, PackedInsnSiteId,
};
use crate::index_engine::IndexFacts;
use crate::index_engine::source_info::IndexSourceInfo;
use crate::project::{ArtifactImport, ArtifactLanguage};

/// How a Java frontend numbers a method's declared parameters.
///
/// The Java-side slot of declared parameter *k* is frontend-dependent and is **not** `k` in
/// general, which is why the port map takes this instead of assuming a fixed `+2` shift.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum SlotModel {
    /// Dex: parameter indices are *register* slots, and `long`/`double` consume two of them.
    Register,
    /// JVM: parameter indices are *argument* positions, one per declared parameter, wide or not.
    Argument,
}

impl SlotModel {
    /// The slot model an imported artifact's frontend uses. Only the Java frontends can
    /// contribute `native` methods; the value is irrelevant for the others.
    pub fn for_language(language: ArtifactLanguage) -> Self {
        match language {
            ArtifactLanguage::Dex | ArtifactLanguage::Apk => SlotModel::Register,
            _ => SlotModel::Argument,
        }
    }

    /// How many slots a parameter of this type descriptor occupies.
    fn width(self, descriptor: &str) -> i16 {
        match self {
            SlotModel::Register if descriptor == "J" || descriptor == "D" => 2,
            _ => 1,
        }
    }
}

/// The ABI a native library was built for, which decides how a `long` or `double` argument
/// arrives: in one native parameter on a 64-bit ABI, possibly in two on a 32-bit one.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum NativeAbi {
    /// `arm64-v8a`.
    Arm64,
    /// `x86_64`.
    X86_64,
    /// `armeabi-v7a` (and `armeabi`).
    Arm32,
    /// `x86`.
    X86,
    /// Not an ELF this recognizes, or no artifact to read. Treated like the 64-bit ABIs.
    Unknown,
}

impl NativeAbi {
    /// The ABI of an import, read from the ELF header of its artifact. For an APK's native
    /// sub-import that is the `.so` extracted into the store. A non-ELF file, a missing artifact
    /// or a Ghidra-server URL gives [`NativeAbi::Unknown`].
    pub fn of(import: &ArtifactImport) -> Self {
        use std::io::Read;
        if crate::project::is_ghidra_server_url(&import.artifact_path) {
            return NativeAbi::Unknown;
        }
        let mut header = [0u8; registry::ELF_IDENT_PREFIX];
        let read = std::fs::File::open(&import.artifact_path)
            .and_then(|mut file| file.read_exact(&mut header));
        match read {
            Ok(()) => Self::from_elf_header(&header),
            Err(_) => NativeAbi::Unknown,
        }
    }

    /// The ABI named by the start of an ELF file. See [`registry::elf_machine_class`].
    pub fn from_elf_header(data: &[u8]) -> Self {
        use object::elf;
        match registry::elf_machine_class(data) {
            Some((elf::EM_AARCH64, elf::ELFCLASS64)) => NativeAbi::Arm64,
            Some((elf::EM_X86_64, elf::ELFCLASS64)) => NativeAbi::X86_64,
            Some((elf::EM_ARM, elf::ELFCLASS32)) => NativeAbi::Arm32,
            Some((elf::EM_386, elf::ELFCLASS32)) => NativeAbi::X86,
            _ => NativeAbi::Unknown,
        }
    }

    /// Whether a `long`/`double` argument can arrive split across two native parameters.
    pub fn is_32bit(self) -> bool {
        matches!(self, NativeAbi::Arm32 | NativeAbi::X86)
    }
}

/// The prototype Ghidra recovered for a native function, as far as the pcode frontend records
/// it: a parameter count and a varargs flag. Parameter types are not recorded.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum NativeProto {
    /// No prototype was recovered. The function has no parameters in the IR at all.
    Unknown,
    /// `params` recovered parameters, plus `...` when `vararg`.
    Known { params: usize, vararg: bool },
}

impl NativeProto {
    /// Parses a native VMT signature. The pcode frontend writes `ret(_, _, …)`, one `_` per
    /// recovered parameter and a trailing `...` for varargs, or `()` when Ghidra recovered no
    /// prototype. Anything else is [`NativeProto::Unknown`].
    pub fn parse(sig: &str) -> Self {
        // The parameter list is the *last* parenthesized group: a return type may hold
        // parentheses of its own (a function pointer), and parameters are only `_` and `...`.
        let Some((ret, params)) = sig.trim().rsplit_once('(') else {
            return NativeProto::Unknown;
        };
        let Some(params) = params.strip_suffix(')') else {
            return NativeProto::Unknown;
        };
        if ret.trim().is_empty() {
            return NativeProto::Unknown;
        }
        let (mut count, mut vararg) = (0, false);
        for param in params.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            if param == "..." {
                vararg = true;
            } else {
                count += 1;
            }
        }
        NativeProto::Known {
            params: count,
            vararg,
        }
    }
}

/// Where a native target was observed.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Origin {
    /// An import of the project being indexed.
    Current,
    /// An import of the named `--summary` project. Its code is not in this index; only its
    /// symbol table, its `RegisterNatives` tables and its saved summaries are.
    Summary(String),
}

/// One native function the bridge can link a Java `native` method to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeTarget {
    /// The fully-qualified IR function name, which is what the call edge targets.
    pub function: String,
    pub proto: NativeProto,
    pub abi: NativeAbi,
    pub origin: Origin,
}

/// How a native implementation receives its declared parameters, past `JNIEnv *` and the
/// `jobject`/`jclass`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum NativeSlotModel {
    /// One native parameter per declared parameter, wide or not. What every ABI does when the
    /// disassembler recovered a typed prototype, and the default.
    Typed,
    /// A `long`/`double` recovered as two consecutive native parameters (low half, then high
    /// half), everything else as one. What a 32-bit ABI looks like to a disassembler that
    /// recovered no types: the value really does arrive in two registers or stack words.
    SplitWide,
}

impl NativeSlotModel {
    /// How many native parameters a parameter of this type descriptor occupies.
    fn width(self, descriptor: &str) -> i16 {
        match self {
            NativeSlotModel::SplitWide if is_wide(descriptor) => 2,
            _ => 1,
        }
    }

    /// How many native parameters a method with these declared parameters has under this
    /// layout, `JNIEnv *` and the `jobject`/`jclass` included.
    fn count(self, params: &[&str]) -> usize {
        2 + params.iter().map(|p| self.width(p) as usize).sum::<usize>()
    }
}

/// Whether a parameter descriptor is a `long` or a `double`.
fn is_wide(descriptor: &str) -> bool {
    descriptor == "J" || descriptor == "D"
}

/// What the prototype check found for one linked method. See [`choose_layout`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtoCheck {
    /// The recovered count is the one the chosen layout implies.
    Match,
    /// Ghidra recovered no prototype, so the native function has no parameters in the IR.
    NoPrototype,
    /// Fewer native parameters were recovered than the chosen layout needs; the arguments past
    /// the last recovered one have nothing to flow into.
    TooFew { recovered: usize, expected: usize },
    /// More were recovered than any layout implies, so the arguments may be mis-slotted.
    /// `split` is the `SplitWide` count when that layout applies to this ABI and method.
    TooMany {
        recovered: usize,
        typed: usize,
        split: Option<usize>,
    },
}

/// Chooses the native slot layout for one linked method and checks its recovered prototype.
///
/// The Dex descriptor is authoritative; the recovered count only picks between the layouts it
/// allows:
///
/// 1. If the count equals the [`NativeSlotModel::Typed`] count, use `Typed`.
/// 2. Otherwise, on a 32-bit ABI, for a method with a `long`/`double` parameter, if it equals
///    the [`NativeSlotModel::SplitWide`] count, use `SplitWide`.
/// 3. Otherwise use `Typed`, and report the mismatch.
///
/// A varargs prototype with more parameters than the layout needs is not a mismatch.
pub fn choose_layout(
    params: &[&str],
    proto: NativeProto,
    abi: NativeAbi,
) -> (NativeSlotModel, ProtoCheck) {
    let typed = NativeSlotModel::Typed.count(params);
    let split = (abi.is_32bit() && params.iter().any(|p| is_wide(p)))
        .then(|| NativeSlotModel::SplitWide.count(params));
    let NativeProto::Known {
        params: recovered,
        vararg,
    } = proto
    else {
        return (NativeSlotModel::Typed, ProtoCheck::NoPrototype);
    };
    if recovered == typed {
        return (NativeSlotModel::Typed, ProtoCheck::Match);
    }
    if split == Some(recovered) {
        return (NativeSlotModel::SplitWide, ProtoCheck::Match);
    }
    let check = if recovered < typed {
        ProtoCheck::TooFew {
            recovered,
            expected: typed,
        }
    } else if vararg {
        ProtoCheck::Match
    } else {
        ProtoCheck::TooMany {
            recovered,
            typed,
            split,
        }
    };
    (NativeSlotModel::Typed, check)
}

// ---------------------------------------------------------------------------
// Name mangling (JNI spec, "Resolving Native Method Names")
// ---------------------------------------------------------------------------

/// Mangles one component of a JNI symbol name.
///
/// Per the JNI spec: `/` becomes `_`, `_` becomes `_1`, `;` becomes `_2`, `[` becomes `_3`, ASCII
/// alphanumerics pass through, and anything else becomes `_0` followed by four lowercase hex digits
/// of its UTF-16 code unit (two escapes for a character outside the BMP, which is one surrogate
/// pair).
pub fn mangle_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '/' => out.push('_'),
            '_' => out.push_str("_1"),
            ';' => out.push_str("_2"),
            '[' => out.push_str("_3"),
            c if c.is_ascii_alphanumeric() => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("_0{:04x}", unit));
                }
            }
        }
    }
    out
}

/// The *short* JNI symbol name: `Java_<class>_<method>`. `class_internal` is the internal form
/// (`com/example/Crypto`), not the type descriptor.
pub fn short_name(class_internal: &str, method: &str) -> String {
    format!(
        "Java_{}_{}",
        mangle_component(class_internal),
        mangle_component(method)
    )
}

/// The *long* JNI symbol name: the short name, `__`, then the mangled parameter descriptor
/// (parentheses and return type stripped, e.g. `Ljava/lang/String;` for
/// `(Ljava/lang/String;)Ljava/lang/String;`).
pub fn long_name(class_internal: &str, method: &str, param_descriptor: &str) -> String {
    format!(
        "{}__{}",
        short_name(class_internal, method),
        mangle_component(param_descriptor)
    )
}

/// Strips the `L...;` wrapper off a Java type descriptor, yielding the internal class name the
/// mangler wants. A name that is not in descriptor form is returned unchanged.
pub fn internal_class_name(class: &str) -> &str {
    class
        .strip_prefix('L')
        .and_then(|c| c.strip_suffix(';'))
        .unwrap_or(class)
}

/// Returns the JVM parameter descriptors in a method descriptor, in order.
///
/// The code lives in [`ctadl_pcode::jni_registry`] rather than here, because the registry
/// scanner uses it to tell a `JNINativeMethod`'s descriptor field from a pointer into unrelated
/// data, and that scanner is in a crate below this one. Re-exported so the name
/// `jni::descriptor_params` works here.
pub use ctadl_pcode::jni_registry::descriptor_params;

/// The parameter descriptor the long name mangles: the method descriptor with its parentheses and
/// return type stripped.
pub fn param_descriptor(descriptor: &str) -> Option<String> {
    Some(descriptor_params(descriptor)?.concat())
}

/// The `(java_index, native_index)` port pairs for one native method, including `this`, the return
/// value and the globals pseudo-parameter.
///
/// The native side is fixed by the JNI ABI up to `native`: index 0 is `JNIEnv *` (never mapped),
/// index 1 is the receiver `jobject` for an instance method or the declaring `jclass` for a static
/// one, and the declared parameters follow from index 2, one native index each under
/// [`NativeSlotModel::Typed`]. Under [`NativeSlotModel::SplitWide`] a `long`/`double` takes two,
/// and its one Java slot is mapped to *both*, so taint reaches whichever half the native code
/// reads. The Java side depends on `slots`. Only `-1` is mapped for returns: a Java function has
/// return arity 2 (`-1` normal, `-2` exception) while a native function has one.
///
/// Returns `None` if `descriptor` is not a well-formed method descriptor.
pub fn port_map(
    descriptor: &str,
    is_static: bool,
    slots: SlotModel,
    native: NativeSlotModel,
) -> Option<Vec<(FormalIndex, FormalIndex)>> {
    let params = descriptor_params(descriptor)?;
    let mut ports = Vec::with_capacity(2 * params.len() + 3);
    let mut java: i16 = 0;
    if !is_static {
        // The receiver occupies slot 0 on both Java frontends, and arrives as the `jobject`.
        ports.push((FormalIndex::new(0), FormalIndex::new(1)));
        java = 1;
    }
    let mut next: i16 = 2;
    for p in params {
        for _ in 0..native.width(p) {
            ports.push((FormalIndex::new(java), FormalIndex::new(next)));
            next += 1;
        }
        java += slots.width(p);
    }
    ports.push((RETURN_INDEX.into(), RETURN_INDEX.into()));
    // Globals ride through the synthetic site exactly as they do at a real call site, so a native
    // implementation writing a global is visible to Java and vice versa.
    ports.push((GLOBALS_INDEX.into(), GLOBALS_INDEX.into()));
    Some(ports)
}

// ---------------------------------------------------------------------------
// Observation
// ---------------------------------------------------------------------------

/// One Java method declared `native`, as observed from an import's VMT.
#[derive(Debug, Clone, Eq, PartialEq)]
struct JavaNative {
    /// Fully-qualified IR name of the Java stub, e.g.
    /// `Lcom/example/Crypto;->encrypt(Ljava/lang/String;)Ljava/lang/String;`.
    method: String,
    /// Internal class name (`com/example/Crypto`), ready for the mangler.
    class_internal: String,
    /// Simple method name (`encrypt`).
    simple_name: String,
    /// Method descriptor (`(Ljava/lang/String;)Ljava/lang/String;`).
    descriptor: String,
    is_static: bool,
    slots: SlotModel,
}

/// Collects, across every import of a project, the two halves the bridge has to join: the Java
/// `native` methods and the native symbol table.
///
/// It holds owned strings rather than [`FunctionId`]s because it runs *before* codegen has interned
/// either side -- only after the whole import loop does one [`crate::facts::IdMap`] contain both
/// programs' functions.
#[derive(Default, Debug)]
pub struct JniObserver {
    natives: Vec<JavaNative>,
    /// Native simple name -> the native function(s) carrying it.
    symbols: BTreeMap<String, Vec<NativeTarget>>,
    /// One entry per import that shipped a `jni-registry.json`: its name, and the tables
    /// recovered from it.
    ///
    /// Kept per import, unlike `natives` and `symbols`, because `table_addr` order is only
    /// meaningful within one library -- and run segmentation is the whole of attribution.
    registries: Vec<(String, registry::JniRegistry)>,
}

impl JniObserver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one import of the project being indexed: both its Java `native` methods and its
    /// native symbol table, whichever it has. Call it per import, before `codegen_program`
    /// consumes the [`ProgramInfo`]. `slots` describes how *this* frontend numbers parameters
    /// (see [`SlotModel::for_language`]), and `abi` what a native import was built for (see
    /// [`NativeAbi::of`]).
    pub fn observe(&mut self, program_info: &ProgramInfo, slots: SlotModel, abi: NativeAbi) {
        self.observe_java(&program_info.vmt, slots);
        self.observe_native_vmt(&program_info.vmt, abi, Origin::Current);
    }

    /// Records the Java half of one import: the methods it declares `native`. Only imports of
    /// the project being indexed contribute these, since only their stubs are in the fact base.
    pub fn observe_java(&mut self, vmt: &VirtualMethodTable, slots: SlotModel) {
        let VirtualMethodTable::Java { natives, .. } = vmt else {
            return;
        };
        for (cls, name, sig, method, is_static) in natives {
            let (cls, name, sig, method): (&str, &str, &str, &str) = (cls, name, sig, method);
            self.natives.push(JavaNative {
                method: method.to_string(),
                class_internal: internal_class_name(cls).to_string(),
                simple_name: name.to_string(),
                descriptor: sig.to_string(),
                is_static: *is_static,
                slots,
            });
        }
    }

    /// Records the native half of one import: its symbol table. `origin` says whether the import
    /// belongs to this project or to a `--summary` project.
    ///
    /// Call it at most once per import. A second call puts two targets under every symbol, and
    /// the bridge then reports each as ambiguous.
    pub fn observe_native_vmt(&mut self, vmt: &VirtualMethodTable, abi: NativeAbi, origin: Origin) {
        let VirtualMethodTable::Native { methods } = vmt else {
            return;
        };
        // Match against the *simple* name, not the IR function name: the pcode frontend
        // decorates the latter (uniquing suffixes, `<EXTERNAL>::sym@addr`) and already strips
        // the leading underscore Mach-O prefixes every C symbol with.
        for (simple, sig, func, _qualified) in methods {
            let (simple, sig, func): (&str, &str, &str) = (simple, sig, func);
            self.symbols
                .entry(simple.to_string())
                .or_default()
                .push(NativeTarget {
                    function: func.to_string(),
                    proto: NativeProto::parse(sig),
                    abi,
                    origin: origin.clone(),
                });
        }
    }

    /// Records one import's recovered `RegisterNatives` tables, if it has any. Call it per
    /// import, beside [`Self::observe`] or [`Self::observe_native_vmt`].
    ///
    /// # Errors
    ///
    /// If the import has a `jni-registry.json` that cannot be read or parsed. A missing one is
    /// not an error: only an ELF import scanned by this build has one at all.
    pub fn observe_registry(&mut self, import: &ArtifactImport) -> Result<(), Error> {
        let Some(registry) = registry::JniRegistry::load(import)? else {
            return Ok(());
        };
        if registry.entries.is_empty() {
            return Ok(());
        }
        self.registries.push((import.name.clone(), registry));
        Ok(())
    }

    /// True when there is no boundary to bridge: no import contributed a Java `native` method, or
    /// none contributed a native half -- a symbol table *or* a recovered `RegisterNatives` table.
    ///
    /// The registry half counts on its own. A library that exports not one `Java_…` symbol and
    /// binds all 28 of its natives through `RegisterNatives` is the ordinary case, not an exotic
    /// one, and treating it as "nothing to link" would skip exactly the apps this exists for.
    pub fn is_empty(&self) -> bool {
        self.natives.is_empty() || (self.symbols.is_empty() && self.registries.is_empty())
    }
}

// ---------------------------------------------------------------------------
// Linking
// ---------------------------------------------------------------------------

/// What [`link`] did, for the `info` line. A missed link produces no flow *and* no error, so these
/// counts are the only signal that the bridge fired at all.
#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct LinkStats {
    /// Java methods declared `native` across all imports.
    pub natives: usize,
    /// Of those, ones joined to a native implementation.
    pub linked: usize,
    /// Of those linked, ones joined through a recovered `RegisterNatives` table rather than
    /// through a `Java_…` symbol. A subset of `linked`, not an addition to it.
    pub registered: usize,
    /// Ones with no matching `Java_…` symbol (or whose two halves were not both in the fact base).
    pub unresolved: usize,
    /// Ones whose only candidate was an ambiguous short name.
    pub ambiguous: usize,
    /// Recovered `RegisterNatives` entries that tier 1 could not attribute to a single class.
    /// Not a subset of anything above: it counts table entries, not Java methods.
    pub unattributed: usize,
    /// Of those linked, ones whose implementation came from a `--summary` project rather than
    /// from an import of this one. A subset of `linked`.
    pub from_summary: usize,
    /// Of those linked, ones whose recovered native prototype disagrees with what the Dex
    /// descriptor implies. A subset of `linked`: the link is emitted anyway.
    pub prototype_mismatch: usize,
}

impl std::fmt::Display for LinkStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} native method(s): {} linked ({} registered, {} from summary, {} prototype \
             mismatch), {} unresolved, {} ambiguous",
            self.natives,
            self.linked,
            self.registered,
            self.from_summary,
            self.prototype_mismatch,
            self.unresolved,
            self.ambiguous
        )
    }
}

/// What [`link`] did: the counts, and each native function it linked a method to.
#[derive(Debug, Default, Clone)]
pub struct LinkOutcome {
    pub stats: LinkStats,
    /// Every native function at least one method was linked to, once each, with where it was
    /// observed.
    pub targets: BTreeMap<FunctionId, NativeTarget>,
}

/// How a Java native method resolved against the native half.
enum Resolution<'a> {
    /// The mangled symbol (or, for a registered native, its registered name), and the native
    /// function it belongs to.
    Found {
        symbol: String,
        target: &'a NativeTarget,
        /// True when this came from a recovered `RegisterNatives` table.
        registered: bool,
    },
    /// A short name matched but could not be attributed to one method.
    Ambiguous {
        symbol: String,
        reason: String,
    },
    NotFound,
}

/// Joins every observed Java `native` method to its implementation, emitting the `call`,
/// `actual_param` and `formal_param` rows that make taint cross the boundary.
///
/// Call it after the import loop and before the facts are saved: that is the first point at which
/// both programs' functions live in one [`crate::facts::IdMap`]. The Java stub has to be in the
/// fact base already. The native function is interned if it is not: that is the case for a
/// target observed only in a `--summary` project, whose code this index never loads.
pub fn link(
    obs: &JniObserver,
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
) -> LinkOutcome {
    let mut outcome = LinkOutcome::default();
    if obs.natives.is_empty() {
        return outcome;
    }
    let stats = &mut outcome.stats;

    // Two imports can declare the same method (an app and a library jar, say). One bridge is
    // enough -- both spellings intern to the same `FunctionId` -- and deduplicating here also
    // keeps the overload count below from mistaking a re-observation for a second overload. The
    // first observation wins, so a method seen through two frontends keeps the first one's slot
    // model; the two agree except on `long`/`double` parameters.
    let mut seen: HashSet<&str> = HashSet::new();
    let natives: Vec<&JavaNative> = obs
        .natives
        .iter()
        .filter(|nat| seen.insert(nat.method.as_str()))
        .collect();

    // (class, simple name) -> how many native methods share it. A short name can only be
    // attributed to one of an overload set.
    let mut overloads: HashMap<(&str, &str), usize> = HashMap::new();
    for nat in &natives {
        *overloads
            .entry((nat.class_internal.as_str(), nat.simple_name.as_str()))
            .or_default() += 1;
    }

    // IR function name -> its target, for the registry tier, whose tables name functions rather
    // than symbols. The first observation wins: the project's own imports are observed before
    // any summary project's.
    let mut by_function: HashMap<&str, &NativeTarget> = HashMap::new();
    for target in obs.symbols.values().flatten() {
        by_function
            .entry(target.function.as_str())
            .or_insert(target);
    }
    let registered = attribute_registries(obs, &natives, &by_function, stats);

    for nat in natives {
        stats.natives += 1;

        let (target, via_registry) = match resolve(nat, &obs.symbols, &overloads, &registered) {
            Resolution::Found {
                symbol,
                target,
                registered,
            } => {
                log::debug!(
                    "jni bridge: {} -> {} ({}{})",
                    nat.method,
                    target.function,
                    if registered { "registered as " } else { "" },
                    symbol
                );
                (target, registered)
            }
            Resolution::Ambiguous { symbol, reason } => {
                log::warn!(
                    "jni bridge: not linking '{}': symbol '{}' is ambiguous ({}). \
                     Give the implementation its long (descriptor-qualified) name to \
                     disambiguate, or bind it with RegisterNatives, which names the method \
                     unambiguously.",
                    nat.method,
                    symbol,
                    reason
                );
                stats.ambiguous += 1;
                continue;
            }
            Resolution::NotFound => {
                log::debug!(
                    "jni bridge: no implementation found for '{}' (looked for '{}')",
                    nat.method,
                    short_name(&nat.class_internal, &nat.simple_name)
                );
                stats.unresolved += 1;
                continue;
            }
        };

        let function = target.function.as_str();
        // The stub must already be interned; it is, unless its import was dropped.
        let Some(java_id) = source_info
            .sites
            .get_function_id(facts::Function(nat.method.as_str().into()))
        else {
            log::debug!("jni bridge: '{}' is not in the fact base", nat.method);
            stats.unresolved += 1;
            continue;
        };

        let Some(params) = descriptor_params(&nat.descriptor) else {
            log::warn!(
                "jni bridge: not linking '{}': malformed descriptor '{}'",
                nat.method,
                nat.descriptor
            );
            stats.unresolved += 1;
            continue;
        };
        // Reads the count the frontend recovered off the target's VMT signature rather than off
        // `formal_param`, so a summary-only target, which has no formals here, is checked
        // exactly like a co-indexed one.
        let (layout, check) = choose_layout(&params, target.proto, target.abi);
        if layout == NativeSlotModel::SplitWide {
            log::debug!(
                "jni bridge: '{}' -> '{}': {:?} ABI, prototype has each long/double split in \
                 two; using SplitWide",
                nat.method,
                function,
                target.abi
            );
        }
        if warn_on_proto_check(&nat.method, function, &check) {
            stats.prototype_mismatch += 1;
        }
        let ports = port_map(&nat.descriptor, nat.is_static, nat.slots, layout)
            .expect("the descriptor parsed above");
        let native_id = source_info
            .sites
            .get_or_add_function(facts::Function(function.into()));

        emit_bridge(java_id, native_id, &ports, facts, source_info);
        stats.linked += 1;
        if via_registry {
            stats.registered += 1;
        }
        if matches!(target.origin, Origin::Summary(_)) {
            stats.from_summary += 1;
        }
        outcome
            .targets
            .entry(native_id)
            .or_insert_with(|| target.clone());
    }

    log::info!("jni bridge: {}", stats);
    outcome
}

/// Warns about a prototype that does not fit its Dex descriptor, and says whether it did. The
/// link is emitted either way.
fn warn_on_proto_check(method: &str, function: &str, check: &ProtoCheck) -> bool {
    match check {
        ProtoCheck::Match => return false,
        ProtoCheck::NoPrototype => log::warn!(
            "jni bridge: '{method}' resolves to '{function}', for which the disassembler \
             recovered no prototype, so no argument and no return value will flow. Build the \
             library with -g (or otherwise give Ghidra its types) and re-import"
        ),
        ProtoCheck::TooFew {
            recovered,
            expected,
        } => log::warn!(
            "jni bridge: '{method}' resolves to '{function}', which has {recovered} recovered \
             parameter(s) but needs {expected}; the prototype is incomplete, so the argument(s) \
             above index {} will not flow",
            recovered.saturating_sub(1)
        ),
        ProtoCheck::TooMany {
            recovered,
            typed,
            split,
        } => log::warn!(
            "jni bridge: '{method}' resolves to '{function}', whose recovered prototype has \
             {recovered} parameter(s) where the Dex descriptor implies {typed}{}; arguments may \
             be mis-slotted",
            split.map_or(String::new(), |split| format!(
                " ({split} with each long/double split in two)"
            ))
        ),
    }
    true
}

/// Runs tier-1 attribution over every import's recovered tables and returns the resulting
/// `Java method -> IR function` pairings, which [`resolve`] consults as tier 0.
///
/// The table side is per import; the Java candidate side spans the project. That asymmetry is
/// what makes a split APK work: in an app bundle the `.so` and the `classes.dex` are different
/// imports, so an attribution scoped to one import on both sides would link nothing.
fn attribute_registries<'a>(
    obs: &'a JniObserver,
    natives: &[&'a JavaNative],
    by_function: &HashMap<&str, &'a NativeTarget>,
    stats: &mut LinkStats,
) -> HashMap<&'a str, &'a NativeTarget> {
    let mut links: HashMap<&'a str, &'a NativeTarget> = HashMap::new();
    if obs.registries.is_empty() {
        return links;
    }

    let index = registry::ClassIndex::build(natives.iter().map(|nat| {
        (
            nat.class_internal.as_str(),
            nat.simple_name.as_str(),
            nat.descriptor.as_str(),
        )
    }));
    // (class, simple name, descriptor) -> the Java stub, so an attributed entry names a method
    // the bridge can emit against.
    let methods: HashMap<(&str, &str, &str), &'a JavaNative> = natives
        .iter()
        .map(|nat| {
            (
                (
                    nat.class_internal.as_str(),
                    nat.simple_name.as_str(),
                    nat.descriptor.as_str(),
                ),
                *nat,
            )
        })
        .collect();

    let (mut entries, mut attributed) = (0usize, 0usize);
    for (import_name, reg) in &obs.registries {
        let report = registry::attribute(reg, &index);
        let mut classes: HashSet<&str> = HashSet::new();
        for hit in &report.attributed {
            classes.insert(hit.class);
            let key = (
                hit.class,
                hit.entry.name.as_str(),
                hit.entry.descriptor.as_str(),
            );
            // An entry with no function is still attributed and still counted: it is the
            // disassembler, not the scan, that came up empty.
            let (Some(nat), Some(function)) = (methods.get(&key), hit.entry.function.as_deref())
            else {
                continue;
            };
            // The scan names a function out of the same import's symbol table, so this finds
            // it unless that import's VMT was not observed.
            let Some(target) = by_function.get(function).copied() else {
                log::debug!(
                    "jni registry: '{}' is registered to '{}', which no observed symbol table \
                     has",
                    nat.method,
                    function,
                );
                continue;
            };
            if let Some(previous) = links.insert(nat.method.as_str(), target)
                && previous.function != target.function
            {
                log::warn!(
                    "jni registry: '{}' is registered twice, to '{}' and '{}'; keeping the \
                     latter",
                    nat.method,
                    previous.function,
                    target.function,
                );
            }
        }
        // Only libraries that have tables: a per-library line for the hundreds that do not is
        // noise, and a config split can hold two hundred of them.
        log::info!(
            "jni registry: {} table(s), {} entr{} in {}: {} attributed to {} class(es), {} \
             unattributed",
            report.tables,
            reg.entries.len(),
            if reg.entries.len() == 1 { "y" } else { "ies" },
            import_name,
            report.attributed.len(),
            classes.len(),
            report.unattributed,
        );
        entries += reg.entries.len();
        attributed += report.attributed.len();
        stats.unattributed += report.unattributed;
    }
    log::info!(
        "jni registry: {} entr{} across {} librar{}: {} attributed, {} unattributed",
        entries,
        if entries == 1 { "y" } else { "ies" },
        obs.registries.len(),
        if obs.registries.len() == 1 {
            "y"
        } else {
            "ies"
        },
        attributed,
        stats.unattributed,
    );
    links
}

/// Resolves one Java native method to its implementation, mirroring the JNI runtime.
///
/// **Tier 0** is a `RegisterNatives` binding recovered by [`registry`]. It wins outright, because
/// that is what the runtime does: a registered method runs the registered function whether or not
/// a matching `Java_…` symbol exists. It also has to be consulted *before* the symbol tiers rather
/// than as a fallback -- the `Ambiguous` arm below never reaches a fallback, and an overloaded
/// native is exactly the case `RegisterNatives` matters most for.
///
/// Otherwise the symbol convention: prefer the long (descriptor-qualified) name when that symbol
/// exists, otherwise fall back to the short name -- but only when the declaring class has exactly
/// one native method with that simple name, since an overloaded native reached by its short name
/// cannot be attributed.
fn resolve<'a>(
    nat: &JavaNative,
    symbols: &'a BTreeMap<String, Vec<NativeTarget>>,
    overloads: &HashMap<(&str, &str), usize>,
    registered: &HashMap<&'a str, &'a NativeTarget>,
) -> Resolution<'a> {
    if let Some(target) = registered.get(nat.method.as_str()).copied() {
        // Resolve the symbol side too, purely to notice a disagreement. `resolve` must still
        // return exactly one answer: `emit_bridge` mints a *fresh* site per call, so returning
        // both would double-bridge the method.
        if let Resolution::Found {
            target: by_symbol,
            symbol,
            ..
        } = resolve_by_symbol(nat, symbols, overloads)
            && by_symbol.function != target.function
        {
            log::warn!(
                "jni bridge: '{}' is registered to '{}' but symbol '{}' names '{}'; using the \
                 registration, which is what the runtime does",
                nat.method,
                target.function,
                symbol,
                by_symbol.function,
            );
        }
        return Resolution::Found {
            symbol: nat.simple_name.clone(),
            target,
            registered: true,
        };
    }
    resolve_by_symbol(nat, symbols, overloads)
}

/// Tiers 1 and 2: the JNI name-mangling convention. See [`resolve`].
fn resolve_by_symbol<'a>(
    nat: &JavaNative,
    symbols: &'a BTreeMap<String, Vec<NativeTarget>>,
    overloads: &HashMap<(&str, &str), usize>,
) -> Resolution<'a> {
    let unique = |symbol: String, candidates: &'a [NativeTarget]| match candidates {
        [only] => Resolution::Found {
            symbol,
            target: only,
            registered: false,
        },
        many => Resolution::Ambiguous {
            symbol,
            reason: format!("{} native functions carry that name", many.len()),
        },
    };

    if let Some(descriptor) = param_descriptor(&nat.descriptor) {
        let long = long_name(&nat.class_internal, &nat.simple_name, &descriptor);
        if let Some(candidates) = symbols.get(&long) {
            return unique(long, candidates.as_slice());
        }
    }

    let short = short_name(&nat.class_internal, &nat.simple_name);
    let Some(candidates) = symbols.get(&short) else {
        return Resolution::NotFound;
    };
    let overloaded = overloads
        .get(&(nat.class_internal.as_str(), nat.simple_name.as_str()))
        .copied()
        .unwrap_or(1);
    if overloaded > 1 {
        return Resolution::Ambiguous {
            symbol: short,
            reason: format!("{overloaded} native overloads of '{}'", nat.simple_name),
        };
    }
    unique(short, candidates.as_slice())
}

/// Emits the facts for one bridge: a fresh call site inside the Java stub targeting the native
/// implementation, one `actual_param` per port, and the `formal_param` rows the summary rule needs
/// on the Java side.
fn emit_bridge(
    java_id: FunctionId,
    native_id: FunctionId,
    ports: &[(FormalIndex, FormalIndex)],
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
) {
    // A *fresh* site: call-arg pseudo-variables key on the instruction id, so reusing an existing
    // site would alias its argument n to the bridge's argument n.
    let site = source_info.add_insn_site(java_id);
    let site: PackedInsnSiteId = site.try_into().expect("packing a fresh JNI bridge site");
    facts.call.push((site, native_id));
    for (java_index, native_index) in ports {
        facts.actual_param.push((
            site,
            *native_index,
            FlowVertex(
                FlowVariable::formal_index(*java_index),
                facts::Path::empty(),
            ),
        ));
        // The Java stub is bodyless, so nothing else declares these. Without them `locals` is
        // never seeded and the stub derives no summary of its own.
        facts.formal_param.push((
            java_id,
            FlowVariable::formal_index(*java_index),
            FormalType::ByRef,
        ));
    }
}

#[cfg(test)]
mod tests;
