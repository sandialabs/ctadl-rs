/*! JVM (.jar and .class) language front end.

Takes an artifact and returns a [`ctadl_ir::ProgramInfo`], and does nothing else: it does not
touch the store, choose a front end, or run the engine. Reading `.class` and `.jar` files is
`jvm-reader`'s job. What lives here is the lowering to CIR.

Conventions (S19). A field is the symbolic field `<Lcls;->name:desc>`, with no layout. A class
that owns a static field is a global named by the class, `$Lcls;`, and a static field is that
global's field named by the field's name alone:
`putstatic` is the store `$Lcls;.name = v`, `getstatic` the load `x = $Lcls;.name`. The owner is
the class the constant pool names. A class referenced from several imports is one global
(G3). An array's elements are the one field `[]`. Arithmetic,
bitwise operations, comparisons and `instanceof` supply values only (a numeric conversion is a cast and stays a copy): each
operand is `value(v)`. A `MethodHandle` constant that invokes a known method is a function
reference to that method (`Context::method_handle_exp` says which kinds do), and
`MethodHandle.invoke` or `invokeExact` is an indirect call through the handle, so the reference
resolves it. An exception is the last result
of every call and of every return; a handler reads it, and an exception a callee throws that no
handler in the caller catches is dropped rather than returned. A store to a parameter's local
slot assigns its `Param` variable, which SSA versions; version 0 is the incoming value.
*/

use std::path::Path;
use std::sync::Arc;

use hashbrown::HashSet;
use hashbrown::hash_map::HashMap;
use smallvec::{SmallVec, smallvec};
use source_info::{ArtifactKey, SpanLen};

use ctadl_import::error::{Error, ErrorContext};
use ctadl_ir::cir::call::{
    CallObject, Dispatch, FunctionNames, JavaClass, JavaDispatch, JavaMethod, JavaSignature,
    JavaSimpleName, VirtualMethodTable, is_method_handle_invoke,
};
use ctadl_ir::*;

use jvm_reader::flow::{CallInfo, CallKind, ConstantValue, DataflowInfo, Location};
use jvm_reader::{ClassFileError, ClassFileParser, JarFileParser};

const JVM_ACC_STATIC: u16 = 0x0008;
const JVM_ACC_NATIVE: u16 = 0x0100;
const JVM_ACC_INTERFACE: u16 = 0x0200;
const JVM_ACC_ABSTRACT: u16 = 0x0400;

/// JVM internal names (`java/lang/Object`, `MyInterface`) and type descriptors
/// (`LMyInterface;`) to the `L...;` symbol form used in CIR and CHA.
fn jvm_class_symbol(name: &str) -> String {
    if name.starts_with('L') && name.ends_with(';') {
        name.to_string()
    } else {
        format!("L{};", name)
    }
}

fn jvm_descriptor_to_params(descriptor: &str, is_instance: bool) -> Vec<ParameterType> {
    let mut params = Vec::new();
    if is_instance {
        params.push(ParameterType::Shared);
    }
    for p in jvm_reader::descriptor_parameter_info(descriptor) {
        match p.kind {
            jvm_reader::MethodParameterKind::Primitive => params.push(ParameterType::SharedFields),
            jvm_reader::MethodParameterKind::Reference => params.push(ParameterType::Shared),
        }
    }
    params
}

#[cfg(test)]
mod tests;

pub fn import_jar(file: &Path) -> Result<ProgramInfo, Error> {
    let timer = PhaseTimer::start();
    let parser =
        JarFileParser::open(file).err_context(|| format!("reading jar: {}", file.display()))?;
    timer.done("read and parse jar");
    log::info!(
        "{}: {} class file(s)",
        file.display(),
        parser.class_parsers().len()
    );
    let keep = one_definition_per_class(file, &parser)?;
    let path = file.to_string_lossy().to_string();
    let mut classes: Vec<Class> = parser
        .into_class_parsers()
        .into_iter()
        .zip(keep)
        .enumerate()
        .filter(|(_, (_, keep))| *keep)
        .map(|(sub_artifact_id, (parser, _))| Class {
            parser: Some(parser),
            key: ArtifactKey {
                path: path.clone(),
                sub_artifact_id: sub_artifact_id.try_into().unwrap(),
                hash: Vec::new(),
                encoding: source_info::ArtifactEncoding::Binary,
            },
        })
        .collect();
    let mut importer = Importer::default();
    let mut builders = ProgramInfoBuilder::new(VirtualMethodTable::new_java());
    importer
        .process(&mut classes, &mut builders)
        .err_context(|| format!("converting class in jar: {}", file.display()))?;
    drop(classes);
    timer.done("process");
    let info = importer.finish(builders);
    timer.done("finish");
    info
}

/// Logs how long each phase of an import took, under the `ctadl_jvm::timing` target at debug
/// level.
struct PhaseTimer(std::cell::Cell<std::time::Instant>);

impl PhaseTimer {
    fn start() -> Self {
        Self(std::cell::Cell::new(std::time::Instant::now()))
    }

    fn done(&self, phase: &str) {
        let now = std::time::Instant::now();
        let took = now - self.0.replace(now);
        log::debug!(target: "ctadl_jvm::timing", "{phase}: {took:.2?}");
    }
}

/// Which class files of the jar `parser` read to import: all but one definition of each class the
/// jar defines more than once are left out.
///
/// A program has one function per method, so a class can be imported only once. A jar defines a
/// class more than once when it is a multi-release jar, whose `META-INF/versions/<n>/` entries
/// replace a base entry on a Java `<n>` runtime, or when a fat jar was assembled from libraries
/// that bundle the same class. The definition kept is the first entry outside
/// `META-INF/versions/`, the one every runtime can load, or failing that the first entry. The
/// others are left out with a warning. Each class file keeps its own artifact key, so spans still
/// point into the entry the definition came from.
fn one_definition_per_class(file: &Path, parser: &JarFileParser) -> Result<Vec<bool>, Error> {
    let classes = parser.class_parsers();
    let entries = parser.entry_names();
    let is_versioned = |i: usize| entries[i].starts_with("META-INF/versions/");
    let mut kept: HashMap<&str, usize> = HashMap::new();
    for (i, class) in classes.iter().enumerate() {
        let name = class
            .class_name()
            .err_context(|| format!("converting class in jar: {}", file.display()))?;
        kept.entry(name)
            .and_modify(|k| {
                if is_versioned(*k) && !is_versioned(i) {
                    *k = i;
                }
            })
            .or_insert(i);
    }
    if kept.len() == classes.len() {
        return Ok(vec![true; classes.len()]);
    }
    log::warn!(
        "{}: leaving out {} class file(s) that define a class another entry defines too",
        file.display(),
        classes.len() - kept.len()
    );
    Ok(classes
        .iter()
        .enumerate()
        .map(|(i, class)| {
            let name = class.class_name().expect("read above");
            let k = kept[name];
            if k != i {
                log::info!(
                    "{}: leaving out {}, which defines {name} as {} does",
                    file.display(),
                    entries[i],
                    entries[k]
                );
            }
            k == i
        })
        .collect())
}

