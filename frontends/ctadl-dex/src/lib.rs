/*! Dex language front end.

Turns a dex file or an APK into CTADL IR. It takes an artifact and returns a
[`ctadl_ir::ProgramInfo`], and does nothing else: it does not touch the store, choose a front
end, or run the engine. Reading the APK's ZIP structure and its `classes*.dex` entries is
`dex_reader::apk`'s job. An APK's native libraries belong to a different front end, and are
handled one level up, in `ctadl-frontends`.

Conventions (S19). A field is the symbolic field `<Lcls;->name:type>`, with no layout. A class
that owns a static field is a global named by the class, `$Lcls;`, and a static field is that
global's field named by the field's name alone: `sput` is the store `$Lcls;.name = v`, `sget`
the load `x = $Lcls;.name`. A class
referenced from several dex files or splits is one global (G3). An array's elements are the one field `[]`, and its length the
field `length`. Arithmetic, bitwise operations, comparisons (a numeric conversion is a cast and stays a copy) and
`instance-of` supply values only: each operand is `value(v)`. A `const-method-handle` that
invokes a known method is a function reference to that method (`Context::method_handle_exp`
says which kinds do), and `invoke-polymorphic` of `MethodHandle.invoke` or `invokeExact` is an
indirect call through the handle, so the reference resolves it. An exception is the last result of every call and of every return;
`move-exception` reads it, and an exception a callee throws that no handler in the caller
catches is dropped rather than returned. An instruction may write a parameter register
directly: that assigns its `Param` variable, which SSA versions; version 0 is the incoming
value.
*/

use std::path::Path;
use std::sync::Arc;

use hashbrown::hash_map::HashMap;
use hashbrown::hash_set::HashSet;
use smallvec::{SmallVec, smallvec};
use source_info::{ArtifactKey, SpanLen};

use ctadl_import::error::{Error, ErrorContext};
use ctadl_import::notifications::Notifications;
use ctadl_ir::cir::call::{
    CallObject, Dispatch, FunctionNames, JavaClass, JavaDispatch, JavaMethod, JavaSignature,
    JavaSimpleName, VirtualMethodTable,
};
use ctadl_ir::*;
use dex_reader::basic_blocks::{basic_blocks, block_successors};
use dex_reader::error::DexError;
use dex_reader::instructions::{DataFlow, Instruction, MethodHandleIdx, Reg, TypeIdx};
use dex_reader::parser::{DecodedCodeItem, decode_code_item};
use dex_reader::types::{ACC_ABSTRACT, ACC_INTERFACE, ACC_NATIVE, ACC_STATIC, CodeItem, MethodId};
use dex_reader::{APKParser, DexParser};

mod merged;

/// The statements in a function's body.
fn statement_count(f: &FunctionData) -> usize {
    f.blocks
        .iter_enumerated()
        .map(|(_, b)| b.statements.len())
        .sum()
}

/// Gives `fdat` a body that returns at once, with no flows.
fn empty_body(fdat: &mut FunctionData) {
    fdat.return_type = ReturnType { arity: 2 };
    let block = fdat.blocks.blocks_mut().push(BasicBlockData::new(None));
    let empty = Exp::new_bytes(Vec::new());
    fdat.blocks[block].terminator = Some(Terminator::new_kind(TerminatorKind::Return {
        args: smallvec![empty.clone(), empty],
    }));
}

#[cfg(test)]
mod tests;

/// A Dex import: the IR, and what the import has to say about it.
pub struct DexImport {
    pub program_info: ProgramInfo,
    pub notifications: Notifications,
}

/// The Java half of an APK import: the IR, what the import has to say about it, and the number
/// of `classes*.dex` entries it was built from.
pub struct ApkImport {
    pub program_info: ProgramInfo,
    pub notifications: Notifications,
    pub dex_count: usize,
}

/// Imports the Dex code out of an APK.
///
/// An APK with no `classes*.dex` yields an empty program rather than an error: a split
/// APK out of an Android App Bundle -- `config.arm64_v8a.apk` inside an XAPK -- puts the
/// native libraries in one APK and the Dex in another, and the native half is still worth
/// importing on its own. Callers that need to distinguish the two check
/// [`ApkImport::dex_count`].
pub fn import_apk<P: AsRef<Path>>(file: P) -> Result<ApkImport, Error> {
    let file = file.as_ref();
    let timer = PhaseTimer::start();
    let data = std::fs::read(file).err_context(|| format!("reading APK: {}", file.display()))?;
    timer.done("read APK");
    let parser =
        APKParser::new(&data).err_context(|| format!("parsing APK: {}", file.display()))?;
    // The dex files are inflated out of it; the rest of the APK is not needed again.
    drop(data);
    let dex_count = parser.dex_count();
    let parsers = parser.dex_parsers_with_filenames();
    timer.done("unpack and parse dex files");
    let mut ctx = Importer::new(merged::MergedClasses::scan(
        &parsers.iter().map(|(_, p)| p).collect::<Vec<_>>(),
    ));
    timer.done("scan merged classes");
    let mut builders = ProgramInfoBuilder::new(VirtualMethodTable::new_java());

    for (sub_artifact_id, (dex_file_name, parser)) in parsers.into_iter().enumerate() {
        // This will not refer to a real file path, but a path "inside" the apk. But this is OK,
        // since for ArtifactEncoding::Binary, format won't read the file on this path.
        let key = ArtifactKey {
            path: file.join(dex_file_name).to_string_lossy().to_string(),
            sub_artifact_id: sub_artifact_id.try_into().unwrap(),
            hash: Vec::new(),
            encoding: source_info::ArtifactEncoding::Binary,
        };
        ctx.process(&parser, key, &mut builders)
            .err_context(|| format!("converting '{}' in APK: {}", dex_file_name, file.display()))?;
        timer.done(dex_file_name);
    }
    let (program_info, notifications) = ctx.finish(builders)?;
    timer.done("finish");
    Ok(ApkImport {
        program_info,
        notifications,
        dex_count,
    })
}

pub fn import_dex<P: AsRef<Path>>(file: P) -> Result<DexImport, Error> {
    let file = file.as_ref();
    let data =
        std::fs::read(file).err_context(|| format!("reading Dex file: {}", file.display()))?;
    let parser =
        DexParser::new(&data).err_context(|| format!("parsing Dex file: {}", file.display()))?;
    let mut ctx = Importer::new(merged::MergedClasses::scan(&[&parser]));
    let mut builders = ProgramInfoBuilder::new(VirtualMethodTable::new_java());
    let key = ArtifactKey {
        path: file.to_string_lossy().to_string(),
        sub_artifact_id: 0,
        hash: Vec::new(),
        encoding: source_info::ArtifactEncoding::Binary,
    };

    ctx.process(&parser, key, &mut builders)
        .err_context(|| format!("converting Dex file: {}", file.display()))?;
    let (program_info, notifications) = ctx.finish(builders)?;
    Ok(DexImport {
        program_info,
        notifications,
    })
}

/// The translation of dex files into CTADL IR.
///
/// The expectation is to call [`Importer::new`], then [`Importer::process`] possibly many times,
/// then [`Importer::finish`] exactly once. The reason is that we have to handle both multiple dex
/// files and open programs, programs that call functions for which we don't have a definition.
///
/// During `process`, the importer records methods that are "external," meaning we haven't so
/// far seen a definition for them. We also stub them out in the program. Since we might see a
/// definition for them during a later call to process, we also clear those entries when we
/// process the defined methods. We record the VMT entry for such methods in the
/// [`Importer::ext`] member.
///
/// During `finish`, any methods still-undefined are added to the VMT using what we recorded
/// during processing.
#[derive(Debug)]
struct Importer {
    defined: HashMap<String, FunctionIdx>,
    /// VMT entries for the externs seen so far.
    ext: HashMap<String, (JavaClass, JavaSimpleName, JavaSignature, JavaMethod)>,
    /// The program's class-merged classes; see [`merged`].
    merged: Arc<merged::MergedClasses>,
    /// Statements in the clones of merged methods, for the guardrail in [`Importer::finish`].
    clone_statements: usize,
    /// The globals declared so far: one per class that owns a static field. Moved into the
    /// program at [`Importer::finish`].
    globals: ctadl_ir::cir::Globals,
    /// Each class this import defines, as its global is named, with the dex file it is in and
    /// the offset of its `class_def_item` there: where the global is declared. Located
    /// in [`Importer::finish`], once every class has been lowered.
    class_defs: Vec<(String, ArtifactKey, u32)>,
}

