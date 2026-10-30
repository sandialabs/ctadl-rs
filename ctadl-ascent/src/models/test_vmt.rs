//! Name tables for hand-built test programs, written the way the frontends write theirs.

use ctadl_ir::mir::Symbol;
use ctadl_ir::mir::call::{Dispatch, FunctionNames, VirtualMethodTable};
use hashbrown::HashMap;
use smallvec::SmallVec;

/// `vmt` with each of its Java or CIL method rows named the way dex, jvm and cil name a method:
/// by its simple name, class and descriptor. A function with two rows keeps the first.
pub fn with_method_names(mut vmt: VirtualMethodTable) -> VirtualMethodTable {
    let names: Vec<FunctionNames> = match &vmt.dispatch {
        Dispatch::Java { methods, .. } => methods
            .iter()
            .map(|(cls, name, desc, fid)| method(&fid.0, &name.0, &cls.0, &desc.0))
            .collect(),
        Dispatch::Cil { methods, .. } => methods
            .iter()
            .map(|(cls, name, desc, fid)| method(&fid.0, &name.0, &cls.0, &desc.0))
            .collect(),
        _ => Vec::new(),
    };
    vmt.names.extend(names);
    vmt.finish_names();
    vmt
}

fn method(fid: &Symbol, simple: &Symbol, cls: &Symbol, desc: &Symbol) -> FunctionNames {
    FunctionNames::new(fid.clone(), simple.clone())
        .with_parent(cls.clone())
        .with_signature(desc.clone())
}

/// A pcode table, each `(simple, signature, fq, namespace-qualified)` named the way the pcode
/// frontend names a function.
pub fn native<'a>(
    rows: impl IntoIterator<Item = (&'a str, &'a str, &'a str, &'a str)>,
) -> VirtualMethodTable {
    let mut vmt = VirtualMethodTable::new(Dispatch::Native);
    vmt.names = rows
        .into_iter()
        .map(|(simple, sig, fq, qualified)| {
            FunctionNames::new(fq, simple)
                .with_signature(sig)
                .with_other(qualified)
        })
        .collect();
    vmt.finish_names();
    vmt
}

/// A Lua table over `methods` and `hierarchy`, its `functions` and `externals` (each `(simple,
/// fq)`) named the way the Lua frontend names them: a function's class is the table holding it
/// in `methods`.
pub fn lua(
    methods: Vec<(Symbol, Symbol, Symbol)>,
    functions: &[(Symbol, Symbol)],
    externals: &[(Symbol, Symbol)],
    hierarchy: HashMap<Symbol, SmallVec<[Symbol; 2]>>,
) -> VirtualMethodTable {
    let parent: HashMap<Symbol, Symbol> = methods
        .iter()
        .map(|(cls, _, fq)| (fq.clone(), cls.clone()))
        .collect();
    let mut names: Vec<FunctionNames> = functions
        .iter()
        .chain(externals)
        .map(|(simple, fq)| {
            let names = FunctionNames::new(fq.clone(), simple.clone());
            match parent.get(fq) {
                Some(cls) => names.with_parent(cls.clone()),
                None => names,
            }
        })
        .collect();
    names.sort_by(|a, b| a.fq.cmp(&b.fq));
    let mut vmt = VirtualMethodTable::new(Dispatch::Lua { methods, hierarchy });
    vmt.names = names;
    vmt.finish_names();
    vmt
}