pub fn import_class(file: &Path) -> Result<ProgramInfo, Error> {
    let data =
        std::fs::read(file).err_context(|| format!("reading class file: {}", file.display()))?;
    let parser = ClassFileParser::parse(&data)
        .err_context(|| format!("parsing class file: {}", file.display()))?;
    let mut importer = Importer::default();
    let mut builders = ProgramInfoBuilder::new(VirtualMethodTable::new_java());
    let key = ArtifactKey {
        path: file.to_string_lossy().to_string(),
        sub_artifact_id: 0,
        hash: Vec::new(),
        encoding: source_info::ArtifactEncoding::Binary,
    };

    let mut classes = [Class {
        parser: Some(parser),
        key,
    }];
    importer
        .process(&mut classes, &mut builders)
        .err_context(|| format!("converting class file: {}", file.display()))?;
    importer.finish(builders)
}

/// A class file to import, with the key of the artifact it is in.
struct Class {
    /// `None` once the class's method bodies are lowered: [`Importer::lower_and_place`] frees each
    /// class file as soon as it is done with it, since all of them together are a sizable share
    /// of an import's memory.
    parser: Option<ClassFileParser>,
    key: ArtifactKey,
}

impl Class {
    fn parser(&self) -> &ClassFileParser {
        self.parser
            .as_ref()
            .expect("a class file is freed only once its method bodies are lowered")
    }
}

/// What spans the classes of an import: the program's globals, the externs the bodies call, and
/// the class headers. Lowering one method body at a time is [`Context`]'s.
#[derive(Debug, Default)]
struct Importer {
    /// The methods the bodies call, by signature, for the extern stubs. Merged from each body's
    /// [`Context::ext`] in program order, so that the last call in program order decides how a
    /// method is called, whatever the thread count.
    ext: HashMap<String, Extern>,
    /// The globals declared so far: one per class that owns a static field. Moved into the
    /// program at [`Importer::finish`].
    globals: cir::Globals,
    /// Each class this import defines, as its global is named, with the class file it is in and
    /// the offset of its header there: where the global is declared. Located in
    /// [`Importer::finish`], once every class has been lowered.
    class_headers: Vec<(String, ArtifactKey, u32)>,
}

/// A method some body calls, as [`Importer::finish`] stubs it if no class defines it: where its
/// signature `Lcls;->name(desc)` splits into class, simple name and descriptor, and whether it
/// takes a receiver.
#[derive(Clone, Copy, Debug)]
struct Extern {
    class_len: u32,
    name_len: u32,
    is_instance: bool,
}

impl Extern {
    /// The class, simple name and descriptor of the method `sig` names.
    fn split<'s>(&self, sig: &'s str) -> (&'s str, &'s str, &'s str) {
        let class_end = self.class_len as usize;
        let name_end = class_end + 2 + self.name_len as usize;
        (
            &sig[..class_end],
            &sig[class_end + 2..name_end],
            &sig[name_end..],
        )
    }
}

/// The lowering of method bodies into CIR: what one thread needs to lower one method at a time
/// ([`Context::lower_job`]). Everything that spans methods is [`Importer`]'s.
#[derive(Debug, Default)]
struct Context {
    /// The class whose method is being lowered.
    enclosing: Option<Arc<EnclosingClass>>,
    /// The globals the method being lowered uses, in the order it first uses them.
    globals: cir::Globals,
    /// The methods the method being lowered calls; see [`Importer::ext`].
    ext: HashMap<String, Extern>,
    /// The offset and length of each instruction lowered so far, in order. The statements of an
    /// instruction carry `1 +` its position here as their span id until [`Importer::place`]
    /// interns the real one.
    spans: Vec<(u32, u32)>,
}

/// What lowering an `invokespecial` needs to know about the class the call appears in.
#[derive(Debug, Default)]
struct EnclosingClass {
    name: Symbol,
    superclass: Option<Symbol>,
    /// The interfaces the class directly implements. An `X.super.m()` names one of these, and
    /// there lookup begins at the named interface rather than at the superclass.
    interfaces: Vec<Symbol>,
}

/// What the bookkeeping pass of [`Importer::process`] enters into the program and VMT for one
/// class, worked out from the class alone so that classes are described in parallel.
struct ClassRows {
    /// The class, as its global is named.
    name: String,
    header_offset: u32,
    is_interface: bool,
    /// The superclass, if there is one, then the interfaces the class directly implements.
    parents: SmallVec<[JavaClass; 2]>,
    enclosing: Arc<EnclosingClass>,
    methods: Vec<MethodRow>,
}

/// One method's function, without its body, and its VMT rows, each name interned once for all
/// the rows that name it.
struct MethodRow {
    function: FunctionData,
    owner: JavaClass,
    simple: JavaSimpleName,
    descriptor: JavaSignature,
    method: JavaMethod,
    access_flags: u16,
    has_code: bool,
}

impl ClassRows {
    fn describe(parser: &ClassFileParser) -> Result<Self, Error> {
        let class_def = parser.class_file();
        let class_name = jvm_class_symbol(parser.class_name()?);
        log::trace!("Class: {}", class_name);
        let superclass = if class_def.super_class != 0 {
            parser
                .get_class_name(class_def.super_class)
                .ok()
                .map(|name| JavaClass(jvm_class_symbol(name).into()))
        } else {
            None
        };
        log::trace!(
            "Superclass: {}",
            match superclass {
                Some(JavaClass(ref s)) => s.to_string(),
                None => "None".to_string(),
            }
        );
        let mut parents = SmallVec::new();
        if let Some(jc) = superclass.clone() {
            parents.push(jc)
        };
        let mut direct_interfaces = Vec::new();
        for type_idx in &class_def.interfaces {
            let iface = jvm_class_symbol(parser.get_class_name(*type_idx).ok().unwrap());
            parents.push(JavaClass(iface.clone().into()));
            direct_interfaces.push(Symbol::from(iface.as_str()));
            log::trace!("Interface: {}", iface);
        }
        let enclosing = Arc::new(EnclosingClass {
            name: Symbol::from(class_name.as_str()),
            superclass: superclass.map(|c| c.0),
            interfaces: direct_interfaces,
        });

        let mut methods = Vec::with_capacity(class_def.methods.len());
        for enc in &class_def.methods {
            let method_name = class_def.get_utf8(enc.name_index)?;
            // The descriptor alone, e.g. "(I)I".
            let java_sig = class_def.get_utf8(enc.descriptor_index)?;
            let full_name = format!("{class_name}->{method_name}{java_sig}");
            let method = JavaMethod(Symbol::from(full_name.as_str()));
            let mut function = FunctionData {
                name: full_name,
                // All JVM functions model (normal_return, exception_return) like DEX.
                return_type: ReturnType { arity: 2 },
                ..Default::default()
            };
            let is_instance = enc.access_flags & JVM_ACC_STATIC == 0;
            for p in jvm_descriptor_to_params(java_sig, is_instance) {
                function.params.push(p);
            }
            if enc.code.is_none() {
                log::trace!("No code for function {}", method_name)
            }
            methods.push(MethodRow {
                function,
                owner: JavaClass(enclosing.name.clone()),
                simple: JavaSimpleName(Symbol::from(method_name)),
                descriptor: JavaSignature(Symbol::from(java_sig)),
                method,
                access_flags: enc.access_flags,
                has_code: enc.code.is_some(),
            });
        }
        Ok(Self {
            name: class_name,
            header_offset: class_def.header_offset,
            is_interface: class_def.access_flags & JVM_ACC_INTERFACE != 0,
            parents,
            enclosing,
            methods,
        })
    }
}

/// One method body for [`Context::lower_job`] to lower, with what the bookkeeping pass of
/// [`Importer::process`] found out about it.
struct Job {
    /// The index of the class file the method is in.
    class: usize,
    /// The index of the method among the class's.
    method: usize,
    /// The function the body goes into.
    fidx: FunctionIdx,
    enclosing: Arc<EnclosingClass>,
}