/// The lowering of method bodies into CTADL IR: what one thread needs to lower one method at a
/// time ([`Context::lower_job`]). Everything that spans methods is [`Importer`]'s.
#[derive(Debug)]
struct Context {
    counter: Counter,
    call_result: Option<VariableRef>,
    catch_result: Option<VariableRef>,
    /// The methods the bodies lowered so far call.
    call_to: HashSet<MethodId>,
    /// The class whose method is being lowered.
    enclosing: Option<Arc<EnclosingClass>>,
    /// The program's class-merged classes; see [`merged`].
    merged: Arc<merged::MergedClasses>,
    /// The class each `new-instance` in the method being lowered allocates instead of the one it
    /// names, by pc.
    retag: HashMap<usize, String>,
    /// The pc of the instruction being lowered.
    pc: usize,
    /// The globals the method being lowered uses, in the order it first uses them.
    globals: ctadl_ir::cir::Globals,
    /// What [`Context::static_field`] returned for each field id, for the method being lowered.
    static_fields: HashMap<u32, (VariableRef, ctadl_ir::cir::PathSegment)>,
    /// What [`Context::instance_field`] returned for each field id of the dex file being lowered.
    instance_fields: HashMap<u32, ctadl_ir::cir::PathSegment>,
    /// The offset of each instruction lowered since the last time this was taken, in order. The
    /// statements of an instruction carry `1 +` its position here as their span id until
    /// [`Importer::place`] interns the real one.
    spans: Vec<u32>,
}

impl Importer {
    fn new(merged: merged::MergedClasses) -> Self {
        Self {
            defined: Default::default(),
            ext: Default::default(),
            merged: Arc::new(merged),
            clone_statements: 0,
            globals: Default::default(),
            class_defs: Vec::new(),
        }
    }

    /// Process one dex file into the program. Stubs any methods that are called but not defined.
    ///
    /// Three passes. The first walks the classes and methods in file order and does all the
    /// bookkeeping -- function slots, VMT rows, parameters -- collecting a [`Job`] per method
    /// body. The second lowers the bodies on the current rayon pool, each into a function of
    /// its own ([`Context::lower_job`]). The third places them into the program in file order
    /// ([`Importer::place`]), interning their spans and globals in exactly the order lowering
    /// them one after another did, so the result does not depend on the thread count.
    fn process(
        &mut self,
        parser: &DexParser<'_>,
        artifact_key: ArtifactKey,
        builders: &mut ProgramInfoBuilder,
    ) -> Result<(), Error> {
        let bookkeeping = PhaseTimer::start();
        let mut jobs: Vec<Job> = Vec::new();
        for class_def in parser.classes() {
            let class_name = parser.class_name(class_def)?;
            self.class_defs
                .push((class_name.clone(), artifact_key.clone(), class_def.offset));
            log::trace!("Class: {}", class_name);
            // Populate class hierarchy information for the VMT.
            // Immediate superclass (if any) and immediate super‑interfaces.
            let superclass_opt = if class_def.superclass_idx != 0 {
                parser
                    .get_type(class_def.superclass_idx as usize)
                    .and_then(|t| parser.type_descriptor(t).ok())
                    .map(|s| JavaClass(s.into()))
            } else {
                None
            };
            let iface_type_list = parser.class_interfaces(class_def)?;
            let mut iface_vec = Vec::new();
            for type_idx in iface_type_list.types {
                if let Some(t) = parser.get_type(type_idx as usize)
                    && let Ok(desc) = parser.type_descriptor(t)
                {
                    iface_vec.push(JavaClass(desc.into()));
                }
            }
            if let Dispatch::Java {
                hierarchy,
                interfaces,
                ..
            } = &mut builders.vmt.dispatch
            {
                let parents = hierarchy
                    .entry(JavaClass(class_name.clone().into()))
                    .or_default();
                for sup in superclass_opt.iter().chain(iface_vec.iter()) {
                    parents.push(sup.clone());
                }
                if ACC_INTERFACE.is_set_in(class_def.access_flags) {
                    interfaces.push(JavaClass(class_name.into()));
                }
            }
            let enclosing = Arc::new(EnclosingClass {
                superclass: superclass_opt.map(|c| c.0),
                interfaces: iface_vec.into_iter().map(|c| c.0).collect(),
            });
            let class_data = parser.class_data(class_def)?;
            for enc in class_data
                .direct_methods
                .iter()
                .chain(class_data.virtual_methods.iter())
            {
                let mi = parser.get_method(enc.method_idx as usize).unwrap();
                // fully qualified signature with class name, simple name, and descriptor
                let sig = parser.method_signature(mi)?;
                // we have the definition
                self.ext.remove(&sig);
                // We may have encountered this method as an "extern" in a previous dex file - if
                // so, we're parsing the definition, so add to the existing FunctionData.
                let fidx = match self.defined.get(&sig) {
                    Some(&fidx) => fidx,
                    None => {
                        let fidx = builders.program.new_function();
                        self.defined.insert(sig.clone(), fidx);
                        fidx
                    }
                };
                let fdat = &mut builders.program[fidx];
                fdat.name = sig.clone();
                let (class_name, method_name, method_descr) = parser.method_triple(mi)?;
                builders.vmt.names.push(
                    FunctionNames::new(sig.as_str(), method_name.as_str())
                        .with_parent(class_name.as_str())
                        .with_signature(method_descr.as_str()),
                );

                if let Dispatch::Java { methods, .. } = &mut builders.vmt.dispatch {
                    methods.push((
                        JavaClass(class_name.as_str().into()),
                        JavaSimpleName(method_name.as_str().into()),
                        JavaSignature(method_descr.as_str().into()),
                        JavaMethod(sig.as_str().into()),
                    ));
                }
                if ACC_ABSTRACT.is_set_in(enc.access_flags)
                    && let Dispatch::Java {
                        abstract_methods, ..
                    } = &mut builders.vmt.dispatch
                {
                    abstract_methods.push((
                        JavaClass(class_name.as_str().into()),
                        JavaSimpleName(method_name.as_str().into()),
                        JavaSignature(method_descr.as_str().into()),
                    ));
                }

                // Native methods are also listed in `natives`, besides their `methods` row
                // above. A `static` native is reached by `invoke-static`, which resolves by
                // name and never consults the table.
                if ACC_NATIVE.is_set_in(enc.access_flags)
                    && let Dispatch::Java { natives, .. } = &mut builders.vmt.dispatch
                {
                    natives.push((
                        JavaClass(class_name.as_str().into()),
                        JavaSimpleName(method_name.as_str().into()),
                        JavaSignature(method_descr.as_str().into()),
                        JavaMethod(sig.as_str().into()),
                        ACC_STATIC.is_set_in(enc.access_flags),
                    ));
                }

                // Parse the instruction stream for the method.
                if let Some(code) = parser.method_code(enc)? {
                    // ---------------------------------------------------------------------
                    // Handle parameter types
                    let param_types = parser.method_parameters(mi)?.types;
                    // Add "this" parameter as param 0
                    if !ACC_STATIC.is_set_in(enc.access_flags)
                        && !ACC_NATIVE.is_set_in(enc.access_flags)
                    {
                        fdat.params.push(ParameterType::Shared);
                    }
                    for param_idx in param_types.into_iter() {
                        let ty = parser.get_type(param_idx.into()).unwrap();
                        let descr = parser.get_string(ty.descriptor_idx.try_into().unwrap())?;
                        // double-wide arguments take two registers
                        let reg_count = if descr.starts_with("J") || descr.starts_with("D") {
                            2
                        } else {
                            1
                        };
                        for _ in 0..reg_count {
                            fdat.params.push(if !descr.starts_with("L") {
                                ParameterType::SharedFields
                            } else {
                                ParameterType::Shared
                            });
                        }
                    }

                    let empty_union = self
                        .merged
                        .class(&class_name)
                        .is_some_and(|c| c.empties_union(&sig));
                    let params = fdat.params.clone();
                    let clones = self.plan_clones(
                        builders,
                        &sig,
                        (&class_name, &method_name, &method_descr),
                    );
                    jobs.push(Job {
                        fidx,
                        sig,
                        class: class_name,
                        code,
                        params,
                        empty_union,
                        enclosing: enclosing.clone(),
                        clones,
                    });
                } else {
                    // Method without code – no basic blocks should be generated.
                    // We just set the return type.
                    fdat.return_type = ReturnType { arity: 2 };
                }
            }
        }

        bookkeeping.done(&format!("bookkeeping, {} jobs", jobs.len()));
        let timer = PhaseTimer::start();
        let call_to = self.lower_and_place(parser, &artifact_key, builders, jobs)?;
        timer.done("lower and place");

        // define extern funcs and parameters/return value
        {
            let mut call_to: Vec<_> = call_to.into_iter().collect();
            call_to.sort();
            for extern_id in call_to {
                let java_sig = parser
                    .method_signature(&extern_id)
                    .err_context(|| format!("method_signature: {extern_id:?}"))?;
                if self.defined.contains_key(&java_sig) {
                    continue;
                }
                let fidx = builders.program.new_function();
                self.defined.insert(java_sig.clone(), fidx);
                let fdat = &mut builders.program.functions[fidx];
                fdat.name = java_sig.clone();
                for ty in parser.method_parameters(&extern_id)?.types {
                    let ty = parser.get_type(ty.into()).unwrap();
                    let descr = parser.get_string(ty.descriptor_idx.try_into().unwrap())?;
                    fdat.params.push(if !descr.starts_with("L") {
                        ParameterType::SharedFields
                    } else {
                        ParameterType::Shared
                    });
                }
                // All functions return 2 values: (normal_return, exception_return)
                fdat.return_type = ReturnType { arity: 2 };
                let (class_name, method_name, descr) = parser
                    .method_triple(&extern_id)
                    .err_context(|| format!("method_triple: {extern_id:?}"))?;
                builders.vmt.names.push(
                    FunctionNames::new(java_sig.as_str(), method_name.as_str())
                        .with_parent(class_name.as_str())
                        .with_signature(descr.as_str()),
                );
                self.ext.insert(
                    java_sig.clone(),
                    (
                        JavaClass(class_name.into()),
                        JavaSimpleName(method_name.into()),
                        JavaSignature(descr.into()),
                        JavaMethod(java_sig.into()),
                    ),
                );
            }
        }

        Ok(())
    }

