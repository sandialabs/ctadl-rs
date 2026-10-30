/*! The per-import program view the model DSL's relations are built from.

One import's method table, its functions' IR, which import it is (for the `language` and
`import` attributes), and, when some rule reads `callsig`, the signature keys its call sites
name. [`super::dsl::relations::ProgramFacts`] is built from it.

# Why it borrows the program

The index holds `&'p str` into the [`ProgramInfo`] rather than owned strings, because under the
streaming posture it never outlives one. Each import builds an index, every rule is evaluated
against it, the *matches* are copied into
[`ProgramModelMatches`](super::matches::ProgramModelMatches) as owned data, and the index and the
IR are both dropped before the next import is loaded. Owning the strings would buy retention
across the import loop, which is exactly what streaming exists not to need.
*/

use std::collections::BTreeMap;

use hashbrown::hash_map::HashMap;

use ctadl_ir::ProgramInfo;
use ctadl_ir::mir::FunctionData;
use ctadl_ir::mir::call::{Dispatch, VirtualMethodTable};

use super::dsl::relations::FunKey;
use super::spec::ImportScope;

/// One program's matchable metadata, keyed the way each frontend spells its functions.
pub struct ProgramMatchIndex<'p> {
    /// Which import this is, for [`super::spec::ProgramScope::admits`].
    pub scope: ImportScope,
    pub(crate) vmt: &'p VirtualMethodTable,
    /// fq-name (== `FunctionData.name`) → the function's IR data: arity, body, parameters and
    /// the fields it uses. A function with no IR body (a Lua external, a dex/jvm `ext` stub) has
    /// no entry.
    pub(crate) program_functions: HashMap<&'p str, &'p FunctionData>,
    /// What `callsig` ranges over: the distinct static signatures the program's Java and CIL
    /// call sites name. `None` unless some loaded rule asked for them, because collecting them
    /// costs a pass over every statement.
    ///
    /// Unlike the method table, this contains types the app never declares. The table is built
    /// from implementations, and about half of an app's interface sites name a type that is not
    /// among them -- `java.util.Iterator` has no row there, so `fun` cannot name it. The keys of
    /// the call sites do, by construction.
    pub(crate) dispatch: Option<DispatchUniverse<'p>>,
}

/// The distinct signature keys of a program's Java and CIL call sites, owned.
///
/// Separate from [`ProgramMatchIndex`] because the index borrows every string it matches on and
/// these are composed rather than read out of the IR. A caller builds this beside the
/// `ProgramInfo` and hands it to [`ProgramMatchIndex::new_with_dispatch`].
#[derive(Default, Debug)]
pub struct DispatchKeys {
    /// `Lcls;->name(desc)` per distinct Java key and `[Asm]Ns.Type::name(desc)->ret` per CIL
    /// key -- the spelling the frontend's own method ids use, so `qualified-id` matching works
    /// unchanged.
    ids: Vec<String>,
    /// The three components of each key, parallel to [`Self::ids`].
    parts: Vec<(String, String, String)>,
}

impl DispatchKeys {
    /// Collects every distinct `(cls, simple_name, descriptor)` a `JavaCall` or a `CilCall` in
    /// `program` names. Sorted, so the matching that reads it does not depend on statement
    /// order.
    pub fn from_program(program: &ctadl_ir::mir::Program) -> Self {
        use ctadl_ir::mir::StatementKind;
        use ctadl_ir::mir::call::CallStyle;
        // Key -> the separator its method id puts between class and name.
        let mut keys: BTreeMap<(String, String, String), &'static str> = BTreeMap::new();
        for func in program.functions.iter() {
            for block in func.blocks.iter() {
                for stmt in block.statements.iter() {
                    let StatementKind::CallAssign { style, .. } = &stmt.kind else {
                        continue;
                    };
                    let (cls, simple_name, descriptor, sep) = match style {
                        CallStyle::JavaCall {
                            cls,
                            simple_name,
                            descriptor,
                            ..
                        } => (cls, simple_name, descriptor, "->"),
                        CallStyle::CilCall {
                            cls,
                            simple_name,
                            descriptor,
                            ..
                        } => (cls, simple_name, descriptor, "::"),
                        _ => continue,
                    };
                    keys.insert(
                        (
                            cls.to_string(),
                            simple_name.to_string(),
                            descriptor.to_string(),
                        ),
                        sep,
                    );
                }
            }
        }
        let ids = keys
            .iter()
            .map(|((cls, name, desc), sep)| format!("{cls}{sep}{name}{desc}"))
            .collect();
        Self {
            ids,
            parts: keys.into_keys().collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }
}

/// The signature keys of a program's Java and CIL call sites, as `callsig` reads them.
pub(crate) struct DispatchUniverse<'p> {
    /// Key id -> its `(cls, simple_name, descriptor)`, so a match can be turned back into the
    /// triple codegen keys on.
    pub(crate) parts: HashMap<&'p str, (&'p str, &'p str, &'p str)>,
}