/// What [`Context::lower_job`] made of a [`Job`]: the body's blocks and locals, with its spans
/// and globals numbered as [`Context`] describes.
struct Lowered {
    body: FunctionData,
    spans: Vec<(u32, u32)>,
    globals: cir::Globals,
    ext: HashMap<String, Extern>,
}

/// A lowered body with the program's id for each of its spans and globals, as
/// [`Importer::intern`] interned them.
struct Placed {
    fidx: FunctionIdx,
    body: FunctionData,
    spans: Vec<source_info::FileSpanId>,
    globals: Vec<GlobalIdx>,
}

/// Rewrites each body in `placed` to the program's ids, in parallel.
fn resolve(placed: &mut [Placed]) {
    use rayon::prelude::*;
    placed.par_iter_mut().for_each(|p| {
        let mut resolve = Resolve {
            spans: &p.spans,
            globals: &p.globals,
        };
        cir::visit::MutVisitor::visit_function_data(&mut resolve, p.fidx, &mut p.body);
    });
}

/// Moves each body in `placed`, once [`resolve`]d, into its function.
fn put(program: &mut Program, placed: Vec<Placed>) {
    for p in placed {
        let slot = &mut program[p.fidx];
        slot.blocks = p.body.blocks;
        slot.locals = p.body.locals;
    }
}

/// Rewrites a body lowered by [`Context::lower_job`] to the program's span ids and globals.
struct Resolve<'a> {
    spans: &'a [source_info::FileSpanId],
    globals: &'a [GlobalIdx],
}

impl cir::visit::MutVisitor for Resolve<'_> {
    fn visit_statement(&mut self, statement: &mut Statement, location: cir::Location) {
        let span = &mut statement.source_info.span_id;
        if span.0 != 0 {
            *span = self.spans[span.0 as usize - 1];
        }
        self.super_statement(statement, location);
    }

    fn visit_variable_ref(&mut self, variable_ref: &mut VariableRef) {
        if let Variable::Global(g) = &mut variable_ref.variable {
            *g = self.globals[g.index()];
        }
    }
}

type DupSlotPair = (u32, u32);

impl Importer {
    /// Imports `classes`, each with the key of the artifact it is in.
    ///
    /// Three passes. The first does all the bookkeeping -- function slots, VMT rows, parameters
    /// -- collecting a [`Job`] per method body: it describes the classes in parallel
    /// ([`ClassRows::describe`]) and enters them into the program in class order. The second lowers the bodies on the current rayon pool, each into a function of its
    /// own ([`Context::lower_job`]). The third places them into the program in order
    /// ([`Importer::place`]), interning their spans and globals in exactly the order lowering
    /// them one after another did, so the result does not depend on the thread count.
    fn process(
        &mut self,
        classes: &mut [Class],
        builders: &mut ProgramInfoBuilder,
    ) -> Result<(), Error> {
        use rayon::prelude::*;

        let timer = PhaseTimer::start();
        let described: Vec<Result<ClassRows, Error>> = classes
            .par_iter()
            .map(|c| ClassRows::describe(c.parser()))
            .collect();
        let mut jobs = Vec::new();
        for (class, (c, rows)) in classes.iter().zip(described).enumerate() {
            let ClassRows {
                name,
                header_offset,
                is_interface,
                parents,
                enclosing,
                methods: method_rows,
            } = rows?;
            if let Dispatch::Java {
                hierarchy,
                interfaces,
                ..
            } = &mut builders.vmt.dispatch
            {
                let class = JavaClass(enclosing.name.clone());
                if is_interface {
                    interfaces.push(class.clone());
                }
                hierarchy.insert(class, parents);
            }
            self.class_headers
                .push((name, c.key.clone(), header_offset));
            for (method_index, row) in method_rows.into_iter().enumerate() {
                let MethodRow {
                    function,
                    owner,
                    simple,
                    descriptor,
                    method,
                    access_flags,
                    has_code,
                } = row;
                let fidx = builders.program.functions.functions.push(function);
                builders.vmt.names.push(
                    FunctionNames::new(method.0.clone(), simple.0.clone())
                        .with_parent(owner.0.clone())
                        .with_signature(descriptor.0.clone()),
                );
                let Dispatch::Java {
                    methods,
                    abstract_methods,
                    natives,
                    ..
                } = &mut builders.vmt.dispatch
                else {
                    unreachable!("a JVM import's VMT is Java's");
                };
                methods.push((
                    owner.clone(),
                    simple.clone(),
                    descriptor.clone(),
                    method.clone(),
                ));
                if access_flags & JVM_ACC_ABSTRACT != 0 {
                    abstract_methods.push((owner.clone(), simple.clone(), descriptor.clone()));
                }
                // Native methods are also listed in `natives`. They are already in `methods`
                // above: this frontend walks every declared method, code or not.
                if access_flags & JVM_ACC_NATIVE != 0 {
                    let is_static = access_flags & JVM_ACC_STATIC != 0;
                    natives.push((owner, simple, descriptor, method, is_static));
                }
                if has_code {
                    jobs.push(Job {
                        class,
                        method: method_index,
                        fidx,
                        enclosing: Arc::clone(&enclosing),
                    });
                }
            }
        }
        timer.done(&format!("bookkeeping, {} jobs", jobs.len()));
        self.lower_and_place(classes, builders, &jobs);
        timer.done("lower and place");
        Ok(())
    }

    /// Lowers `jobs` on the current rayon pool and places the results into the program in
    /// order.
    ///
    /// The jobs go in batches, so that only a couple of batches of lowered bodies wait to be
    /// placed at a time. Placing a batch is three steps: interning its spans and globals
    /// ([`Importer::intern`]), which is serial and must go in program order; rewriting its bodies
    /// to the ids interned ([`resolve`]); and moving them into the program ([`put`]). While one
    /// batch is interned, the next is lowered and the one before is rewritten. Once no batch
    /// still to be lowered needs a class file, it is freed.
    fn lower_and_place(
        &mut self,
        classes: &mut [Class],
        builders: &mut ProgramInfoBuilder,
        jobs: &[Job],
    ) {
        use rayon::prelude::*;

        /// Bodies per batch: enough to keep every thread busy across uneven method sizes, few
        /// enough that a batch of lowered bodies is a small part of the program.
        const BATCH: usize = 4096;

        let lower = |classes: &[Class], batch: &[Job]| -> Vec<Result<Lowered, ClassFileError>> {
            batch
                .par_iter()
                .map_init(Context::default, |ctx, job| {
                    ctx.lower_job(classes[job.class].parser(), job)
                })
                .collect()
        };
        let mut file = None;
        let mut freed = 0;
        let mut batches = jobs.chunks(BATCH);
        let mut batch = batches.next();
        let mut current = batch.map(|batch| lower(classes, batch));
        let mut interned = Vec::new();
        let (mut interning, mut total) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
        while let (Some(mut results), Some(placing)) = (current.take(), batch) {
            batch = batches.next();
            let started = std::time::Instant::now();
            let shared: &[Class] = classes;
            let ProgramInfoBuilder {
                program,
                source_info_builder,
                ..
            } = &mut *builders;
            let mut previous = std::mem::take(&mut interned);
            // Merged in program order, beside the interning rather than in it.
            let called: Vec<_> = results
                .iter_mut()
                .filter_map(|lowered| Some(std::mem::take(&mut lowered.as_mut().ok()?.ext)))
                .collect();
            let ext = &mut self.ext;
            rayon::scope(|scope| {
                scope.spawn(|_| current = batch.map(|batch| lower(shared, batch)));
                scope.spawn(|_| resolve(&mut previous));
                scope.spawn(|_| called.into_iter().for_each(|called| ext.extend(called)));
                interned = Self::intern(
                    &mut self.globals,
                    shared,
                    placing,
                    results,
                    &mut file,
                    source_info_builder,
                    program,
                );
                interning += started.elapsed();
            });
            put(program, previous);
            total += started.elapsed();
            // The batch after `batch` is the next to read the class files.
            let needed = batches
                .clone()
                .next()
                .map_or(classes.len(), |next| next[0].class);
            for class in &mut classes[freed..needed] {
                class.parser = None;
            }
            freed = needed;
        }
        resolve(&mut interned);
        put(&mut builders.program, interned);
        log::debug!(
            target: "ctadl_jvm::timing",
            "interning: {interning:.2?} of {total:.2?} lowering and placing"
        );
    }