    /// Allocates the functions the clones of merged method `sig` will fill, one per class id,
    /// and writes their VMT rows: the bookkeeping half of splitting a merged class (see
    /// [`merged`]), in the order lowering the clones one after another did it. Returns each
    /// clone's slot and id; [`Context::lower_job`] lowers the bodies.
    fn plan_clones(
        &mut self,
        builders: &mut ProgramInfoBuilder,
        sig: &str,
        (class, simple, descr): (&str, &str, &str),
    ) -> Vec<(FunctionIdx, i64)> {
        let Some(class_info) = self.merged.class(class) else {
            return Vec::new();
        };
        if class_info.switches(sig).is_none() {
            return Vec::new();
        }
        let ids: Vec<i64> = class_info.ids.iter().copied().collect();
        let mut clones = Vec::with_capacity(ids.len());
        for id in ids {
            let split = merged::split_class(class, id);
            let split_sig = merged::split_method(sig, &split);
            let fidx = builders.program.new_function();
            self.defined.insert(split_sig.clone(), fidx);
            builders.vmt.names.push(
                FunctionNames::new(split_sig.as_str(), simple)
                    .with_parent(split.as_str())
                    .with_signature(descr),
            );
            builders.program[fidx].name = split_sig.clone();
            if let Dispatch::Java {
                methods, hierarchy, ..
            } = &mut builders.vmt.dispatch
            {
                methods.push((
                    JavaClass(split.as_str().into()),
                    JavaSimpleName(simple.into()),
                    JavaSignature(descr.into()),
                    JavaMethod(split_sig.as_str().into()),
                ));
                let parents = hierarchy
                    .entry(JavaClass(split.as_str().into()))
                    .or_default();
                let parent = JavaClass(class.into());
                if !parents.contains(&parent) {
                    parents.push(parent);
                }
            }
            clones.push((fidx, id));
        }
        clones
    }

    /// Lowers `jobs` on the current rayon pool and places the results into the program in
    /// order. Returns the methods the bodies call, for the extern stubs.
    ///
    /// The jobs go in batches, so that only one batch of lowered bodies waits to be placed at a
    /// time, and so that the next batch is lowered while this one is placed.
    fn lower_and_place(
        &mut self,
        parser: &DexParser<'_>,
        artifact_key: &ArtifactKey,
        builders: &mut ProgramInfoBuilder,
        jobs: Vec<Job>,
    ) -> Result<HashSet<MethodId>, Error> {
        use rayon::prelude::*;

        /// Bodies per batch: enough to keep every thread busy across uneven method sizes, few
        /// enough that a batch of lowered bodies is a small part of the program.
        const BATCH: usize = 4096;

        let merged = Arc::clone(&self.merged);
        let lower = |batch: &[Job]| -> Vec<(Result<Lowered, Error>, HashSet<MethodId>)> {
            batch
                .par_iter()
                .map_init(
                    || Context::for_file(Arc::clone(&merged)),
                    |ctx, job| {
                        let lowered = ctx.lower_job(parser, job);
                        (lowered, std::mem::take(&mut ctx.call_to))
                    },
                )
                .collect()
        };
        let mut call_to = HashSet::new();
        let mut file = None;
        let mut batches = jobs.chunks(BATCH);
        let mut current = batches.next().map(lower);
        let (mut placing, mut waiting) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
        while let Some(results) = current.take() {
            let next_batch = batches.next();
            let mut placed = Ok(());
            // Lower the next batch while this one is placed.
            let started = std::time::Instant::now();
            rayon::scope(|scope| {
                scope.spawn(|_| current = next_batch.map(lower));
                placed = self.place(results, artifact_key, &mut file, builders, &mut call_to);
                placing += started.elapsed();
            });
            waiting += started.elapsed();
            placed?;
        }
        log::debug!(
            target: "ctadl_dex::timing",
            "placing: {placing:.2?} of {waiting:.2?} lowering and placing"
        );
        Ok(call_to)
    }

    /// Puts a batch of lowered methods, and their clones, into the program, in order: interns
    /// their spans and declares their globals in the order lowering them in place would have,
    /// then rewrites their statements to use the program's ids (in parallel: each body on its
    /// own), then moves them into their functions.
    fn place(
        &mut self,
        batch: Vec<(Result<Lowered, Error>, HashSet<MethodId>)>,
        artifact_key: &ArtifactKey,
        file: &mut Option<source_info::FileId>,
        builders: &mut ProgramInfoBuilder,
        call_to: &mut HashSet<MethodId>,
    ) -> Result<(), Error> {
        use rayon::prelude::*;

        /// One lowered method with the program's ids for what it uses.
        struct Placed {
            /// Each function, with the span list it uses, the program's local for each of its
            /// locals (when lowered on top of an earlier definition), and that definition's
            /// locals table.
            bodies: Vec<(
                FunctionIdx,
                FunctionData,
                usize,
                Vec<LocalIdx>,
                Option<Locals>,
            )>,
            spans: Vec<Vec<source_info::FileSpanId>>,
            globals: Vec<GlobalIdx>,
            empty_union: bool,
        }

        // A dex file may define one method twice, and the second definition is then lowered on
        // top of the first: whether a body is grafted is decided by what is already in its
        // function. So when two bodies of a batch go into the same function, place the batch one
        // method at a time.
        if batch.len() > 1 {
            let mut targets = HashSet::new();
            let repeats = batch
                .iter()
                .filter_map(|(lowered, _)| lowered.as_ref().ok())
                .flat_map(|lowered| lowered.bodies.iter().map(|(fidx, _, _)| *fidx))
                .any(|fidx| !targets.insert(fidx));
            if repeats {
                for one in batch {
                    self.place(vec![one], artifact_key, file, builders, call_to)?;
                }
                return Ok(());
            }
        }
        let mut placed = Vec::with_capacity(batch.len());
        for (lowered, called) in batch {
            call_to.extend(called);
            let Lowered {
                bodies,
                spans,
                globals,
                clone_statements,
                empty_union,
            } = lowered?;
            self.clone_statements += clone_statements;
            let spans: Vec<Vec<source_info::FileSpanId>> = spans
                .iter()
                .map(|offsets| {
                    if offsets.is_empty() {
                        return Vec::new();
                    }
                    // Looked up at the file's first span, not before: that is when interning a
                    // span in it numbered the artifact.
                    let file = *file
                        .get_or_insert_with(|| builders.source_info_builder.file_for(artifact_key));
                    offsets
                        .iter()
                        .map(|&offset| {
                            builders.source_info_builder.span_in_file(
                                file,
                                offset,
                                SpanLen::ByteLen(2),
                            )
                        })
                        .collect()
                })
                .collect();
            let globals: Vec<GlobalIdx> = globals
                .iter_enumerated()
                .map(|(_, g)| self.globals.declare(&*g.name, None))
                .collect();
            let bodies = bodies
                .into_iter()
                .map(|(fidx, body, list)| {
                    let slot = &mut builders.program[fidx];
                    // A method defined in an earlier dex file too is lowered on top of that
                    // definition, interning its locals into that definition's table.
                    if slot.blocks.is_empty() && slot.locals.is_empty() {
                        return (fidx, body, list, Vec::new(), None);
                    }
                    let mut slot_locals = std::mem::take(&mut slot.locals);
                    let locals = body
                        .locals
                        .iter_enumerated()
                        .map(|(_, l)| slot_locals.get_or_intern(&l.name))
                        .collect();
                    slot_locals.drop_index();
                    (fidx, body, list, locals, Some(slot_locals))
                })
                .collect();
            placed.push(Placed {
                bodies,
                spans,
                globals,
                empty_union,
            });
        }

        placed.par_iter_mut().for_each(|p| {
            for (fidx, body, list, locals, _) in &mut p.bodies {
                let mut resolve = Resolve {
                    spans: &p.spans[*list],
                    globals: &p.globals,
                    locals,
                };
                ctadl_ir::cir::visit::MutVisitor::visit_function_data(&mut resolve, *fidx, body);
            }
        });

        for p in placed {
            for (fidx, body, _, _, slot_locals) in p.bodies {
                let slot = &mut builders.program[fidx];
                match slot_locals {
                    Some(slot_locals) => {
                        slot.locals = slot_locals;
                        graft(slot, body, p.empty_union);
                    }
                    None => {
                        let name = std::mem::take(&mut slot.name);
                        *slot = body;
                        slot.name = name;
                    }
                }
            }
        }
        Ok(())
    }

