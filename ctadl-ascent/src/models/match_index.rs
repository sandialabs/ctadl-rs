/*! The per-import program view Portcullis's relations are built from.

One import's method table, its functions' IR, which import it is (for the `language` and
`import` attributes), and, when some rule reads `callsig`, the signature keys its call sites
name. [`super::portcullis::relations::ProgramFacts`] is built from it.

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
use ctadl_ir::mir::call::VirtualMethodTable;

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
    /// The VMT names more functions than this — Lua externals have no body — so
    /// this is the *arity / has_code / body* half of the program, not its function universe.
    /// The DSL relation layer joins the two; see [`super::portcullis::relations::ProgramFacts`].
    #[inline]
    pub fn functions_with_data(&self) -> impl Iterator<Item = (&'p str, &'p FunctionData)> + '_ {
        self.program_functions.iter().map(|(fq, data)| (*fq, *data))
    }
}