    /// Interns the spans of a batch of lowered bodies and declares their globals in `globals`, in
    /// the order lowering them in place would have. A body jvm-reader could not decode is left
    /// out, which leaves its function a declaration, so calls to it still resolve.
    ///
    /// `file` is the class file whose spans were interned last, with its id.
    fn intern(
        globals_table: &mut cir::Globals,
        classes: &[Class],
        jobs: &[Job],
        results: Vec<Result<Lowered, ClassFileError>>,
        file: &mut Option<(usize, source_info::FileId)>,
        source_info_builder: &mut source_info::SourceInfoBuilder,
        program: &Program,
    ) -> Vec<Placed> {
        let mut placed = Vec::with_capacity(results.len());
        for (job, lowered) in jobs.iter().zip(results) {
            let Lowered {
                body,
                spans,
                globals,
                ext: _,
            } = match lowered {
                Ok(lowered) => lowered,
                Err(e) => {
                    let name = &program[job.fidx].name;
                    log::warn!("{name}: importing without a body: {e}");
                    continue;
                }
            };
            let spans: Vec<source_info::FileSpanId> = if spans.is_empty() {
                Vec::new()
            } else {
                // Looked up at the class's first span, not before: that is when interning a span
                // in it numbered the artifact.
                let file = match *file {
                    Some((class, file)) if class == job.class => file,
                    _ => {
                        let id = source_info_builder.file_for(&classes[job.class].key);
                        *file = Some((job.class, id));
                        id
                    }
                };
                spans
                    .iter()
                    .map(|&(start, len)| {
                        source_info_builder.span_in_file(file, start, SpanLen::ByteLen(len))
                    })
                    .collect()
            };
            let globals: Vec<GlobalIdx> = globals
                .iter_enumerated()
                .map(|(_, g)| globals_table.declare(&*g.name, None))
                .collect();
            placed.push(Placed {
                fidx: job.fidx,
                body,
                spans,
                globals,
            });
        }
        placed
    }

    fn finish(&mut self, mut builders: ProgramInfoBuilder) -> Result<ProgramInfo, Error> {
        let mut program = builders.program;
        program.globals = std::mem::take(&mut self.globals);
        // A class's global is declared at its class header, if the program uses it.
        for (class, key, offset) in self.class_headers.drain(..) {
            if program.globals.lookup(&class).is_some() {
                let span = builders.source_info_builder.span_for(
                    key,
                    offset,
                    SpanLen::ByteLen(jvm_reader::types::ClassFile::HEADER_LEN),
                );
                program.globals.declare(&class, Some(span));
            }
        }
        // Each extern gets a function, so the externs are taken in signature order: the hash
        // map's order would number the functions differently from one import to the next.
        let mut ext: Vec<_> = self.ext.drain().collect();
        ext.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
        if let Dispatch::Java { methods, .. } = &mut builders.vmt.dispatch {
            let defined: HashSet<JavaMethod> = methods.iter().map(|m| m.3.clone()).collect();
            for (sig, entry) in ext {
                let method = JavaMethod(sig.as_str().into());
                if defined.contains(&method) {
                    log::trace!("skipping defined method: {sig}");
                    continue;
                }
                log::trace!("adding external method: {sig}");
                let (class, name, descr) = entry.split(&sig);
                methods.push((
                    JavaClass(class.into()),
                    JavaSimpleName(name.into()),
                    JavaSignature(descr.into()),
                    method,
                ));
                builders.vmt.names.push(
                    FunctionNames::new(sig.as_str(), name)
                        .with_parent(class)
                        .with_signature(descr),
                );
                // Add empty definition
                let fidx = program.new_function();
                let fdat = &mut program.functions[fidx];
                for p in jvm_descriptor_to_params(descr, entry.is_instance) {
                    fdat.params.push(p);
                }
                fdat.return_type = ReturnType { arity: 2 };
                fdat.name = sig;
            }
        }

        log::trace!("program: {program}");
        // Verify the generated program.
        program.verify()?;

        builders.vmt.finish_names();
        let source_info = builders.source_info_builder.finish();
        log::trace!("source_info: {source_info}");
        let vmt = builders.vmt;
        Ok(ProgramInfo {
            program,
            vmt,
            source_info,
        })
    }
}

impl Context {
    /// Where an `invokespecial` naming `cls.name` begins method lookup.
    ///
    /// Under `ACC_SUPER` a `super.m()` starts at the superclass of the class declaring the
    /// current method, not at the class the instruction names. Three cases start at the named
    /// class instead: `X.super.m()`, whose reference names an interface the enclosing class
    /// implements directly; a constructor, whose target class is exact; and a private call,
    /// which names the current class.
    ///
    /// `None` when the enclosing class is unknown or has no superclass, which leaves the site
    /// on the ordinary resolvent set.
    fn super_start(&self, cls: &str, name: &str) -> Option<Symbol> {
        let enclosing = self.enclosing.as_ref()?;
        if name == "<init>"
            || &*enclosing.name == cls
            || enclosing.interfaces.iter().any(|i| &**i == cls)
        {
            return Some(Symbol::from(cls));
        }
        enclosing.superclass.clone()
    }

