/*! The match state a model generator's `where` is evaluated against.

Matching is a function of a program's name / parent / signature / qualified-id tables plus its
function universe. Those live here, in one struct with one construction path, and both matching
pipelines borrow it:

- [`super::json::ModelGeneratorIngest`], which runs the full source/sink/propagation visit and
  is what `ctadl query --models` uses per import;
- the index-time streaming matcher ([`super::matches::observe_import`]), which evaluates both
  sides of every bridge as the IR streams by.

There are permanently two pipelines, so the rule that keeps them from drifting is that there is
only one evaluator and only one index. A second implementation of `where` is how
`signature_match` ends up meaning two different things in two places.

# Why it borrows the program

The index holds `&'p str` into the [`ProgramInfo`] rather than owned strings, because under the
streaming posture it never outlives one. Each import builds an index, every applicable generator
is evaluated against it, the *matches* are copied into
[`ProgramModelMatches`](super::matches::ProgramModelMatches) as owned data, and the index and the
IR are both dropped before the next import is loaded. Owning the strings would buy retention
across the import loop, which is exactly what streaming exists not to need.
*/

use hashbrown::hash_map::HashMap;

use ctadl_ir::ProgramInfo;
use ctadl_ir::mir::FunctionData;
use ctadl_ir::mir::call::VirtualMethodTable;

use super::spec::ImportScope;
use super::universe_set::UniverseSet;

/// One program's matchable metadata, keyed the way each frontend spells its functions.
pub struct ProgramMatchIndex<'p> {
    /// Which import this is, for [`super::spec::ProgramScope::admits`].
    pub scope: ImportScope,
    pub(crate) vmt: &'p VirtualMethodTable,
    /// Maps simple names to fully qualified names.
    pub(crate) program_method_names: HashMap<&'p str, Vec<&'p str>>,
    /// Maps parent class to fully qualified name.
    pub(crate) program_method_parents: HashMap<&'p str, Vec<&'p str>>,
    /// Maps signatures to fully qualified name.
    pub(crate) program_method_signatures: HashMap<&'p str, Vec<&'p str>>,
    /// Maps a method's fully-qualified id to its fq-name, backing the exact-match
    /// `qualified-id` constraint. The key is whatever spelling uniquely names the
    /// method on this frontend: the `JavaMethod` id on jvm/dex, the
    /// namespace-qualified (but address-free) name on native, the module-qualified
    /// IR name on lua. Unlike [`Self::program_method_names`] this is never keyed on
    /// a bare name, so it can disambiguate two same-named methods in different
    /// namespaces.
    pub(crate) program_method_qualified_ids: HashMap<&'p str, Vec<&'p str>>,
    /// fq-name (== `FunctionData.name`) → the function's IR data. Backs the
    /// `has_code` / `number_parameters` / `uses_field` constraints, which need
    /// per-function body/parameter/field information.
    ///
    /// A function with no IR body has no entry, which is why those three constraints cannot
    /// match a Lua external or a dex/jvm `ext` stub. `signature_match` is the supported shape
    /// for a bodyless callee.
    pub(crate) program_functions: HashMap<&'p str, &'p FunctionData>,
    /// The full set of function fq-names, always [`UniverseSet::Explicit`]. Mirrors
    /// what [`matched_functions`](super::json::matched_functions)`(&All)` enumerates for this
    /// frontend, so a top-level `not X` can be materialized to `universe \ X`.
    pub(crate) universe: UniverseSet<&'p str>,
}