    fn finish(
        &mut self,
        mut builders: ProgramInfoBuilder,
    ) -> Result<(ProgramInfo, Notifications), Error> {
        let mut program = builders.program;
        program.globals = std::mem::take(&mut self.globals);
        // A class's global is declared at its `class_def_item`, if the program uses it.
        for (class, key, offset) in self.class_defs.drain(..) {
            if program.globals.lookup(&class).is_some() {
                let span = builders.source_info_builder.span_for(
                    key,
                    offset,
                    SpanLen::ByteLen(dex_reader::types::ClassDef::ITEM_LEN),
                );
                program.globals.declare(&class, Some(span));
            }
        }
        log::trace!("program: {program}");
        let notifications = self
            .merged
            .guardrail_notifications(self.clone_statements, || {
                program.functions.iter().map(statement_count).sum()
            });
        // Verify the generated program.
        program.verify()?;
        let mut ext: Vec<_> = self.ext.drain().collect();
        // Sort for determinism
        ext.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
        for (_sig, entry) in ext {
            if let Dispatch::Java { methods, .. } = &mut builders.vmt.dispatch {
                methods.push(entry);
            }
        }
        builders.vmt.finish_names();
        let source_info = builders.source_info_builder.finish();
        log::trace!("source_info: {source_info}");
        let vmt = builders.vmt;
        Ok((
            ProgramInfo {
                program,
                vmt,
                source_info,
            },
            notifications,
        ))
    }
}

/// Logs how long each phase of an import took, under the `ctadl_dex::timing` target at debug
/// level.
struct PhaseTimer(std::cell::Cell<std::time::Instant>);

impl PhaseTimer {
    fn start() -> Self {
        Self(std::cell::Cell::new(std::time::Instant::now()))
    }

    fn done(&self, phase: &str) {
        let now = std::time::Instant::now();
        let took = now - self.0.replace(now);
        log::debug!(target: "ctadl_dex::timing", "{phase}: {took:.2?}");
    }
}

/// What lowering an `invoke-super` needs to know about the class the call appears in.
#[derive(Debug, Default)]
struct EnclosingClass {
    superclass: Option<Symbol>,
    /// The interfaces the class directly implements. An `X.super.m()` names one of these, and
    /// there lookup begins at the named interface rather than at the superclass.
    interfaces: Vec<Symbol>,
}

impl Context {
    /// The class global `$Lcls;` and field `name` of the static field `field_ids[idx]`, whose
    /// pretty name is `Lcls;->name:type`. Declares the global on first use.
    fn static_field(
        &mut self,
        parser: &DexParser<'_>,
        idx: u32,
    ) -> Result<(VariableRef, ctadl_ir::cir::PathSegment), DexError> {
        if let Some(found) = self.static_fields.get(&idx) {
            return Ok(found.clone());
        }
        let pretty = parser.field_pretty_name(idx as usize)?;
        let (class, rest) = pretty
            .split_once("->")
            .unwrap_or_else(|| panic!("a field reference has a class: {pretty}"));
        let name = rest.rsplit_once(':').map_or(rest, |(name, _)| name);
        let global = self.globals.declare(class, None);
        let found = (
            VariableRef::global(global),
            ctadl_ir::cir::PathSegment::field(name),
        );
        self.static_fields.insert(idx, found.clone());
        Ok(found)
    }

    /// The field segment `<Lcls;->name:type>` an instance field access `field_ids[idx]` reads or
    /// writes.
    fn instance_field(
        &mut self,
        parser: &DexParser<'_>,
        idx: u32,
    ) -> Result<ctadl_ir::cir::PathSegment, DexError> {
        if let Some(found) = self.instance_fields.get(&idx) {
            return Ok(found.clone());
        }
        let pretty = parser.field_pretty_name(idx as usize)?;
        let found = ctadl_ir::cir::PathSegment::field(format!("<{pretty}>"));
        self.instance_fields.insert(idx, found.clone());
        Ok(found)
    }

    fn new() -> Self {
        Self {
            counter: Default::default(),
            call_result: Default::default(),
            catch_result: Default::default(),
            call_to: Default::default(),
            enclosing: None,
            merged: Default::default(),
            retag: Default::default(),
            globals: Default::default(),
            static_fields: Default::default(),
            instance_fields: Default::default(),
            spans: Vec::new(),
            pc: 0,
        }
    }

    /// A lowerer for the dex files of a program whose merged classes are `merged`.
    fn for_file(merged: Arc<merged::MergedClasses>) -> Self {
        Self {
            merged,
            ..Self::new()
        }
    }

    /// Lowers one method body, and the clones a merged class splits it into, each into a
    /// function of its own. Spans are recorded as instruction offsets and globals declared in a
    /// table of the job's own; [`Importer::place`] resolves both.
    fn lower_job(&mut self, parser: &DexParser<'_>, job: &Job) -> Result<Lowered, Error> {
        self.counter.reset();
        self.call_result = None;
        self.catch_result = None;
        self.globals = Default::default();
        self.static_fields.clear();
        self.spans.clear();
        self.enclosing = Some(Arc::clone(&job.enclosing));
        let mut out = Lowered::default();

        let mut base = FunctionData {
            params: job.params.clone(),
            ..Default::default()
        };
        self.retag = self.merged.retags(&job.sig, None);
        if job.empty_union {
            empty_body(&mut base);
        } else {
            self.lower_code(parser, &job.code, &mut base, &HashMap::new())?;
        }
        base.locals.drop_index();
        out.spans.push(std::mem::take(&mut self.spans));
        out.bodies.push((job.fidx, base, 0));
        out.empty_union = job.empty_union;

        if !job.clones.is_empty() {
            self.lower_clones(parser, job, &mut out)?;
        }
        out.globals = std::mem::take(&mut self.globals);
        Ok(out)
    }

    /// Lowers the clones of merged method `job`, one per class id, into the class split off for
    /// that id, with each switch on the id narrowed to that id's arm. See [`merged`].
    ///
    /// Every clone is the same lowering of the body but for the switch arm it keeps and the
    /// classes its `new-instance`s allocate. So the body is lowered once, unnarrowed and
    /// unretagged, and each clone is built out of that template, copying only the blocks its arm
    /// reaches ([`instantiate_clone`]). Lowering every clone in full and pruning afterwards did
    /// the whole body's work once per id. The template interns the same spans and globals, in
    /// the same order, as lowering the first clone did.
    fn lower_clones(
        &mut self,
        parser: &DexParser<'_>,
        job: &Job,
        out: &mut Lowered,
    ) -> Result<(), Error> {
        let merged = Arc::clone(&self.merged);
        let switches = merged
            .class(&job.class)
            .and_then(|c| c.switches(&job.sig))
            .expect("a job has clones only for a switching method of a merged class");
        let mut template = FunctionData {
            params: job.params.clone(),
            ..Default::default()
        };
        self.counter.reset();
        self.retag.clear();
        let info = self.lower_code(parser, &job.code, &mut template, &HashMap::new())?;
        let template_spans = out.spans.len();
        out.spans.push(std::mem::take(&mut self.spans));
        for &(fidx, id) in &job.clones {
            self.counter.reset();
            self.retag = merged.retags(&job.sig, Some(id));
            let narrow = merged::narrowed(switches, id);
            let (mut fdat, spans) = if info.clonable {
                let fdat = instantiate_clone(&template, &info, &narrow, |pc, ty| {
                    match self.retag.get(&pc) {
                        Some(split) => Some(Exp::new_object_ref(CallObject::JavaObject(
                            JavaClass(split.as_str().into()),
                        ))),
                        None => Self::allocation_exp(parser, ty),
                    }
                });
                (fdat, template_spans)
            } else {
                let mut fdat = FunctionData {
                    params: job.params.clone(),
                    ..Default::default()
                };
                self.lower_code(parser, &job.code, &mut fdat, &narrow)?;
                // The narrowed switch leaves every other id's arm unreachable. Without this each
                // clone stores the whole merged body, so a class with n ids stores it n times
                // over. The index would prune the same blocks before SSA anyway; doing it here
                // keeps them out of the stored program.
                ctadl_ir::ssa::prune_unreachable_nodes(&mut fdat);
                out.spans.push(std::mem::take(&mut self.spans));
                (fdat, out.spans.len() - 1)
            };
            fdat.locals.drop_index();
            out.clone_statements += statement_count(&fdat);
            out.bodies.push((fidx, fdat, spans));
        }
        self.retag.clear();
        Ok(())
    }