    /// Lowers the body of `job`'s method into a function of its own. Spans are recorded as
    /// instruction offsets and globals declared in a table of the job's own;
    /// [`Importer::place`] resolves both. An error is jvm-reader's, decoding the method.
    fn lower_job(
        &mut self,
        parser: &ClassFileParser,
        job: &Job,
    ) -> Result<Lowered, ClassFileError> {
        self.enclosing = Some(Arc::clone(&job.enclosing));
        self.spans.clear();
        self.globals = Default::default();
        self.ext.clear();
        let method = &parser.class_file().methods[job.method];
        let code_attr = method.code.as_ref().expect("a job's method has code");
        let basic_blocks = parser
            .basic_blocks_with_stack_slots(method)?
            .expect("Non-empty function");
        let mut fdat = FunctionData::default();
        fdat.blocks
            .blocks_mut()
            .raw
            .reserve_exact(basic_blocks.blocks().len());

        let handler_pcs: HashSet<u32> = code_attr
            .exception_table
            .iter()
            .map(|e| e.handler_pc as u32)
            .collect();

        let mut dup_slot_pairs: HashSet<DupSlotPair> = HashSet::new();
        // The statements of the block being lowered, moved into it in one exactly sized
        // allocation once the block is done, rather than grown by doubling.
        let mut block_stmts: Vec<Statement> = Vec::new();

        for bb in basic_blocks.blocks() {
            // Exception handler entry: model MoveException (thrown value → stack slot).
            if handler_pcs.contains(&bb.start_pc) {
                let dest = VariableRef::new_local_idx(fdat.locals.get_or_intern("stack0"));
                let except = Self::except(&mut fdat.locals);
                block_stmts.push(Statement::new_kind(StatementKind::Assign {
                    dest: dest.into(),
                    sources: smallvec![Exp::from(AccessPath::from(except),)],
                }));
            }

            let block_instrs = bb.instructions(&basic_blocks);

            // Add statements to the basic block
            for instr in block_instrs.iter() {
                self.spans.push((instr.file_byte_offset, instr.byte_length));
                let source_info = SourceInfo::new(source_info::FileSpanId(
                    self.spans.len().try_into().unwrap(),
                ));
                if let Some(call_info) = &instr.call {
                    let mut stmt = self
                        .decode_call(call_info, &mut fdat.locals)
                        .expect("Call should be there");
                    stmt.source_info = source_info;
                    block_stmts.push(stmt);
                    // A long or double result takes two slots, and each carries the value, as
                    // a `lload` writes both.
                    for ret_loc in Self::call_result_slots(call_info) {
                        let dest = self.convert_location_to_var_ref(&ret_loc, &mut fdat.locals);
                        let ret = Self::ret(&mut fdat.locals);
                        let mut assign = Statement::new_kind(StatementKind::Assign {
                            dest: dest.into(),
                            sources: smallvec![Exp::from(AccessPath::from(ret),)],
                        });
                        assign.source_info = source_info;
                        block_stmts.push(assign);
                    }
                    if let Some(mut link) =
                        Self::init_survivor_link(call_info, &dup_slot_pairs, &mut fdat.locals)
                    {
                        link.source_info = source_info;
                        block_stmts.push(link);
                    }
                }
                if Self::is_stack_shuffle_opcode(instr.opcode) {
                    let stmts = self.stack_shuffle_statements(&instr.dataflow, &mut fdat.locals);
                    Self::record_dup_slot_pairs(&mut dup_slot_pairs, &instr.dataflow);
                    for mut stmt in stmts {
                        stmt.source_info = source_info;
                        block_stmts.push(stmt);
                    }
                } else {
                    for df in &instr.dataflow {
                        for mut stmt in
                            self.dataflow_to_statements(instr.opcode, df, &mut fdat.locals)
                        {
                            stmt.source_info = source_info;
                            block_stmts.push(stmt);
                        }
                    }
                }
            }

            let empty_exp = || Exp::new_bytes(Vec::new());
            let term = match block_instrs.last() {
                Some(instr) => match instr.opcode {
                    0xbf => {
                        let succs = bb
                            .successors
                            .iter()
                            .map(|&b| BasicBlockIdx::new(b))
                            .collect::<SmallVec<[BasicBlockIdx; 4]>>();
                        if succs.is_empty() {
                            let except = Self::except(&mut fdat.locals);
                            TerminatorKind::Return {
                                args: smallvec![empty_exp(), Exp::from(AccessPath::from(except)),],
                            }
                        } else {
                            TerminatorKind::Goto { targets: succs }
                        }
                    }
                    0xac..=0xb0 => TerminatorKind::Return {
                        args: smallvec![
                            self.convert_location_to_exp(
                                instr
                                    .return_value
                                    .as_ref()
                                    .expect("a value return names its value"),
                                &mut fdat.locals,
                            ),
                            empty_exp(),
                        ],
                    },
                    0xb1 => {
                        let e = empty_exp();
                        TerminatorKind::Return {
                            args: smallvec![e.clone(), e],
                        }
                    }
                    _ => {
                        if bb.successors.is_empty() {
                            let e = empty_exp();
                            TerminatorKind::Return {
                                args: smallvec![e.clone(), e],
                            }
                        } else {
                            TerminatorKind::Goto {
                                targets: bb
                                    .successors
                                    .iter()
                                    .map(|&b| BasicBlockIdx::new(b))
                                    .collect::<SmallVec<[BasicBlockIdx; 4]>>(),
                            }
                        }
                    }
                },
                None => {
                    let e = empty_exp();
                    TerminatorKind::Return {
                        args: smallvec![e.clone(), e],
                    }
                }
            };
            let mut bb_data = BasicBlockData::new(Some(Terminator::new_kind(term)));
            bb_data.statements = block_stmts.drain(..).collect();
            fdat.blocks.blocks_mut().push(bb_data);
        }
        fdat.locals.drop_index();
        Ok(Lowered {
            body: fdat,
            spans: std::mem::take(&mut self.spans),
            globals: std::mem::take(&mut self.globals),
            ext: std::mem::take(&mut self.ext),
        })
    }

    /// Records the method `java_sig`, which is `class_name->method_name` followed by `descr`, as
    /// one the body calls.
    fn declare_extern(
        &mut self,
        java_sig: &str,
        class_name: &str,
        method_name: &str,
        descr: &str,
        is_instance: bool,
    ) {
        debug_assert_eq!(
            java_sig.len(),
            class_name.len() + 2 + method_name.len() + descr.len()
        );
        let entry = Extern {
            class_len: class_name.len().try_into().unwrap(),
            name_len: method_name.len().try_into().unwrap(),
            is_instance,
        };
        match self.ext.get_mut(java_sig) {
            Some(found) => *found = entry,
            None => {
                self.ext.insert(java_sig.to_owned(), entry);
            }
        }
    }

