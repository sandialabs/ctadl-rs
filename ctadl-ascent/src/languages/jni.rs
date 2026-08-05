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

# Two ways a native method finds its implementation

The symbol convention above is one of them, and the only one a JVM applies on its own. The other
is `env->RegisterNatives(clazz, table, n)`, which an app calls from `JNI_OnLoad` to bind a
`JNINativeMethod[]` at run time -- name, descriptor and function pointer, with no exported symbol
anywhere. Most Android apps use it for most of their natives.

[`registry`] recovers those tables straight out of the library's data sections at import time and
recovers each entry's declaring class from the Dex side at index time. [`resolve`] consults that
result *first*, because it is what the runtime does: a method bound by `RegisterNatives` runs the
registered function even when a matching `Java_…` symbol also exists.

# Limitations

Calls through the `JNIEnv` accessor vtable (`(*env)->GetStringUTFChars(...)`) are not modelled.
See `docs/jni.md`.
*/

use hashbrown::hash_map::HashMap;
use hashbrown::hash_set::HashSet;
use std::collections::BTreeMap;

use ctadl_ir::ProgramInfo;
use ctadl_ir::mir::call::VirtualMethodTable;

pub mod registry;

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

/// Splits a method descriptor's parameter list into individual type descriptors:
/// `(Ljava/lang/String;[IJ)V` yields `["Ljava/lang/String;", "[I", "J"]`. Returns `None` if the
/// descriptor is malformed.
///
/// The scan stops at the `)` that terminates the parameter list rather than at the first `)` in the
/// string, because the JVM's unqualified-name grammar forbids only `.;[/` inside a class name.
pub fn descriptor_params(descriptor: &str) -> Option<Vec<&str>> {
    let inner = descriptor.strip_prefix('(')?;
    let bytes = inner.as_bytes();
    let mut params = Vec::new();
    let mut i = 0;
    loop {
        if *bytes.get(i)? == b')' {
            return Some(params);
        }
        let start = i;
        while *bytes.get(i)? == b'[' {
            i += 1;
        }
        match *bytes.get(i)? {
            b'L' => {
                i += 1;
                // A multi-byte UTF-8 sequence never contains a byte equal to `;`, so this both
                // terminates correctly and leaves `i` on a char boundary.
                while *bytes.get(i)? != b';' {
                    i += 1;
                }
                i += 1;
            }
            b'Z' | b'B' | b'C' | b'S' | b'I' | b'J' | b'F' | b'D' => i += 1,
            _ => return None,
        }
        params.push(&inner[start..i]);
    }
}

/// The parameter descriptor the long name mangles: the method descriptor with its parentheses and
/// return type stripped.
pub fn param_descriptor(descriptor: &str) -> Option<String> {
    Some(descriptor_params(descriptor)?.concat())
}

/// The `(java_index, native_index)` port pairs for one native method, including `this`, the return
/// value and the globals pseudo-parameter.
///
/// The native side is fixed by the JNI ABI: index 0 is `JNIEnv *` (never mapped), index 1 is the
/// receiver `jobject` for an instance method or the declaring `jclass` for a static one, and
/// declared parameter *k* lands at `2 + k`. The Java side depends on `slots`. Only `-1` is mapped
/// for returns: a Java function has return arity 2 (`-1` normal, `-2` exception) while a native
/// function has one.
///
/// Returns `None` if `descriptor` is not a well-formed method descriptor.
pub fn port_map(
    descriptor: &str,
    is_static: bool,
    slots: SlotModel,
) -> Option<Vec<(FormalIndex, FormalIndex)>> {
    let params = descriptor_params(descriptor)?;
    let mut ports = Vec::with_capacity(params.len() + 3);
    let mut java: i16 = 0;
    if !is_static {
        // The receiver occupies slot 0 on both Java frontends, and arrives as the `jobject`.
        ports.push((FormalIndex::new(0), FormalIndex::new(1)));
        java = 1;
    }
    let mut native: i16 = 2;
    for p in params {
        ports.push((FormalIndex::new(java), FormalIndex::new(native)));
        java += slots.width(p);
        native += 1;
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
    /// Native simple name -> the fully-qualified IR function name(s) carrying it.
    symbols: BTreeMap<String, Vec<String>>,
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

    /// Records one import's contribution. Call it per import, before `codegen_program` consumes the
    /// [`ProgramInfo`]. `slots` describes how *this* frontend numbers parameters; see
    /// [`SlotModel::for_language`].
    pub fn observe(&mut self, program_info: &ProgramInfo, slots: SlotModel) {
        match &program_info.vmt {
            VirtualMethodTable::Java { natives, .. } => {
                for (cls, name, sig, method, is_static) in natives {
                    let (cls, name, sig, method): (&str, &str, &str, &str) =
                        (cls, name, sig, method);
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
            VirtualMethodTable::Native { methods } => {
                // Match against the *simple* name, not the IR function name: the pcode frontend
                // decorates the latter (uniquing suffixes, `<EXTERNAL>::sym@addr`) and already
                // strips the leading underscore Mach-O prefixes every C symbol with.
                for (simple, _sig, func, _qualified) in methods {
                    let (simple, func): (&str, &str) = (simple, func);
                    self.symbols
                        .entry(simple.to_string())
                        .or_default()
                        .push(func.to_string());
                }
            }
            VirtualMethodTable::Lua { .. } | VirtualMethodTable::Unknown => {}
        }
    }

    /// Records one import's recovered `RegisterNatives` tables, if it has any. Call it per
    /// import, beside [`Self::observe`].
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
}

impl std::fmt::Display for LinkStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} native method(s): {} linked ({} registered), {} unresolved, {} ambiguous",
            self.natives, self.linked, self.registered, self.unresolved, self.ambiguous
        )
    }
}