impl<'p> DispatchUniverse<'p> {
    fn new(keys: &'p DispatchKeys) -> Self {
        let parts = keys
            .ids
            .iter()
            .zip(keys.parts.iter())
            .map(|(id, (cls, name, desc))| {
                (id.as_str(), (cls.as_str(), name.as_str(), desc.as_str()))
            })
            .collect();
        Self { parts }
    }
}

impl<'p> ProgramMatchIndex<'p> {
    /// Indexes `program_info`'s metadata for matching. `scope` identifies the import it came
    /// from; pass [`ImportScope::unknown`] when there is none.
    pub fn new(program_info: &'p ProgramInfo, scope: ImportScope) -> Self {
        Self::new_with_dispatch(program_info, scope, None)
    }

    /// [`Self::new`], plus the signature keys `callsig` ranges over. Pass `None` when no loaded
    /// rule reads `callsig`; it is then empty rather than costing a pass over every statement.
    pub fn new_with_dispatch(
        program_info: &'p ProgramInfo,
        scope: ImportScope,
        dispatch_keys: Option<&'p DispatchKeys>,
    ) -> Self {
        let vmt = &program_info.vmt;
        // Index every IR function by its fq-name for the arity / body / field attributes.
        let mut program_functions: HashMap<&'p str, &'p FunctionData> = HashMap::new();
        for func in &program_info.program.functions.functions {
            program_functions.entry(func.name.as_str()).or_insert(func);
        }

        Self {
            scope,
            vmt,
            program_functions,
            dispatch: dispatch_keys.map(DispatchUniverse::new),
        }
    }

    /// The virtual method table this index was built from.
    #[inline]
    pub fn vmt(&self) -> &'p VirtualMethodTable {
        self.vmt
    }

    /// Every function with IR data, as `(fq-name, data)`.
    ///
    /// The VMT names more functions than this — externals and `ext` stubs have no body — so
    /// this is the *arity / has_code / body* half of the program, not its function universe.
    /// The DSL relation layer joins the two; see [`super::dsl::relations::ProgramFacts`].
    #[inline]
    pub fn functions_with_data(&self) -> impl Iterator<Item = (&'p str, &'p FunctionData)> + '_ {
        self.program_functions.iter().map(|(fq, data)| (*fq, *data))
    }
}