    fn decode_call(&mut self, call: &CallInfo, locals: &mut Locals) -> Option<Statement> {
        // Get call target
        let style = match &call.receiver {
            None => {
                match call.call_kind {
                    CallKind::Dynamic => {
                        // I don't know why dynamic calls with no receiver exist, but apparently makeConcatWithConstants does this
                        let class_name = "L?unknown;";
                        let method_name = call.dynamic_name.as_ref().unwrap();
                        let descr = call.dynamic_type.as_ref().unwrap();
                        let java_sig = class_name.to_owned() + "->" + method_name + descr;
                        self.declare_extern(&java_sig, class_name, method_name, descr, false);
                        CallStyle::DirectCall {
                            call_edges: CallEdges::Explicit([java_sig].into_iter().collect()),
                        }
                    }
                    CallKind::Interface | CallKind::Special | CallKind::Virtual => {
                        let target = call.target.as_ref().expect("call target");
                        let class_name = "L".to_owned() + &target.class_name + ";";
                        let method_name = &target.method_name;
                        let descr = &target.descriptor;
                        let java_sig = class_name.to_owned() + "->" + method_name + descr;
                        self.declare_extern(&java_sig, &class_name, method_name, descr, true);
                        CallStyle::DirectCall {
                            call_edges: CallEdges::Explicit([java_sig].into_iter().collect()),
                        }
                    }
                    CallKind::Static => {
                        let class_name =
                            "L".to_owned() + &call.target.as_ref().unwrap().class_name + ";";
                        let method_name = &call.target.as_ref().unwrap().method_name;
                        let descr = &call.target.as_ref().unwrap().descriptor;
                        let java_sig = class_name.to_owned() + "->" + method_name + descr;
                        self.declare_extern(&java_sig, &class_name, method_name, descr, false);
                        CallStyle::DirectCall {
                            call_edges: CallEdges::Explicit([java_sig].into_iter().collect()),
                        }
                    }
                }
            }
            Some(recv) => {
                match call.call_kind {
                    // Java invokedynamic calls have a bootstrap method index and dynamic name/type
                    // I'm not entirely sure what these are supposed to look like
                    CallKind::Dynamic => {
                        let class_name =
                            "L".to_owned() + &call.target.as_ref().unwrap().class_name + ";";
                        let method_name = &call.target.as_ref().unwrap().method_name;
                        let descr = call.dynamic_type.as_ref().unwrap();
                        let java_sig = class_name.to_owned() + "->" + method_name + descr;
                        self.declare_extern(&java_sig, &class_name, method_name, descr, true);
                        CallStyle::JavaCall {
                            receiver: self.convert_location_to_var_ref(recv, locals),
                            cls: class_name.clone().into(),
                            simple_name: method_name.clone().into(),
                            descriptor: descr.clone().into(),
                            // `invokedynamic` has no dispatch instruction to read: where it
                            // goes is decided by a bootstrap method at run time. Recorded as
                            // unknown rather than guessed at.
                            dispatch: JavaDispatch::Unknown,
                            super_start: None,
                        }
                    }
                    // other calls have a class name, method name, and descriptor
                    _ => {
                        let class_name =
                            "L".to_owned() + &call.target.as_ref().unwrap().class_name + ";";
                        let method_name = &call.target.as_ref().unwrap().method_name;
                        let descr = &call.target.as_ref().unwrap().descriptor;
                        // Invoking a method handle calls whatever the handle refers to: an
                        // indirect call through the handle, whose function reference resolves
                        // it (S11), with the arguments passed straight through.
                        if matches!(call.call_kind, CallKind::Virtual)
                            && is_method_handle_invoke(&class_name, method_name)
                        {
                            return Some(self.call_statement(
                                CallStyle::FuncPtrCall {
                                    callee: AccessPath::from(
                                        self.convert_location_to_var_ref(recv, locals),
                                    ),
                                    signature: None,
                                },
                                call,
                                locals,
                            ));
                        }
                        let java_sig = class_name.to_owned() + "->" + method_name + descr;
                        self.declare_extern(&java_sig, &class_name, method_name, descr, true);
                        CallStyle::JavaCall {
                            receiver: self.convert_location_to_var_ref(recv, locals),
                            cls: class_name.clone().into(),
                            simple_name: method_name.clone().into(),
                            descriptor: descr.clone().into(),
                            dispatch: match call.call_kind {
                                CallKind::Interface => JavaDispatch::Interface,
                                // `invokespecial` with a receiver: a `super.m()`, a private
                                // method, or a constructor. All three are fixed at the named
                                // class rather than found from the receiver.
                                CallKind::Special => JavaDispatch::Super,
                                CallKind::Virtual => JavaDispatch::Virtual,
                                // Unreachable: `Dynamic` is the arm above and `Static` has no
                                // receiver. Recorded honestly rather than asserted, since a
                                // new `CallKind` should not make this arm lie.
                                CallKind::Dynamic | CallKind::Static => JavaDispatch::Unknown,
                            },
                            super_start: match call.call_kind {
                                CallKind::Special => self.super_start(&class_name, method_name),
                                _ => None,
                            },
                        }
                    }
                }
            }
        };
        Some(self.call_statement(style, call, locals))
    }

    /// The call `style` with `call`'s arguments, returning into the result and exception
    /// temporaries.
    fn call_statement(
        &mut self,
        style: CallStyle,
        call: &CallInfo,
        locals: &mut Locals,
    ) -> Statement {
        let args: ctadl_ir::ThinVec<Exp> = call
            .arguments
            .iter()
            .map(|x| self.convert_location_to_exp(x, locals))
            .collect();

        let retval = Self::ret(locals);
        let throwval = Self::except(locals);

        Statement::new_kind(StatementKind::CallAssign {
            style,
            rets: ctadl_ir::thin_vec![retval, throwval],
            args,
        })
    }

    fn ret(locals: &mut Locals) -> VariableRef {
        VariableRef::new_local_idx(locals.get_or_intern("retval"))
    }

    fn except(locals: &mut Locals) -> VariableRef {
        VariableRef::new_local_idx(locals.get_or_intern("throwval"))
    }

    /// The stack slots a call's result lands in: none for `void`, the lower
    /// and upper slot for a long or double.
    fn call_result_slots(call: &CallInfo) -> Vec<Location> {
        let Some(Location::StackSlot(n)) = call.return_value else {
            return call.return_value.iter().cloned().collect();
        };
        let descriptor = match call.call_kind {
            CallKind::Dynamic => call.dynamic_type.as_deref(),
            _ => call.target.as_ref().map(|t| t.descriptor.as_str()),
        };
        let slots = descriptor.map_or(1, jvm_reader::flow::descriptor_return_slot_count);
        (0..slots.max(1) as u32)
            .map(|i| Location::StackSlot(n + i))
            .collect()
    }

    fn allocation_exp(class_name: &str) -> Exp {
        let jclass = if class_name.starts_with('L') || class_name.starts_with('[') {
            class_name.to_string()
        } else {
            format!("L{};", class_name)
        };
        Exp::new_object_ref(CallObject::JavaObject(JavaClass(jclass.into())))
    }

    /// The class global `$Lcls;` and field `name` of the static field `f`.
    fn static_field(&mut self, f: &jvm_reader::flow::FieldRef) -> (VariableRef, cir::PathSegment) {
        let class = format!("L{};", f.class_name);
        let global = self.globals.declare(&class, None);
        (
            VariableRef::global(global),
            cir::PathSegment::field(f.field_name.as_str()),
        )
    }

    fn jvm_field_symbol(f: &jvm_reader::flow::FieldRef) -> cir::PathSegment {
        let class = format!("L{};", f.class_name);
        cir::PathSegment::field(format!("<{}->{}:{}>", class, f.field_name, f.descriptor))
    }

    fn stack_exp(&self, loc: &Location, locals: &mut Locals) -> Exp {
        Exp::from(AccessPath::from(
            self.convert_location_to_var_ref(loc, locals),
        ))
    }

    fn field_ref_in(locs: &[Location]) -> Option<&jvm_reader::flow::FieldRef> {
        locs.iter().find_map(|l| match l {
            Location::FieldRef(f) => Some(f),
            _ => None,
        })
    }

    fn stack_sources(locs: &[Location]) -> impl Iterator<Item = &Location> {
        locs.iter()
            .filter(|l| matches!(l, Location::StackSlot(_) | Location::StackInput(_)))
    }

    fn array_base(locs: &[Location]) -> Option<&Location> {
        locs.iter().find_map(|l| match l {
            Location::ArrayElement { base, .. } => Some(base.as_ref()),
            _ => None,
        })
    }

    /// Stack-manipulation opcodes: `dup*` and `swap` (JVMS 6.5).
    fn is_stack_shuffle_opcode(opcode: u8) -> bool {
        matches!(opcode, 0x59..=0x5f)
    }

    fn assign_var(dest: VariableRef, src: VariableRef) -> Statement {
        Statement::new_kind(StatementKind::Assign {
            dest: dest.into(),
            sources: smallvec![Exp::from(AccessPath::from(src))],
        })
    }

    fn stack_slot_index(loc: &Location) -> Option<u32> {
        match loc {
            Location::StackSlot(n) => Some(*n),
            _ => None,
        }
    }

    fn record_dup_slot_pair(pairs: &mut HashSet<DupSlotPair>, a: u32, b: u32) {
        if a != b {
            pairs.insert((a.min(b), a.max(b)));
        }
    }