    /// Lowers one method body into `fdat`, whose parameters are already set. `narrow` maps the pc
    /// of a switch to the pc it must jump to, for a clone of a merged class's method.
    fn lower_code(
        &mut self,
        parser: &DexParser<'_>,
        code: &CodeItem,
        fdat: &mut FunctionData,
        narrow: &HashMap<usize, usize>,
    ) -> Result<LowerInfo, Error> {
        // A call's result is read by the instruction after it, in the same body. Left over from
        // the previous body, it would name a local of that body's table.
        self.call_result = None;
        self.catch_result = None;
        let items = decode_code_item(code);
        // Compute Dex basic blocks.
        let bb_vec = basic_blocks(code, &items);

        // Compute successors for each basic block using dex-reader.
        let block_successors_vec = block_successors(code, &items);

        // Ensure CIR has a block for each Dex basic block.
        for _ in &bb_vec {
            fdat.blocks.blocks_mut().push(BasicBlockData::new(None));
        }

        let mut offset_to_bb: HashMap<usize, BasicBlockIdx> = HashMap::new();
        for (i, bb) in bb_vec.iter().enumerate() {
            let offset = items[bb.start].offset();
            offset_to_bb.insert(offset, BasicBlockIdx::new(i));
        }

        // All Java functions return 2 values: (normal_return, exception_return)
        fdat.return_type = ReturnType { arity: 2 };

        // What `dataflow_to_assign_into` makes of one instruction, reused across instructions.
        let mut stmts = Vec::new();
        // The statements of the block being lowered, moved into it in one exactly sized
        // allocation once the block is done: pushing them one by one grew each block's deque by
        // doubling, which cost both the reallocations and up to half of every block's memory.
        let mut block_stmts: Vec<Statement> = Vec::new();
        let mut info = LowerInfo {
            last_pc: Vec::with_capacity(bb_vec.len()),
            offset_to_bb: HashMap::new(),
            new_instances: Vec::new(),
            clonable: true,
        };
        // Populate each CIR block.
        for (i, bb) in bb_vec.iter().enumerate() {
            let block_idx = BasicBlockIdx::new(i);
            let range = &items[bb.start..bb.end];
            for dci in range {
                if let DecodedCodeItem::Instruction { inst, offset } = dci {
                    self.pc = *offset;
                    let item_offset = code.absolute_offset(dci);
                    self.spans.push(item_offset.try_into().unwrap());
                    let source_info = SourceInfo::new(source_info::FileSpanId(
                        self.spans.len().try_into().unwrap(),
                    ));
                    if let Some(mut stmt) = self.decode_call(parser, code, inst, &mut fdat.locals) {
                        stmt.source_info = source_info;
                        block_stmts.push(stmt);
                    } else {
                        self.dataflow_to_assign_into(
                            parser,
                            code,
                            inst,
                            &mut fdat.locals,
                            &mut stmts,
                        )?;
                        if let Instruction::NewInstance(f) = inst {
                            // A site a clone of a merged method may retag; see `LowerInfo`.
                            if stmts.len() == 1
                                && matches!(stmts[0].kind, StatementKind::Assign { .. })
                                && Self::allocation_exp(parser, f.idx).is_some()
                            {
                                info.new_instances.push(NewInstanceSite {
                                    block: block_idx,
                                    stmt: fdat.blocks[block_idx].len() + block_stmts.len(),
                                    pc: *offset,
                                    ty: f.idx,
                                });
                            } else {
                                info.clonable = false;
                            }
                        }
                        for mut stmt in stmts.drain(..) {
                            stmt.source_info = source_info;
                            block_stmts.push(stmt);
                        }
                    }
                }
            }
            let block = &mut fdat.blocks[block_idx];
            if block.is_empty() {
                block.statements = block_stmts.drain(..).collect();
            } else {
                for stmt in block_stmts.drain(..) {
                    block.push_back(stmt);
                }
            }

            // Determine the terminator for this block using the last instruction in the block.
            let last = range.iter().rev().find_map(|dci| {
                if let DecodedCodeItem::Instruction { inst, offset } = dci {
                    Some((*offset, inst))
                } else {
                    None
                }
            });
            info.last_pc.push(last.map(|(pc, _)| pc));
            let last_inst = last.map(|(_, inst)| inst);
            let arm = last
                .and_then(|(pc, _)| narrow.get(&pc))
                .and_then(|target| offset_to_bb.get(target));

            let term = if let Some(&arm) = arm {
                TerminatorKind::Goto {
                    targets: smallvec![arm],
                }
            } else if let Some(inst) = last_inst {
                // Return instructions end the function.
                match inst {
                    // Throw instruction - jump to handlers if protected, else return exception
                    Instruction::Throw(f) => {
                        let succ_usizes = &block_successors_vec[i];
                        let succs = succ_usizes
                            .iter()
                            .map(|&b| BasicBlockIdx::new(b))
                            .collect::<SmallVec<[BasicBlockIdx; 4]>>();

                        if succs.is_empty() {
                            let throw_exp = Exp::from(AccessPath::from(reg_to_var(
                                code,
                                f.a,
                                &mut fdat.locals,
                            )));
                            let empty_exp = Exp::new_bytes(Vec::new());
                            TerminatorKind::Return {
                                args: smallvec![empty_exp, throw_exp],
                            }
                        } else {
                            TerminatorKind::Goto { targets: succs }
                        }
                    }
                    // Return with a value (register)
                    Instruction::Return(reg)
                    | Instruction::ReturnWide(reg)
                    | Instruction::ReturnObject(reg) => {
                        let ret_exp =
                            Exp::from(AccessPath::from(reg_to_var(code, reg.a, &mut fdat.locals)));
                        let empty_exp = Exp::new_bytes(Vec::new());
                        TerminatorKind::Return {
                            args: smallvec![ret_exp, empty_exp],
                        }
                    }
                    // Void return
                    Instruction::ReturnVoid(_) => {
                        let empty_exp = Exp::new_bytes(Vec::new());
                        TerminatorKind::Return {
                            args: smallvec![empty_exp.clone(), empty_exp],
                        }
                    }
                    // Compute successors for this block using the precalculated vector.
                    _ => {
                        let succ_usizes = &block_successors_vec[i];
                        let succs = succ_usizes
                            .iter()
                            .map(|&b| BasicBlockIdx::new(b))
                            .collect::<SmallVec<[BasicBlockIdx; 4]>>();

                        if succs.is_empty() {
                            // No successors - this block should return
                            let empty_exp = Exp::new_bytes(Vec::new());
                            TerminatorKind::Return {
                                args: smallvec![empty_exp.clone(), empty_exp],
                            }
                        } else {
                            TerminatorKind::Goto { targets: succs }
                        }
                    }
                }
            } else {
                // Empty block or block with only payloads – create a return.
                let empty_exp = Exp::new_bytes(Vec::new());
                TerminatorKind::Return {
                    args: smallvec![empty_exp.clone(), empty_exp],
                }
            };
            fdat.blocks[block_idx].terminator = Some(Terminator::new_kind(term));
        }
        info.offset_to_bb = offset_to_bb;
        Ok(info)
    }

    /// Where an `invoke-super` naming `cls` begins method lookup.
    ///
    /// Dalvik starts at the superclass of the class declaring the current method, not at the
    /// class the instruction names -- d8 usually writes the superclass there, but not always,
    /// and a walk from the current class would resolve the call to itself. The exception is
    /// `X.super.m()`, whose reference names an interface the enclosing class implements
    /// directly; there lookup does start at the named interface.
    ///
    /// `None` when the enclosing class is unknown or has no superclass, which leaves the site
    /// on the ordinary resolvent set.
    fn super_start(&self, cls: &Symbol) -> Option<Symbol> {
        let enclosing = self.enclosing.as_ref()?;
        if enclosing.interfaces.iter().any(|i| i == cls) {
            return Some(cls.clone());
        }
        enclosing.superclass.clone()
    }