/// Every spelling `vmt` publishes for its functions, as `(fq, attribute, spelling)`.
///
/// The one statement of what each frontend makes matchable, read by the DSL's `fun` relation
/// ([`ProgramFacts`](super::dsl::relations::ProgramFacts)). Spellings come in the frontend's
/// order, and within one function the canonical spelling of an attribute first: the DSL binds an
/// output attribute to the first.
///
/// Lua's metatable classes are reported as [`FunKey::Parent`]; a [`Dispatch::Unknown`]
/// table publishes nothing.
pub(crate) fn for_each_spelling<'p>(
    vmt: &'p VirtualMethodTable,
    mut f: impl FnMut(&'p str, FunKey, &'p str),
) {
    match &vmt.dispatch {
        Dispatch::Java { methods, .. } => {
            // The `JavaMethod` id, e.g. `Lcom/example/Foo;->bar(I)V`, is the qualified id:
            // descriptor-bearing and stable, so keying on it is what makes exact
            // fully-qualified matching possible on jvm/dex.
            for (cls, name, sig, fid) in methods {
                let fid = fid.as_ref();
                f(fid, FunKey::Name, name.as_ref());
                f(fid, FunKey::Parent, cls.as_ref());
                f(fid, FunKey::Signature, sig.as_ref());
                f(fid, FunKey::QualifiedId, fid);
            }
        }
        Dispatch::Cil { methods, .. } => {
            // The Java arm, over .NET spellings: `[Asm]Ns.Type`, `Parse`,
            // `(System.String)->System.Int32`, and the method id
            // `[Asm]Ns.Type::Parse(System.String)->System.Int32`. An override alias row makes a
            // method matchable under the base method's name and descriptor too, which is what a
            // model of an interface method wants.
            for (cls, name, sig, fid) in methods {
                let fid = fid.as_ref();
                f(fid, FunKey::Name, name.as_ref());
                f(fid, FunKey::Parent, cls.as_ref());
                f(fid, FunKey::Signature, sig.as_ref());
                f(fid, FunKey::QualifiedId, fid);
            }
        }
        Dispatch::Native { methods } => {
            // Native frontends (pcode, clang) carry, per function, a simple (un-decorated) name
            // and a best-effort type signature alongside the fully-qualified IR name. The simple
            // name comes first, so a pattern like `^system$` resolves even when the IR name is
            // decorated (Ghidra's `<EXTERNAL>::system@00101008`); the fq name is also kept
            // matchable for models that spell it out verbatim. The qualified id is the
            // namespace-qualified name (`Foo::bar`, `<EXTERNAL>::system`), double-keyed on the
            // fq id for the same reason.
            for (simple, sig, fq, qualified) in methods {
                let fq = fq.as_ref();
                f(fq, FunKey::Name, simple.as_ref());
                f(fq, FunKey::Name, fq);
                f(fq, FunKey::Signature, sig.as_ref());
                f(fq, FunKey::Signature, fq);
                f(fq, FunKey::QualifiedId, qualified.as_ref());
                f(fq, FunKey::QualifiedId, fq);
            }
        }
        Dispatch::Lua {
            methods,
            functions,
            externals,
            ..
        } => {
            // Lua IR names are fully qualified by module (`kong.pdk.request.get_headers`,
            // `direct-flow.source`). The simple name is matchable as well, so a model can say
            // `^source$` without spelling the module it happens to live in -- the same treatment
            // the Native arm gives decorated names.
            //
            // The simple name is read from the VMT, where the frontend put the name the
            // definition site actually wrote; it is not re-derived from the fq name. The two
            // differ when a module has two functions of one name: the second's IR name is
            // `<module>.f%1`, while the function is still simply named `f`.
            //
            // A Lua function has exactly ONE name, so the fq name *is* the qualified id. Only it:
            // keying the bare name there too would hand `qualified-id` exactly the collisions it
            // exists to remove. One consequence of deriving the id from the module: a single
            // file imported as the root itself has an empty module name, so there a function's
            // id and its bare name coincide.
            //
            // Externals -- called but never defined (the stdlib, and modules outside the import)
            // -- are spelled exactly as functions are. `os.execute` is reachable both as
            // `execute` (which also covers the method-call spelling `x:execute()`) and as the fq
            // `os.execute`. They have no `FunctionData`, so the body-dependent constraints will
            // not match them; `signature_match` is the supported shape for such a callee.
            for (simple, fq) in functions.iter().chain(externals.iter()) {
                let (simple, fq) = (simple.as_ref(), fq.as_ref());
                for key in [FunKey::Name, FunKey::Signature] {
                    if simple != fq {
                        f(fq, key, simple);
                    }
                    f(fq, key, fq);
                }
                f(fq, FunKey::QualifiedId, fq);
            }
            // The metatable-recovered class methods.
            for (cls, _name, fq) in methods {
                f(fq.as_ref(), FunKey::Parent, cls.as_ref());
            }
        }
        Dispatch::Unknown => {}
    }
}