    /// Record each pair of outputs that are copies of one value: they have the
    /// same sources.
    fn record_dup_slot_pairs(pairs: &mut HashSet<DupSlotPair>, dataflow: &[DataflowInfo]) {
        for (i, a) in dataflow.iter().enumerate() {
            for b in &dataflow[i + 1..] {
                if a.sources != b.sources {
                    continue;
                }
                if let (Some(x), Some(y)) = (
                    Self::stack_slot_index(&a.destination),
                    Self::stack_slot_index(&b.destination),
                ) {
                    Self::record_dup_slot_pair(pairs, x, y);
                }
            }
        }
    }

    /// After `new; dup; …; invokespecial <init>`, the receiver slot is consumed but a
    /// duplicate reference remains on the stack below it. Link survivor ← receiver so
    /// constructor summaries on the receiver reach the value that `athrow` uses.
    ///
    /// Only emitted when the receiver and the slot immediately below it were outputs of
    /// the same `dup*` instruction (avoids merging unrelated stack values).
    fn init_survivor_link(
        call: &CallInfo,
        dup_slot_pairs: &HashSet<DupSlotPair>,
        locals: &mut Locals,
    ) -> Option<Statement> {
        let target = call.target.as_ref()?;
        if target.method_name != "<init>" {
            return None;
        }
        let receiver = call.receiver.as_ref()?;
        let Location::StackSlot(recv_idx) = receiver else {
            return None;
        };
        if *recv_idx == 0 {
            return None;
        }
        let survivor_idx = recv_idx - 1;
        let pair = (survivor_idx.min(*recv_idx), survivor_idx.max(*recv_idx));
        if !dup_slot_pairs.contains(&pair) {
            return None;
        }
        let survivor =
            VariableRef::new_local_idx(locals.get_or_intern(&numbered("stack", survivor_idx)));
        let recv_var =
            VariableRef::new_local_idx(locals.get_or_intern(&numbered("stack", *recv_idx)));
        Some(Self::assign_var(survivor, recv_var))
    }

    /// Lower `dup*` and `swap`. Every source is read before the instruction, but
    /// the outputs land on the input slots (`dup_x1` puts `v1` where `v2` was), so
    /// a source slot that an output overwrites is first copied to a temporary.
    fn stack_shuffle_statements(
        &mut self,
        dataflow: &[DataflowInfo],
        locals: &mut Locals,
    ) -> SmallVec<[Statement; 8]> {
        let moves: SmallVec<[&DataflowInfo; 6]> = dataflow
            .iter()
            .filter(|df| df.sources.as_slice() != std::slice::from_ref(&df.destination))
            .collect();
        let overwritten: SmallVec<[u32; 6]> = moves
            .iter()
            .filter_map(|df| Self::stack_slot_index(&df.destination))
            .collect();
        let mut stmts = SmallVec::new();
        let mut saved = SmallVec::<[u32; 6]>::new();
        for df in &moves {
            for n in df.sources.iter().filter_map(Self::stack_slot_index) {
                if overwritten.contains(&n) && !saved.contains(&n) {
                    saved.push(n);
                    stmts.push(Self::assign_var(
                        Self::shuffle_temp(n, locals),
                        self.convert_location_to_var_ref(&Location::StackSlot(n), locals),
                    ));
                }
            }
        }
        for df in moves {
            let sources = df
                .sources
                .iter()
                .map(|loc| match Self::stack_slot_index(loc) {
                    Some(n) if saved.contains(&n) => {
                        Exp::from(AccessPath::from(Self::shuffle_temp(n, locals)))
                    }
                    _ => self.convert_location_to_exp(loc, locals),
                })
                .collect();
            stmts.push(Statement::new_kind(StatementKind::Assign {
                dest: self
                    .convert_location_to_var_ref(&df.destination, locals)
                    .into(),
                sources,
            }));
        }
        stmts
    }

    /// The temporary [`Self::stack_shuffle_statements`] saves stack slot `n` in.
    fn shuffle_temp(n: u32, locals: &mut Locals) -> VariableRef {
        VariableRef::new_local_idx(locals.get_or_intern(&numbered("shuffle", n)))
    }

    fn dataflow_to_statements(
        &mut self,
        opcode: u8,
        data: &DataflowInfo,
        locals: &mut Locals,
    ) -> SmallVec<[Statement; 2]> {
        let dest_is_array = matches!(data.destination, Location::ArrayElement { .. });

        match opcode {
            // new / newarray / anewarray
            0xbb..=0xbd => {
                let class_name = data
                    .sources
                    .iter()
                    .find_map(|l| match l {
                        Location::Allocation(c) => Some(c.as_str()),
                        _ => None,
                    })
                    .expect("allocation type");
                smallvec![Statement::new_kind(StatementKind::Assign {
                    dest: self
                        .convert_location_to_var_ref(&data.destination, locals)
                        .into(),
                    sources: smallvec![Self::allocation_exp(class_name)],
                })]
            }
            // getstatic
            0xb2 => {
                let field = Self::field_ref_in(&data.sources).expect("getstatic field");
                let (global, field) = self.static_field(field);
                smallvec![Statement::new_kind(StatementKind::load(
                    self.convert_location_to_var_ref(&data.destination, locals),
                    global,
                    field,
                ))]
            }
            // putstatic
            0xb3 => {
                let field = match &data.destination {
                    Location::FieldRef(f) => f,
                    _ => panic!("putstatic destination"),
                };
                let value = self.stack_exp(
                    Self::stack_sources(&data.sources)
                        .next()
                        .expect("putstatic value"),
                    locals,
                );
                let (global, field) = self.static_field(field);
                smallvec![Statement::new_kind(StatementKind::store(
                    AccessPath::from(global),
                    field,
                    value,
                ))]
            }
            // getfield
            0xb4 => {
                let field = Self::field_ref_in(&data.sources).expect("getfield field");
                let object = self.convert_location_to_var_ref(
                    Self::stack_sources(&data.sources)
                        .next()
                        .expect("getfield object"),
                    locals,
                );
                smallvec![Statement::new_kind(StatementKind::load(
                    self.convert_location_to_var_ref(&data.destination, locals),
                    object,
                    Self::jvm_field_symbol(field),
                ))]
            }
            // putfield
            0xb5 => {
                let field = match &data.destination {
                    Location::FieldRef(f) => f,
                    _ => panic!("putfield destination"),
                };
                // The value is on top (one slot, or two for long/double), the object below it.
                let mut stacks = Self::stack_sources(&data.sources);
                let value = self.stack_exp(stacks.next().expect("putfield value"), locals);
                let object_slot = stacks.last().expect("putfield object");
                let object = self.convert_location_to_var_ref(object_slot, locals);
                smallvec![Statement::new_kind(StatementKind::store(
                    AccessPath::from(object),
                    Self::jvm_field_symbol(field),
                    value,
                ))]
            }
            // athrow
            0xbf => {
                let thrown = self.stack_exp(
                    Self::stack_sources(&data.sources)
                        .next()
                        .expect("athrow value"),
                    locals,
                );
                let except = Self::except(locals);
                smallvec![Statement::new_kind(StatementKind::Assign {
                    dest: except.into(),
                    sources: smallvec![thrown],
                })]
            }
            // *aload
            0x2e..=0x35 if Self::array_base(&data.sources).is_some() => {
                let base = Self::array_base(&data.sources).expect("aload base");
                let object = self.convert_location_to_var_ref(base, locals);
                smallvec![Statement::new_kind(StatementKind::load(
                    self.convert_location_to_var_ref(&data.destination, locals),
                    object,
                    cir::PathSegment::field("[]"),
                ))]
            }
            // *astore
            0x4f..=0x55 if dest_is_array => {
                let Location::ArrayElement { base, .. } = &data.destination else {
                    unreachable!();
                };
                let value = self.stack_exp(
                    Self::stack_sources(&data.sources)
                        .next()
                        .expect("astore value"),
                    locals,
                );
                let object = self.convert_location_to_var_ref(base, locals);
                smallvec![Statement::new_kind(StatementKind::store(
                    AccessPath::from(object),
                    cir::PathSegment::field("[]"),
                    value,
                ))]
            }
            _ => {
                let value_only = Self::is_value_only_opcode(opcode);
                let mut sources = SmallVec::new();
                for source_loc in data.sources.iter() {
                    let source = self.convert_location_to_exp(source_loc, locals);
                    sources.push(match source {
                        Exp::Path(ap) if value_only => Exp::Value(ap),
                        source => source,
                    });
                }
                smallvec![Statement::new_kind(StatementKind::Assign {
                    dest: self
                        .convert_location_to_var_ref(&data.destination, locals)
                        .into(),
                    sources,
                })]
            }
        }
    }