    /// Decodes a call instruction into a `StatementKind::CallAssign` (see
    /// [`Context::call_assign`]); `None` for an instruction that is not a call.
    fn decode_call(
        &mut self,
        parser: &DexParser<'_>,
        code: &CodeItem,
        inst: &Instruction,
        locals: &mut Locals,
    ) -> Option<Statement> {
        // If this instruction is not a call, bail out.
        let args_regs = inst.call_args()?; // returns &[Reg]

        // invoke-custom names a call site, not a method: a bootstrap method picks the target at
        // run time. Record the call with an unknown target, so that the move-result after it
        // reads this call's result rather than an earlier call's.
        if matches!(
            inst,
            Instruction::InvokeCustom(_) | Instruction::InvokeCustomRange(_)
        ) {
            let args = args_regs
                .iter()
                .map(|reg| AccessPath::from(reg_to_var(code, *reg, locals)).into())
                .collect();
            return Some(self.call_assign(CallStyle::Unknown, args, locals));
        }

        // Resolve the method index from the specific invoke variant, and with it the
        // dispatch kind.
        let (method_idx, dispatch) = match inst {
            Instruction::InvokeVirtual(fmt) => (fmt.idx, Some(JavaDispatch::Virtual)),
            Instruction::InvokeSuper(fmt) => (fmt.idx, Some(JavaDispatch::Super)),
            Instruction::InvokeDirect(fmt) => (fmt.idx, None),
            Instruction::InvokeStatic(fmt) => (fmt.idx, None),
            Instruction::InvokeInterface(fmt) => (fmt.idx, Some(JavaDispatch::Interface)),
            Instruction::InvokeVirtualRange(fmt) => (fmt.idx, Some(JavaDispatch::Virtual)),
            Instruction::InvokeSuperRange(fmt) => (fmt.idx, Some(JavaDispatch::Super)),
            Instruction::InvokeDirectRange(fmt) => (fmt.idx, None),
            Instruction::InvokeStaticRange(fmt) => (fmt.idx, None),
            Instruction::InvokeInterfaceRange(fmt) => (fmt.idx, Some(JavaDispatch::Interface)),
            // invoke-polymorphic calls a signature-polymorphic method on the receiver in its
            // first argument. `MethodHandle.invoke` and `invokeExact` become an indirect call
            // below; any other (`VarHandle`'s) is a virtual call.
            Instruction::InvokePolymorphic(fmt) => (fmt.method_id, Some(JavaDispatch::Virtual)),
            Instruction::InvokePolymorphicRange(fmt) => {
                (fmt.method_id, Some(JavaDispatch::Virtual))
            }
            _ => return None,
        };
        let is_static = dispatch.is_none();

        // Resolve the callee name (human‑readable signature).
        let mi = parser.get_method(method_idx.0 as usize)?;
        let names = parser.method_names(method_idx.0 as usize).ok()?;
        let (cls, simple_name, descriptor) = (names.class, names.name, names.descriptor);

        // Convert argument registers into IR expressions.
        let args: ctadl_ir::ThinVec<Exp> = args_regs
            .iter()
            .map(|reg| AccessPath::from(reg_to_var(code, *reg, locals)).into())
            .collect();

        // Invoking a method handle calls whatever the handle refers to: an indirect call
        // through the handle, whose function reference resolves it (S11), with the remaining
        // arguments passed straight through.
        if matches!(
            inst,
            Instruction::InvokePolymorphic(_) | Instruction::InvokePolymorphicRange(_)
        ) && ctadl_ir::cir::call::is_method_handle_invoke(cls, simple_name)
        {
            let callee = args[0].access_path()?.clone();
            let style = CallStyle::FuncPtrCall {
                callee,
                signature: None,
            };
            return Some(self.call_assign(style, args.into_iter().skip(1).collect(), locals));
        }
        self.call_to.insert(*mi);
        let style = match dispatch {
            None => CallStyle::DirectCall {
                call_edges: CallEdges::Explicit(
                    [names.signature.to_string()].into_iter().collect(),
                ),
            },
            Some(dispatch) => {
                let cls: Symbol = cls.into();
                CallStyle::JavaCall {
                    receiver: args[0].variable_ref().unwrap().clone(),
                    method: JavaMethodRef {
                        super_start: (dispatch == JavaDispatch::Super)
                            .then(|| self.super_start(&cls))
                            .flatten(),
                        cls,
                        simple_name: simple_name.into(),
                        descriptor: descriptor.into(),
                        dispatch,
                    }
                    .into(),
                }
            }
        };

        let args = if is_static {
            args
        } else {
            args.into_iter().skip(1).collect()
        };
        Some(self.call_assign(style, args, locals))
    }

    /// A call statement returning into the result and exception temporaries, which the next
    /// move-result or move-exception reads.
    fn call_assign(
        &mut self,
        style: CallStyle,
        args: ctadl_ir::ThinVec<Exp>,
        locals: &mut Locals,
    ) -> Statement {
        // Dex returns into a special register, so just create a temporary.
        let retval = Context::ret(locals);
        let throwval = Context::except(locals);
        self.call_result = Some(retval.clone());
        self.catch_result = Some(throwval.clone());
        Statement::new_kind(StatementKind::CallAssign {
            style,
            rets: ctadl_ir::thin_vec![retval, throwval],
            args,
        })
    }

    /// A new object of the class or array type `idx` names, or `None` when the
    /// type cannot be read.
    fn allocation_exp(parser: &DexParser<'_>, idx: TypeIdx) -> Option<Exp> {
        let tid = parser.constant_pool().type_ids.get(idx.0 as usize)?;
        let desc = tid.descriptor_str(&parser.constant_pool().strings).ok()?;
        Some(Exp::new_object_ref(CallObject::JavaObject(JavaClass(
            desc.into(),
        ))))
    }

    /// The method handle `idx`, when it invokes a known method, as a function reference to that
    /// method (S11), named as the method's `FunctionData` is. The method joins the callees, so
    /// it gets a declaration if this program does not define it.
    ///
    /// `invoke-static` and `invoke-direct` handles name the method that runs. For
    /// `invoke-instance` and `invoke-interface` the handle names the declared method, which is
    /// the target; an override in a subclass is not. `invoke-constructor` is left out:
    /// invoking it allocates the object `<init>` takes as `this`, so the handle's arguments do
    /// not line up with `<init>`'s formals. Field handles name no method. Those, and a handle
    /// that cannot be read, are `None`.
    fn method_handle_exp(&mut self, parser: &DexParser<'_>, idx: MethodHandleIdx) -> Option<Exp> {
        const INVOKE_STATIC: u16 = 0x04;
        const INVOKE_INSTANCE: u16 = 0x05;
        const INVOKE_DIRECT: u16 = 0x07;
        const INVOKE_INTERFACE: u16 = 0x08;
        let handle = parser.constant_pool().method_handles.get(idx.0 as usize)?;
        if !matches!(
            handle.method_handle_type,
            INVOKE_STATIC | INVOKE_INSTANCE | INVOKE_DIRECT | INVOKE_INTERFACE
        ) {
            return None;
        }
        let mi = parser.get_method(handle.field_or_method_id.into())?;
        let name = parser.method_signature(mi).ok()?;
        self.call_to.insert(*mi);
        Some(Exp::new_object_ref(CallObject::FunctionPtr(
            name.as_str().into(),
        )))
    }

    /// [`Context::dataflow_to_assign_into`] into a fresh vector.
    #[cfg(test)]
    fn dataflow_to_assign(
        &mut self,
        parser: &DexParser<'_>,
        code_item: &CodeItem,
        inst: &Instruction,
        locals: &mut Locals,
    ) -> Result<Vec<Statement>, DexError> {
        let mut stmts = Vec::new();
        self.dataflow_to_assign_into(parser, code_item, inst, locals, &mut stmts)?;
        Ok(stmts)
    }