impl<'p> ProgramMatchIndex<'p> {
    /// Indexes `program_info`'s metadata for matching. `scope` identifies the import it came
    /// from; pass [`ImportScope::unknown`] when there is none.
    pub fn new(program_info: &'p ProgramInfo, scope: ImportScope) -> Self {
        let vmt = &program_info.vmt;
        let mut program_method_names: HashMap<&'p str, Vec<&'p str>> = HashMap::new();
        let mut program_method_parents: HashMap<&'p str, Vec<&'p str>> = HashMap::new();
        let mut program_method_signatures: HashMap<&'p str, Vec<&'p str>> = HashMap::new();
        let mut program_method_qualified_ids: HashMap<&'p str, Vec<&'p str>> = HashMap::new();

        if let VirtualMethodTable::Java { methods, .. } = vmt {
            methods
                .iter()
                .map(|(_cls, name, _sig, fid)| (name.as_ref(), fid.as_ref()))
                .for_each(|(key, val)| program_method_names.entry(key).or_default().push(val));

            methods
                .iter()
                .map(|(cls, _name, _sig, fid)| (cls.as_ref(), fid.as_ref()))
                .for_each(|(key, val)| program_method_parents.entry(key).or_default().push(val));

            methods
                .iter()
                .map(|(_cls, _name, sig, fid)| (sig.as_ref(), fid.as_ref()))
                .for_each(|(key, val)| program_method_signatures.entry(key).or_default().push(val));

            // The `JavaMethod` id, e.g. `Lcom/example/Foo;->bar(I)V`. Descriptor-bearing and
            // stable, but until now only ever a *value* above — never a key — which is what
            // made exact fully-qualified matching impossible on jvm/dex.
            methods
                .iter()
                .map(|(_cls, _name, _sig, fid)| (fid.as_ref(), fid.as_ref()))
                .for_each(|(key, val)| {
                    program_method_qualified_ids
                        .entry(key)
                        .or_default()
                        .push(val)
                });
        } else if let VirtualMethodTable::Native { methods } = vmt {
            // Native frontends (pcode, clang) carry, per function, a simple
            // (un-decorated) name and a best-effort type signature alongside the
            // fully-qualified IR name. Key matching off the SIMPLE name so a model
            // pattern like `^system$` resolves even when the IR name is decorated
            // (e.g. Ghidra's `<EXTERNAL>::system@00101008`). The fully-qualified
            // name is also kept matchable for models that spell it out verbatim.
            for (simple, sig, fq, qualified) in methods {
                let simple = simple.as_ref();
                let fq = fq.as_ref();
                program_method_names.entry(simple).or_default().push(fq);
                program_method_signatures.entry(sig).or_default().push(fq);
                program_method_names.entry(fq).or_default().push(fq);
                program_method_signatures.entry(fq).or_default().push(fq);
                // The namespace-qualified name, e.g. `Foo::bar` or `<EXTERNAL>::system`.
                // Double-key on the fq id as well, mirroring the names/signatures maps
                // above, so a model that spells the decorated id out verbatim still
                // resolves through `qualified-id`.
                program_method_qualified_ids
                    .entry(qualified.as_ref())
                    .or_default()
                    .push(fq);
                program_method_qualified_ids.entry(fq).or_default().push(fq);
            }
        } else if let VirtualMethodTable::Lua {
            functions,
            externals,
            ..
        } = vmt
        {
            // Lua IR names are fully qualified by module (`kong.pdk.request.get_headers`,
            // `direct-flow.source`). Key matching off the simple name as well, so a model can say
            // `^source$` without spelling the module it happens to live in -- the same treatment
            // the Native arm gives decorated names. Both spellings resolve to the fully-qualified
            // IR name.
            //
            // The simple name is read from the VMT, where the frontend put the name the definition
            // site actually wrote; it is not re-derived from the fq name here. The two differ when
            // a module has two functions of one name: the second's IR name is `<module>.f%1`, whose
            // trailing component is `f%1`, while the function is still simply named `f`.
            //
            // A Lua function has exactly ONE name, so unlike the Native arm there is no separate
            // id column to key `qualified-id` on: the fq name *is* the qualified id, and one entry
            // covers both roles. Note the `entry(fq)` below sits OUTSIDE the `keys` loop on
            // purpose -- keying it on `simple` too would hand `qualified-id` exactly the bare-name
            // collisions it exists to remove (see the field's doc comment). One consequence of
            // deriving the id from the module: a single file imported as the root itself has an
            // empty module name, so there a function's id and its bare name coincide.
            for (simple, fq) in functions {
                let simple = simple.as_ref();
                let fq = fq.as_ref();
                let keys: &[&str] = if simple == fq { &[fq] } else { &[simple, fq] };
                for key in keys {
                    program_method_names.entry(key).or_default().push(fq);
                    program_method_signatures.entry(key).or_default().push(fq);
                }
                program_method_qualified_ids.entry(fq).or_default().push(fq);
            }
            // Externals -- called but never defined (the stdlib, and modules outside the import).
            // Indexed exactly as `functions` above, with the same reason for keeping the fq name
            // out of the `keys` loop for `qualified-id`: keying it on the bare name would hand
            // `qualified-id` the collisions it exists to remove. `os.execute` is reachable both
            // as `execute` (which also covers the method-call spelling `x:execute()`) and as the
            // fq `os.execute`.
            //
            // Externals have no `FunctionData`, so `has_code` / `number_parameters` / `uses_field`
            // will not match them -- already true of the dex/jvm `ext` entries. This is what lets
            // a bridge, or a default propagation model, attach to a bodyless callee at all, and
            // it is also why the supported shape for such a side is `signature_match`.
            for (simple, fq) in externals {
                let simple = simple.as_ref();
                let fq = fq.as_ref();
                let keys: &[&str] = if simple == fq { &[fq] } else { &[simple, fq] };
                for key in keys {
                    program_method_names.entry(key).or_default().push(fq);
                    program_method_signatures.entry(key).or_default().push(fq);
                }
                program_method_qualified_ids.entry(fq).or_default().push(fq);
            }
        } else {
            // Fallback (Unknown / CplusPlus): use the IR function names directly.
            for func in &program_info.program.functions.functions {
                let name = func.name.as_str();
                program_method_signatures
                    .entry(name)
                    .or_default()
                    .push(name);
                program_method_names.entry(name).or_default().push(name);
                program_method_qualified_ids
                    .entry(name)
                    .or_default()
                    .push(name);
            }
        }
        // Index every IR function by its fq-name for the body/parameter/field
        // constraints (`has_code`, `number_parameters`, `uses_field`).
        let mut program_functions: HashMap<&'p str, &'p FunctionData> = HashMap::new();
        for func in &program_info.program.functions.functions {
            program_functions.entry(func.name.as_str()).or_insert(func);
        }

        let universe: UniverseSet<&'p str> = match vmt {
            VirtualMethodTable::Java { methods, .. } => {
                methods.iter().map(|(_, _, _, fid)| fid.as_ref()).collect()
            }
            VirtualMethodTable::Native { methods } => {
                methods.iter().map(|(_, _, fq, _)| fq.as_ref()).collect()
            }
            // Every lowered function, class method or not, plus the externals. Before the VMT
            // carried the `functions` column there was nothing here to enumerate free functions
            // with, so the universe was empty and a top-level `not` on lua matched *nothing* --
            // while `matched_functions(&All)` (the sibling of this set) returned the class
            // methods, so the two disagreed on the one frontend. The externals belong here for
            // the same reason: a top-level `not` should see everything a model can name.
            VirtualMethodTable::Lua {
                functions,
                externals,
                ..
            } => functions
                .iter()
                .chain(externals.iter())
                .map(|(_, fq)| fq.as_ref())
                .collect(),
            VirtualMethodTable::Unknown => UniverseSet::empty(),
        };

        Self {
            scope,
            vmt,
            program_method_names,
            program_method_parents,
            program_method_signatures,
            program_method_qualified_ids,
            program_functions,
            universe,
        }
    }

    /// The virtual method table this index was built from.
    #[inline]
    pub fn vmt(&self) -> &'p VirtualMethodTable {
        self.vmt
    }

    /// Resolves a matched set to the fq-names it denotes, consulting the VMT when the set is
    /// still `All`.
    #[inline]
    pub fn functions_of(&self, set: &UniverseSet<&'p str>) -> Vec<String> {
        super::json::matched_functions(set, self.vmt)
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