/// How a Java native method resolved against the native half.
enum Resolution<'a> {
    /// The mangled symbol (or, for a registered native, its registered name), and the IR
    /// function name it belongs to.
    Found {
        symbol: String,
        function: &'a str,
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
/// both programs' functions live in one [`crate::facts::IdMap`].
pub fn link(
    obs: &JniObserver,
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
) -> LinkStats {
    let mut stats = LinkStats::default();
    if obs.natives.is_empty() {
        return stats;
    }

    // Arity of each function *before* the bridge adds formals, so the diagnostic below reports
    // what the frontend recovered rather than what this pass just synthesized.
    let num_params = facts.compute_num_params();

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

    let registered = attribute_registries(obs, &natives, &mut stats);

    for nat in natives {
        stats.natives += 1;

        let (function, via_registry) = match resolve(nat, &obs.symbols, &overloads, &registered) {
            Resolution::Found {
                symbol,
                function,
                registered,
            } => {
                log::debug!(
                    "jni bridge: {} -> {} ({}{})",
                    nat.method,
                    function,
                    if registered { "registered as " } else { "" },
                    symbol
                );
                (function, registered)
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

        // Both sides must already be interned; they are, unless an import was dropped.
        let (Some(java_id), Some(native_id)) = (
            source_info
                .sites
                .get_function_id(facts::Function(nat.method.as_str().into())),
            source_info
                .sites
                .get_function_id(facts::Function(function.into())),
        ) else {
            log::debug!(
                "jni bridge: '{}' or '{}' is not in the fact base",
                nat.method,
                function
            );
            stats.unresolved += 1;
            continue;
        };

        let Some(ports) = port_map(&nat.descriptor, nat.is_static, nat.slots) else {
            log::warn!(
                "jni bridge: not linking '{}': malformed descriptor '{}'",
                nat.method,
                nat.descriptor
            );
            stats.unresolved += 1;
            continue;
        };

        // An incomplete prototype is the one failure mode that silently drops arguments: Ghidra
        // gives a function with no recovered prototype zero parameters, and a mapped port past its
        // arity then has no formal on the far side to flow into.
        let expected = ports
            .iter()
            .map(|(_, native)| **native)
            .filter(|native| *native >= 0)
            .max()
            .map_or(0, |highest| highest + 1);
        let recovered = num_params.get(&native_id).copied().unwrap_or(0);
        if recovered < expected {
            log::warn!(
                "jni bridge: '{}' resolves to '{}', which has {} recovered parameter(s) but needs \
                 {}; the prototype is incomplete, so some argument(s) will not flow",
                nat.method,
                function,
                recovered,
                expected
            );
        }

        emit_bridge(java_id, native_id, &ports, facts, source_info);
        stats.linked += 1;
        if via_registry {
            stats.registered += 1;
        }
    }

    log::info!("jni bridge: {}", stats);
    stats
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
    stats: &mut LinkStats,
) -> HashMap<&'a str, &'a str> {
    let mut links: HashMap<&'a str, &'a str> = HashMap::new();
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
            if let Some(previous) = links.insert(nat.method.as_str(), function)
                && previous != function
            {
                log::warn!(
                    "jni registry: '{}' is registered twice, to '{}' and '{}'; keeping the \
                     latter",
                    nat.method,
                    previous,
                    function,
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
    symbols: &'a BTreeMap<String, Vec<String>>,
    overloads: &HashMap<(&str, &str), usize>,
    registered: &HashMap<&'a str, &'a str>,
) -> Resolution<'a> {
    if let Some(function) = registered.get(nat.method.as_str()).copied() {
        // Resolve the symbol side too, purely to notice a disagreement. `resolve` must still
        // return exactly one answer: `emit_bridge` mints a *fresh* site per call, so returning
        // both would double-bridge the method.
        if let Resolution::Found {
            function: by_symbol,
            symbol,
            ..
        } = resolve_by_symbol(nat, symbols, overloads)
            && by_symbol != function
        {
            log::warn!(
                "jni bridge: '{}' is registered to '{}' but symbol '{}' names '{}'; using the \
                 registration, which is what the runtime does",
                nat.method,
                function,
                symbol,
                by_symbol,
            );
        }
        return Resolution::Found {
            symbol: nat.simple_name.clone(),
            function,
            registered: true,
        };
    }
    resolve_by_symbol(nat, symbols, overloads)
}

/// Tiers 1 and 2: the JNI name-mangling convention. See [`resolve`].
fn resolve_by_symbol<'a>(
    nat: &JavaNative,
    symbols: &'a BTreeMap<String, Vec<String>>,
    overloads: &HashMap<(&str, &str), usize>,
) -> Resolution<'a> {
    let unique = |symbol: String, candidates: &'a [String]| match candidates {
        [only] => Resolution::Found {
            symbol,
            function: only.as_str(),
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