    /// Appends to `stmts` the statements a non-call instruction lowers to.
    fn dataflow_to_assign_into(
        &mut self,
        parser: &DexParser<'_>,
        code_item: &CodeItem,
        inst: &Instruction,
        locals: &mut Locals,
        stmts: &mut Vec<Statement>,
    ) -> Result<(), DexError> {
        let DataFlow { source, dest } = inst.data_flow();

        if let Some(const_exp) = match inst {
            // 8-bit signed constant
            Instruction::Const4(f) => Some(Exp::new_int(f.lit as i64)),
            // 16-bit signed constant
            Instruction::Const16(f) => Some(Exp::new_int(f.lit as i64)),
            // 32-bit signed constant
            Instruction::Const(f) => Some(Exp::new_int(f.lit as i64)),
            // high 16 bits of a 32-bit constant
            Instruction::ConstHigh16(f) => Some(Exp::new_int(((f.lit as i32) << 16) as i64)),
            // 16-bit wide constant, sign-extended to 64 bits
            Instruction::ConstWide16(f) => Some(Exp::new_int(f.lit as i64)),
            // 32-bit wide constant, sign-extended to 64 bits
            Instruction::ConstWide32(f) => Some(Exp::new_int(f.lit as i64)),
            // full 64-bit constant
            Instruction::ConstWide(f) => Some(Exp::new_int(f.lit)),
            // high 16 bits of a 64-bit constant
            Instruction::ConstWideHigh16(f) => Some(Exp::new_int((f.lit as i64) << 48)),
            // String constant (regular). Lossy on purpose: a DEX string
            // constant may legally hold unpaired UTF-16 surrogates -- packed
            // lexer tables do -- and no `str` can.
            Instruction::ConstString(f) => {
                if let Ok(s) = parser
                    .constant_pool()
                    .strings
                    .get_lossy_str(f.idx.0 as usize)
                {
                    Some(Exp::new_str(s))
                } else {
                    None
                }
            }
            Instruction::ConstStringJumbo(f) => {
                if let Ok(s) = parser
                    .constant_pool()
                    .strings
                    .get_lossy_str(f.idx.0 as usize)
                {
                    Some(Exp::new_str(s))
                } else {
                    None
                }
            }
            // Class constant – resolved to its descriptor string
            Instruction::ConstClass(f) => {
                if let Some(tid) = parser.constant_pool().type_ids.get(f.idx.0 as usize)
                    && let Ok(desc) = tid.descriptor_str(&parser.constant_pool().strings)
                {
                    Some(Exp::new_str(desc))
                } else {
                    None
                }
            }
            // NewInstance – resolved to a JavaObject reference
            // ... or the class split off a merged class for the site's id.
            Instruction::NewInstance(f) => match self.retag.get(&self.pc) {
                Some(split) => Some(Exp::new_object_ref(CallObject::JavaObject(JavaClass(
                    split.as_str().into(),
                )))),
                None => Self::allocation_exp(parser, f.idx),
            },
            // A new-array is a new object of its array type. Its size register does
            // not flow into it: copying the size in would make every array built from
            // one size register a copy of that register, so taint in one would reach the
            // others.
            Instruction::NewArray(f) => Self::allocation_exp(parser, f.idx),
            // A method handle that invokes a known method is a function reference to it; any
            // other method handle, and a method type constant, is an object of that class.
            Instruction::ConstMethodHandle(f) => {
                Some(self.method_handle_exp(parser, f.idx).unwrap_or_else(|| {
                    Exp::new_object_ref(CallObject::JavaObject(JavaClass(
                        "Ljava/lang/invoke/MethodHandle;".into(),
                    )))
                }))
            }
            Instruction::ConstMethodType(_) => Some(Exp::new_object_ref(CallObject::JavaObject(
                JavaClass("Ljava/lang/invoke/MethodType;".into()),
            ))),
            _ => None,
        } {
            for d in dest {
                stmts.push(Statement::new_kind(StatementKind::assign(
                    reg_to_var(code_item, d, locals),
                    [const_exp.clone()],
                )));
            }
            return Ok(());
        }

        match inst {
            Instruction::ArrayLength(f) => {
                let source = reg_to_var(code_item, f.b, locals);
                let dest = reg_to_var(code_item, f.a, locals);
                stmts.push(Statement::new_kind(StatementKind::load(
                    dest,
                    source,
                    ctadl_ir::cir::PathSegment::field("length"),
                )));
                return Ok(());
            }
            Instruction::SPut(f)
            | Instruction::SPutObject(f)
            | Instruction::SPutByte(f)
            | Instruction::SPutChar(f)
            | Instruction::SPutBoolean(f)
            | Instruction::SPutShort(f)
            | Instruction::SPutWide(f) => {
                let (global, field) = self.static_field(parser, f.idx.0)?;
                let temp_var = self.counter.temp(locals);
                // flow sources to temp
                stmts.push(Statement::new_kind(StatementKind::assign(
                    temp_var.clone(),
                    source
                        .into_iter()
                        .map(|r| reg_to_var(code_item, r, locals).into()),
                )));
                // flow temp into field update
                stmts.push(Statement::new_kind(StatementKind::store(
                    AccessPath::from(global),
                    field,
                    temp_var.into(),
                )));
                return Ok(());
            }
            Instruction::SGet(f)
            | Instruction::SGetObject(f)
            | Instruction::SGetByte(f)
            | Instruction::SGetChar(f)
            | Instruction::SGetBoolean(f)
            | Instruction::SGetShort(f)
            | Instruction::SGetWide(f) => {
                let (global, field) = self.static_field(parser, f.idx.0)?;
                for dest in dest
                    .iter()
                    .cloned()
                    .map(|d| reg_to_var(code_item, d, locals))
                {
                    stmts.push(Statement::new_kind(StatementKind::load(
                        dest,
                        global.clone(),
                        field.clone(),
                    )));
                }
                return Ok(());
            }
            Instruction::IPut(f)
            | Instruction::IPutBoolean(f)
            | Instruction::IPutByte(f)
            | Instruction::IPutChar(f)
            | Instruction::IPutShort(f)
            | Instruction::IPutObject(f)
            | Instruction::IPutWide(f) => {
                let temp_var = self.counter.temp(locals);
                // flow the stored value to temp. Take it from vA by position, not by filtering the
                // object out of `source`: in `iput-object p0, p0, F` the value is the object.
                let value = stored_value_regs(f.a, matches!(inst, Instruction::IPutWide(_)));
                stmts.push(Statement::new_kind(StatementKind::assign(
                    temp_var.clone(),
                    value
                        .into_iter()
                        .map(|r| reg_to_var(code_item, r, locals).into()),
                )));
                let object = reg_to_var(code_item, f.b, locals);
                let name = self.instance_field(parser, f.idx.0)?;
                // flow temp into field update
                stmts.push(Statement::new_kind(StatementKind::store(
                    AccessPath::from(object),
                    name,
                    temp_var.into(),
                )));
                return Ok(());
            }
            Instruction::IGet(f)
            | Instruction::IGetBoolean(f)
            | Instruction::IGetByte(f)
            | Instruction::IGetChar(f)
            | Instruction::IGetShort(f)
            | Instruction::IGetObject(f)
            | Instruction::IGetWide(f) => {
                let object = reg_to_var(code_item, f.b, locals);
                let name = self.instance_field(parser, f.idx.0)?;
                for dest in dest
                    .iter()
                    .cloned()
                    .map(|d| reg_to_var(code_item, d, locals))
                {
                    stmts.push(Statement::new_kind(StatementKind::load(
                        dest,
                        object.clone(),
                        name.clone(),
                    )));
                }
                return Ok(());
            }
            Instruction::AGet(f)
            | Instruction::AGetBoolean(f)
            | Instruction::AGetByte(f)
            | Instruction::AGetChar(f)
            | Instruction::AGetShort(f)
            | Instruction::AGetObject(f)
            | Instruction::AGetWide(f) => {
                let array_var = reg_to_var(code_item, f.b, locals);
                for d in dest.iter().cloned() {
                    let dest_var = reg_to_var(code_item, d, locals);
                    stmts.push(Statement::new_kind(StatementKind::load(
                        dest_var,
                        array_var.clone(),
                        ctadl_ir::cir::PathSegment::field("[]"),
                    )));
                }
                return Ok(());
            }
            Instruction::APut(f)
            | Instruction::APutBoolean(f)
            | Instruction::APutByte(f)
            | Instruction::APutChar(f)
            | Instruction::APutShort(f)
            | Instruction::APutObject(f)
            | Instruction::APutWide(f) => {
                let temp_var = self.counter.temp(locals);
                // As for iput: the value is vA by position, since it may share a register with
                // the array (`a[i] = a`) or the index (`a[i] = i`).
                let value = stored_value_regs(f.a, matches!(inst, Instruction::APutWide(_)));
                stmts.push(Statement::new_kind(StatementKind::assign(
                    temp_var.clone(),
                    value
                        .into_iter()
                        .map(|r| reg_to_var(code_item, r, locals).into()),
                )));
                let array_var = reg_to_var(code_item, f.b, locals);
                stmts.push(Statement::new_kind(StatementKind::store(
                    AccessPath::from(array_var),
                    ctadl_ir::cir::PathSegment::field("[]"),
                    temp_var.into(),
                )));
                return Ok(());
            }
            Instruction::Throw(f) => {
                let src_var = reg_to_var(code_item, f.a, locals);
                stmts.push(Statement::new_kind(StatementKind::assign(
                    Context::except(locals),
                    [src_var.into()],
                )));
                return Ok(());
            }
            _ => {
                // Flows all the sources to each dest; only their values, for arithmetic.
                let value_only = is_value_only(inst);
                if !source.is_empty() {
                    for dest in dest.iter() {
                        let dst_var = reg_to_var(code_item, *dest, locals);
                        stmts.push(Statement::new_kind(StatementKind::assign(
                            dst_var,
                            source.iter().map(|s| {
                                let var = reg_to_var(code_item, *s, locals);
                                if value_only {
                                    Exp::value(var)
                                } else {
                                    var.into()
                                }
                            }),
                        )));
                    }
                }
            }
        }

        // Handles moving results of calls into registers
        let is_move_result = inst.is_move_result();
        let is_move_exception = matches!(inst, Instruction::MoveException(_));

        let call_res = std::mem::take(&mut self.call_result);
        let catch_res = std::mem::take(&mut self.catch_result);

        if is_move_result {
            let result = call_res.unwrap_or_else(|| Context::ret(locals));
            for d_reg in dest.iter().cloned() {
                let src_exp = result.clone().into();
                let dst_var = reg_to_var(code_item, d_reg, locals);
                stmts.push(Statement::new_kind(StatementKind::assign(
                    dst_var,
                    [src_exp],
                )));
            }
        }

        if is_move_exception {
            let result = catch_res.unwrap_or_else(|| Context::except(locals));
            for d_reg in dest.iter().cloned() {
                let src_exp = result.clone().into();
                let dst_var = reg_to_var(code_item, d_reg, locals);
                stmts.push(Statement::new_kind(StatementKind::assign(
                    dst_var,
                    [src_exp],
                )));
            }
        }

        Ok(())
    }

    /// The `retval` temporary a call returns into and `move-result` reads.
    fn ret(locals: &mut Locals) -> VariableRef {
        VariableRef::new_local_idx(locals.get_or_intern("retval"))
    }

    fn except(locals: &mut Locals) -> VariableRef {
        VariableRef::new_local_idx(locals.get_or_intern("throwval"))
    }
}

/// One method body for [`Context::lower_job`] to lower, with everything it needs from the
/// bookkeeping pass of [`Importer::process`].
struct Job {
    /// The function the body goes into.
    fidx: FunctionIdx,
    sig: String,
    class: String,
    code: CodeItem,
    params: Params,
    /// Whether the body is replaced by an empty one (see [`merged`]).
    empty_union: bool,
    enclosing: Arc<EnclosingClass>,
    /// The function and class id of each clone of the body (see [`Importer::plan_clones`]).
    clones: Vec<(FunctionIdx, i64)>,
}