    /// Opcodes whose result is computed from its operands' values and shares no memory with
    /// them (S19): arithmetic, bitwise, shift and negation (`iadd`..`lxor`, 0x60..=0x83),
    /// `iinc` (0x84), comparisons (`lcmp`..`dcmpg`, 0x94..=0x98) and `instanceof` (0xc1), whose
    /// `int` is a test of the reference, a comparison. A numeric conversion (`i2l`..`i2s`,
    /// 0x85..=0x93) is a cast, the same value at another width, and stays a copy: a value a
    /// bridged native returned in memory (`ret.deref.#value`) must survive `int out = f(); long
    /// v = out`.
    fn is_value_only_opcode(opcode: u8) -> bool {
        matches!(opcode, 0x60..=0x84 | 0x94..=0x98 | 0xc1)
    }

    /// A `CONSTANT_MethodHandle` that invokes a known method, as a function reference to that
    /// method (S11), named as the method's `FunctionData` is (`Lcls;->name(desc)`).
    ///
    /// `REF_invokeStatic` and `REF_invokeSpecial` name the method that runs. For
    /// `REF_invokeVirtual` and `REF_invokeInterface` the handle names the declared method,
    /// which is the target; an override in a subclass is not. `REF_newInvokeSpecial` is left
    /// out: invoking it allocates the object `<init>` takes as `this`, so the handle's
    /// arguments do not line up with `<init>`'s formals. Field handles name no method. Those
    /// are `None` and stay an opaque `MethodHandle` object.
    fn method_handle_exp(&mut self, handle: &ConstantValue) -> Option<Exp> {
        const REF_INVOKE_VIRTUAL: u8 = 5;
        const REF_INVOKE_STATIC: u8 = 6;
        const REF_INVOKE_SPECIAL: u8 = 7;
        const REF_INVOKE_INTERFACE: u8 = 9;
        let ConstantValue::MethodHandle {
            reference_kind,
            class_name,
            name,
            descriptor,
        } = handle
        else {
            return None;
        };
        let is_instance = match *reference_kind {
            REF_INVOKE_STATIC => false,
            REF_INVOKE_VIRTUAL | REF_INVOKE_SPECIAL | REF_INVOKE_INTERFACE => true,
            _ => return None,
        };
        // As `decode_call` names a call's target.
        let class_name = format!("L{class_name};");
        let java_sig = format!("{class_name}->{name}{descriptor}");
        self.declare_extern(&java_sig, &class_name, name, descriptor, is_instance);
        Some(Exp::new_object_ref(CallObject::FunctionPtr(
            java_sig.as_str().into(),
        )))
    }

    fn convert_location_to_exp(&mut self, loc: &Location, locals: &mut Locals) -> Exp {
        match loc {
            Location::StackSlot(_) | Location::StackInput(_) => Exp::from(AccessPath::from(
                self.convert_location_to_var_ref(loc, locals),
            )),
            Location::Constant(ConstantValue::Integer(n)) => Exp::new_int(*n as i64),
            Location::Constant(ConstantValue::Long(n)) => Exp::new_int(*n),
            Location::Constant(ConstantValue::String(s)) => Exp::new_str(s),
            // As ctadl-dex models const-method-handle and const-method-type.
            Location::Constant(handle @ ConstantValue::MethodHandle { .. }) => self
                .method_handle_exp(handle)
                .unwrap_or_else(|| Self::allocation_exp("java/lang/invoke/MethodHandle")),
            Location::Constant(ConstantValue::MethodType(_)) => {
                Self::allocation_exp("java/lang/invoke/MethodType")
            }
            Location::Constant(ConstantValue::Dynamic { descriptor, .. })
                if descriptor.starts_with('L') || descriptor.starts_with('[') =>
            {
                Self::allocation_exp(descriptor)
            }
            Location::Allocation(class_name) => Self::allocation_exp(class_name),
            // A field read is not expressible as an Exp; genuine field reads are lowered to
            // Load instructions in the getfield/getstatic/aload opcode arms. This &self helper
            // cannot emit a load, so a FieldRef reaching here (a rare generic-source fallback)
            // degrades to its base variable.
            Location::FieldRef(_) => Exp::from(AccessPath::from(
                self.convert_location_to_var_ref(loc, locals),
            )),
            _ => Exp::from(AccessPath::from(
                self.convert_location_to_var_ref(loc, locals),
            )),
        }
    }

    fn convert_location_to_var_ref(&self, loc: &Location, locals: &mut Locals) -> VariableRef {
        match loc {
            Location::StackSlot(n) => {
                VariableRef::new_local_idx(locals.get_or_intern(&numbered("stack", *n)))
            }
            Location::StackInput(_) | Location::StackOutput => {
                VariableRef::new_local_idx(locals.get_or_intern("Stack Local?"))
            }
            Location::Register(n) => {
                VariableRef::new_local_idx(locals.get_or_intern(&numbered("reg", (*n).into())))
            }
            Location::Parameter(n) => VariableRef::new_parameter((*n).into()),
            Location::FieldRef(_) => {
                // Field accesses are lowered in `dataflow_to_statements`; this path is a fallback.
                VariableRef::new_local_idx(locals.get_or_intern("unknownFieldBase"))
            }
            Location::ArrayElement { base, .. } => self.convert_location_to_var_ref(base, locals),
            _ => VariableRef::new_local_idx(locals.get_or_intern("unknownLocationType")),
        }
    }
}

/// The local name `prefix` followed by `n` in decimal (`stack3`), built on the stack: a name is
/// looked up for every operand of every instruction, so each in a `String` of its own was an
/// allocation per operand.
fn numbered(prefix: &str, n: u32) -> NumberedName {
    use std::fmt::Write;
    let mut name = NumberedName {
        buf: [0; 24],
        len: 0,
    };
    write!(name, "{prefix}{n}").expect("a local's prefix and number fit");
    name
}

struct NumberedName {
    buf: [u8; 24],
    len: usize,
}

impl std::fmt::Write for NumberedName {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let end = self.len + s.len();
        self.buf
            .get_mut(self.len..end)
            .ok_or(std::fmt::Error)?
            .copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

impl std::ops::Deref for NumberedName {
    type Target = str;
    fn deref(&self) -> &str {
        std::str::from_utf8(&self.buf[..self.len]).expect("built from strs")
    }
}