/// What [`Context::lower_job`] made of a [`Job`].
#[derive(Default)]
struct Lowered {
    /// Each function lowered, with the list in `spans` its statements' spans index.
    bodies: Vec<(FunctionIdx, FunctionData, usize)>,
    /// Instruction offsets, in the order lowering met them. A statement's span id is, for now,
    /// 1 + its position in its list.
    spans: Vec<Vec<u32>>,
    /// The globals the bodies use, numbered in the order they first used them.
    globals: ctadl_ir::cir::Globals,
    clone_statements: usize,
    /// Whether the method body is [`empty_body`]'s.
    empty_union: bool,
}

/// Rewrites a function lowered by [`Context::lower_job`] to the program's numbering: the span
/// ids, the globals, and -- for a body lowered on top of an earlier definition -- the locals.
struct Resolve<'a> {
    spans: &'a [source_info::FileSpanId],
    globals: &'a [GlobalIdx],
    /// Empty when the body keeps its own locals.
    locals: &'a [LocalIdx],
}

impl ctadl_ir::cir::visit::MutVisitor for Resolve<'_> {
    fn visit_statement(&mut self, statement: &mut Statement, location: ctadl_ir::cir::Location) {
        let span = &mut statement.source_info.span_id;
        if span.0 != 0 {
            *span = self.spans[span.0 as usize - 1];
        }
        self.super_statement(statement, location);
    }

    fn visit_variable_ref(&mut self, variable_ref: &mut VariableRef) {
        match &mut variable_ref.variable {
            Variable::Global(g) => *g = self.globals[g.index()],
            Variable::Local(l) if !self.locals.is_empty() => *l = self.locals[l.index()],
            _ => {}
        }
    }
}

/// Puts `body` into `slot` the way lowering it in place onto an earlier definition of the same
/// method did: [`Context::lower_code`] appends as many empty blocks as the body has, then fills
/// blocks from the first on, appending to their statements and replacing their terminators;
/// [`empty_body`] appends its one block. `body`'s locals are already `slot`'s.
fn graft(slot: &mut FunctionData, body: FunctionData, empty_union: bool) {
    slot.return_type = body.return_type;
    let mut body_blocks = body.blocks;
    let blocks = std::mem::take(body_blocks.blocks_mut());
    if empty_union {
        for data in blocks {
            slot.blocks.blocks_mut().push(data);
        }
        return;
    }
    for _ in 0..blocks.len() {
        slot.blocks.blocks_mut().push(BasicBlockData::new(None));
    }
    for (i, data) in blocks.into_iter().enumerate() {
        let BasicBlockData {
            statements,
            terminator,
        } = data;
        let dst = &mut slot.blocks[BasicBlockIdx::new(i)];
        for s in statements {
            dst.push_back(s);
        }
        dst.terminator = terminator;
    }
}

/// What [`Context::lower_code`] records about a body beyond the body itself, for building the
/// clones of a merged method out of one lowering of it ([`instantiate_clone`]).
#[derive(Debug)]
struct LowerInfo {
    /// Per block, the pc of its last instruction: where a narrowed switch would sit.
    last_pc: Vec<Option<usize>>,
    /// The block each instruction offset that starts a block starts.
    offset_to_bb: HashMap<usize, BasicBlockIdx>,
    /// Every `new-instance`, where its statement is.
    new_instances: Vec<NewInstanceSite>,
    /// Whether every `new-instance` lowered to the one assignment a retag would rewrite. When
    /// one did not (its type could not be read), clones are lowered in full instead.
    clonable: bool,
}

/// A `new-instance` in a lowered body: statement `stmt` of `block`, at `pc`, allocating `ty`.
#[derive(Debug)]
struct NewInstanceSite {
    block: BasicBlockIdx,
    stmt: usize,
    pc: usize,
    ty: TypeIdx,
}

/// One clone of a merged method, built from `template`, its unnarrowed and unretagged lowering:
/// each switch `narrow` names jumps straight to its arm, each `new-instance` allocates what
/// `allocation(pc, type)` says, and only the blocks reachable from the start are kept.
///
/// The result is exactly what lowering the clone with `narrow` and then running
/// [`ctadl_ir::ssa::prune_unreachable_nodes`] gives: the same blocks in the same order, found by
/// the same depth-first walk, and the same locals, since the full lowering interned every
/// local of the body either way.
fn instantiate_clone(
    template: &FunctionData,
    info: &LowerInfo,
    narrow: &HashMap<usize, usize>,
    mut allocation: impl FnMut(usize, TypeIdx) -> Option<Exp>,
) -> FunctionData {
    let blocks = &template.blocks;
    let n = ctadl_ir::graph::DirectedGraph::num_nodes(blocks);
    let narrowed: Vec<Option<BasicBlockIdx>> = (0..n)
        .map(|i| {
            let pc = info.last_pc.get(i).copied().flatten()?;
            info.offset_to_bb.get(narrow.get(&pc)?).copied()
        })
        .collect();
    let successors = |b: BasicBlockIdx| -> SmallVec<[BasicBlockIdx; 4]> {
        match narrowed[b.index()] {
            Some(arm) => smallvec![arm],
            None => blocks[b].terminator().successors().collect(),
        }
    };

    // The walk `ctadl_ir::graph::reachable` makes.
    let mut reach: Vec<BasicBlockIdx> = Vec::new();
    let mut seen = vec![false; n];
    let mut stack = vec![BasicBlockIdx::START_BLOCK];
    while let Some(b) = stack.pop() {
        if !std::mem::replace(&mut seen[b.index()], true) {
            reach.push(b);
            stack.extend(successors(b));
        }
    }
    // Pruning leaves the numbering alone when nothing is unreachable.
    if reach.len() == n {
        reach = (0..n).map(BasicBlockIdx::new).collect();
    }
    let mut renumber = vec![None; n];
    for (new, old) in reach.iter().enumerate() {
        renumber[old.index()] = Some(BasicBlockIdx::new(new));
    }

    let mut out = FunctionData {
        name: String::new(),
        params: template.params.clone(),
        return_type: template.return_type,
        blocks: Default::default(),
        locals: template.locals.clone(),
    };
    for &old in &reach {
        let mut data = blocks[old].clone();
        if let Some(arm) = narrowed[old.index()] {
            data.terminator = Some(Terminator::new_kind(TerminatorKind::Goto {
                targets: smallvec![arm],
            }));
        }
        if let Some(term) = &mut data.terminator
            && let TerminatorKind::Goto { targets } = &mut term.kind
        {
            for t in targets {
                *t = renumber[t.index()].expect("a successor of a reachable block is reachable");
            }
        }
        for site in info.new_instances.iter().filter(|s| s.block == old) {
            if let Some(stmt) = data.iter_mut().nth(site.stmt)
                && let StatementKind::Assign { sources, .. } = &mut stmt.kind
                && let Some(exp) = allocation(site.pc, site.ty)
            {
                sources[0] = exp;
            }
        }
        out.blocks.blocks_mut().push(data);
    }
    out
}

/// Whether `inst` computes its result from its operands' values and shares no memory with them
/// (S19): comparisons (`cmpl-float`..`cmp-long`, 0x2d..=0x31), negation and `not`
/// (0x7b..=0x80), binary arithmetic, bitwise and shift operations (`add-int`..`ushr-int/lit8`,
/// 0x90..=0xe2), and `instance-of` (0x20), whose boolean is a test of the reference, a
/// comparison. A numeric conversion (`int-to-long`..`int-to-short`, 0x81..=0x8f) is a cast, the
/// same value at another width, and stays a copy, as the JVM front end's does.
fn is_value_only(inst: &Instruction) -> bool {
    matches!(inst.discriminant(), 0x20 | 0x2d..=0x31 | 0x7b..=0x80 | 0x90..=0xe2)
}

/// The registers holding the value a store writes: `a`, or the pair `a, a+1` when `wide`.
fn stored_value_regs(a: Reg, wide: bool) -> SmallVec<[Reg; 2]> {
    if wide {
        smallvec![a, a.next_reg()]
    } else {
        smallvec![a]
    }
}

fn reg_to_var(code_item: &CodeItem, reg: Reg, locals: &mut Locals) -> VariableRef {
    if let Some(pidx) = code_item.reg_to_p(reg.0.try_into().expect("reg too big")) {
        VariableRef::new_parameter(pidx.into())
    } else {
        VariableRef::new_local_idx(locals.get_or_intern_numbered("v", reg.0))
    }
}

#[derive(Debug, Clone, Default)]
struct Counter {
    value: u32,
}

impl Counter {
    #[inline]
    fn reset(&mut self) {
        self.value = Default::default()
    }

    #[inline]
    fn next(&mut self) -> u32 {
        let v = self.value;
        self.value += 1u32;
        v
    }

    /// Make a new temporary with 't' prefix
    fn temp(&mut self, locals: &mut Locals) -> VariableRef {
        let i = self.next();
        VariableRef::new_local_idx(locals.get_or_intern_numbered("t", i))
    }
}
