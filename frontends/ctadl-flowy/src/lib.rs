/*!
Flowy language.st:

The language defines functions, assignments, calls. It provides constructs for requiring
function summaries and taint flows, including requiring the absence of summaries and the
absence of flows.

The basic language is simple. Functions are defined at the top level, and consist of a sequence
of labeled basic blocks. A basic block contains any number of assignments, function calls, and
is terminated by a return or goto. The function name `Main` has no special meaning. Function
calls are pass-by-value. Variables are untyped and do not need to be declared. Assignments can
refer to fields.

```text
def Main(a, b, c) {
start:
  b.foo = c.baz;
  a = b;
  F(a, c);
goto a, b;
a:
  return a.field;
b:
  return b.bar;
}
```

Assignments can take several forms:

```text
a = b; // normal
c.foo = b.bar.baz; // field update
c = a, d.baz; // multiple flows to a variable
```

A single field map be updated on the left-hand side. For a field update, the comma operator may not
be used on the right hand side. To express multiple flows into a field, first a assign a variable,
then update the field: `tmp = a, b; c.foo = tmp;`. The comma operator is used on the right-hand
side to merge multiple flows into a variable.

A field can also be set *functionally*, with the bracket form:

```text
x = [y.foo := v]; // x is y, but with .foo set to v
x = [x.foo := v]; // a fresh version of x
```

This reads "x is y with .foo replaced by v", and it differs from the assignment `x.foo = v` in
what happens to the rest of the aggregate. An assignment writes through `x` and says nothing about
where `x` came from; the bracket form names the source aggregate `y` separately from the
destination `x` it defines, so everything in `y` that the update did not overwrite flows into `x`
as well. Naming the two apart is also what lets SSA give the destination a fresh version of the
variable the source reads, which is why `x = [x.foo := v]` is meaningful rather than circular.

The path may carry offsets (`x = [y.[8].foo := v]`), which are address arithmetic, but it names
exactly one field: the form denotes a single update instruction, and a nested update would instead
have to rebuild each enclosing aggregate. Globals cannot be updated, since a global is itself a
field of the global heap.

The function `F` is required to have a summary that returns its first
argument. `F` satisfies this requirement.

```text
def F(a)
where summaries [return <- a]
{
s:
    return a;
}
```

The function `G` is required *not* to have a summary that returns its first argument. `G` does
*not* satisfy this requirement.

```text
def G(a)
where summaries [return </- a]
{
s:
    return a;
}
```

The function `H` tests that data flows between source and sink. If the analyzer cannot conclude
that data flows, the test will fail. If the label (`Data`) were different on either the source
or sink, the test will fail.

```text
def H(a, b) {
s:
  b = source(Data);
  a = b;
  sink(a, Data); // Change Data to Sink and the test will fail
  return;
}
```

The call `errsink` (and its companion, `errsource`) is used to test for the absence of a flow;
if there is a flow from a source to a corresponding `errsink`, the test will fail. The test
below fails.

```text
def H(a, b) {
s:
  b = source(Data);
  a = b;
  errsink(a, Data);
  return;
}
```

Global variables are created at the top level with the `var` syntax:

```text
var x;
```

Any function in the file referring to `x` accesses the global variable. There is no way to have a
local variable and a global variable of the same name.
*/
use std::{fmt, fmt::Display};

use ctadl_ir::{ThinVec, thin_vec};
use hashbrown::{hash_map::HashMap, hash_set::HashSet};
use internment::ArcIntern;
use smallvec::SmallVec;
use thiserror::Error;

use crate::parse::{Ap, LineIndex, Pos};
use ctadl_ir::index::idx::Idx;
use ctadl_ir::mir::visit::MutVisitor;
use ctadl_ir::mir::*;
use ctadl_ir::ssa;

pub mod parse;

/// The base of a port reference for a summary or a source-sink requirement. See
/// [`SummaryRequires`] and [`EndpointRequires`]
#[derive(Debug, Eq, PartialEq, Hash, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PortBase {
    /// Denotes the return value in a summary requirement
    Return,
    /// A variable
    Var(VariableRef),
}

impl Display for PortBase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PortBase::Return => write!(f, "return"),
            PortBase::Var(v) => write!(f, "{v}"),
        }
    }
}

/// A port is an access path in a summary or source-sink requirement. See
/// [`SummaryRequires`] and [`EndpointRequires`]
#[derive(Debug, Eq, PartialEq, Hash, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Port {
    pub base: PortBase,
    pub fields: ThinVec<PathSegment>,
}

impl Display for Port {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Port { base, fields } = self;
        write!(f, "{base}")?;
        for field in fields {
            write!(f, ".{field}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum FlowSpec {
    /// Requires a flow
    FlowPresent,
    /// Requires absence of a flow
    FlowAbsent,
}

/// A flowy program contains a CFG and some requirements to check.
#[derive(Debug, Default)]
pub struct FlowyProgram {
    pub requirements: FlowyRequires,
    /// The program to be checked.
    pub program_info: ProgramInfo,
}

/// Requirements to check on summaries and taint endpoints.
#[derive(Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FlowyRequires {
    /// Function names and summary requirements to check
    pub summary_requires: SummaryRequires,
    /// Source-sink requirements to check
    pub endpoint_requires: EndpointRequires,
}

impl Display for FlowyRequires {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.summary_requires, self.endpoint_requires)
    }
}

/// Summaries produced by indexing are checked against these requirements.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SummaryRequires {
    /// Maps function name to list of summary requirements
    pub requires: HashMap<ArcIntern<str>, Vec<SummarySpec>>,
}

impl Display for SummaryRequires {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (function, reqs) in &self.requires {
            writeln!(f, "summary requirements for {function}")?;
            for summary_spec in reqs {
                writeln!(f, "{summary_spec}")?;
            }
            writeln!(f, "end summary requirements for {function}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SummarySpec {
    pub dest: Port,
    pub flow: FlowSpec,
    pub source: Port,
}

impl Display for SummarySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let SummarySpec {
            dest: dst,
            flow: spec,
            source: src,
        } = self;
        let spec = match spec {
            FlowSpec::FlowPresent => "",
            FlowSpec::FlowAbsent => "! ",
        };
        write!(f, "  {spec}{dst} <- {src}")
    }
}

/// Taint flows produced by querying are checked against these requirements.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EndpointRequires {
    /// Maps function to list of endpoint requirements
    pub requires: HashMap<ArcIntern<str>, Vec<(Endpoint, FlowSpec)>>,
}

impl Display for EndpointRequires {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (function, endpoints) in &self.requires {
            writeln!(f, "source-sink requirements for {function}")?;
            for (endpoint, spec) in endpoints {
                match spec {
                    FlowSpec::FlowPresent => writeln!(f, "  must reach {endpoint}")?,
                    FlowSpec::FlowAbsent => writeln!(f, "  error to reach {endpoint}")?,
                }
            }
            writeln!(f, "end source-sink requirements for {function}")?;
        }
        Ok(())
    }
}

/// An endpoint is a source or a sink.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Endpoint {
    pub infunc: ArcIntern<str>,
    pub port: (VariableRef, OffsetAccesses),
    /// The taint label
    pub label: String,
    pub direction: EndpointDirection,
    /// The parameter index this endpoint's port denotes, if it is (a copy of) a parameter.
    /// `Some` anchors the endpoint at the function's call sites (the call-arg for this
    /// formal); `None` (a local such as a `source`'s returned value, or a global) keeps it
    /// function-anchored. The front-end's `t? = c; sink(t?, ...)` lowering means the port is
    /// usually a temp, so this is resolved from the copied parameter rather than the port.
    pub formal: Option<i16>,
    pub source_info: SourceInfo,
    /// Optional expected number of distinct source->sink paths to assert for
    /// this endpoint. Supplied as a trailing integer argument to the
    /// `source`/`sink` (and `errsource`/`errsink`) intrinsic, e.g.
    /// `sink(x, Label, 2)`. When `None`, the endpoint is checked for *presence*
    /// only (the default). When `Some(n)`, the human-profile path check asserts
    /// that exactly `n` distinct paths reach (for a sink) or leave (for a
    /// source) this endpoint -- this is how we encode the call-site-distinct
    /// path expectation that formal-anchored endpoints currently collapse.
    pub path_count: Option<usize>,
}

impl Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Endpoint {
            infunc,
            port,
            label,
            direction,
            formal: _,
            source_info,
            path_count,
        } = self;
        write!(
            f,
            "{}@{}: {}{} is a {} label '{}'",
            infunc, source_info, port.0, port.1, direction, label
        )?;
        if let Some(n) = path_count {
            write!(f, " (expect {n} paths)")?;
        }
        Ok(())
    }
}

/// Specifies whether an endpoint is:
/// 1. a source and tracks data flow forward; or
/// 2. a sink and tracks data flow backward.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum EndpointDirection {
    Source,
    Sink,
}

impl Display for EndpointDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EndpointDirection::Source => write!(f, "source"),
            EndpointDirection::Sink => write!(f, "sink"),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Env {
    /// Parameters or globals
    parameters: HashMap<String, VariableRef>,
    globals: HashSet<String>,
}

/// Compile errors
#[derive(Error, Debug)]
pub enum FlowyError {
    #[error("i/o error")]
    Io(#[from] std::io::Error),
    #[error("parse error")]
    Parse(#[from] peg::error::ParseError<peg::str::LineCol>),
    #[error("ir verification error")]
    Verify(#[from] ctadl_ir::mir::VerifyErrors),
    #[error("{line}:{col}: {message}")]
    Compile {
        message: String,
        line: usize,
        col: usize,
    },
}

/// Compiles a flowy program.
///
/// The program is returned in SSA form.
pub fn compile_program<P: AsRef<std::path::Path>>(file: P) -> Result<FlowyProgram, FlowyError> {
    let file = file.as_ref();
    let contents = source_info::read_source(file)?;
    compile_program_contents(file.to_string_lossy().as_ref(), contents.as_str())
}

pub fn compile_program_contents(
    artifact_name: &str,
    contents: &str,
) -> Result<FlowyProgram, FlowyError> {
    let mut result = FlowyProgram::default();
    let mut ctx = FlowyCtx::new(artifact_name);

    ctx.parse(contents)?;

    ctx.program.verify()?;
    let program_info = ProgramInfo {
        program: ctx.program,
        vmt: Default::default(),
        source_info: ctx.source_info_builder.finish(),
    };
    result.program_info = program_info;
    let summary_requires = SummaryRequires {
        requires: ctx.summary_requires,
    };

    // Transform each func to SSA and extract endpoint requirements
    let mut ssa_funcs = Vec::new();
    let mut find_specs = ExtractSpec::default();
    while let Some(mut f) = result.program_info.program.functions.pop() {
        ssa::transform(&mut f, false);
        find_specs.set_function_name(f.name.clone().into());
        find_specs.visit_function_data(FunctionIdx::new(0), &mut f);
        ssa_funcs.push(f);
    }
    result
        .program_info
        .program
        .functions
        .extend(ssa_funcs.into_iter().rev());
    let endpoint_requires = EndpointRequires {
        requires: find_specs.endpoint_requires,
    };
    result.requirements = FlowyRequires {
        endpoint_requires,
        summary_requires,
    };
    Ok(result)
}

#[derive(Debug)]
struct FlowyCtx {
    /// The program we parse into
    program: Program,
    artifact_key: source_info::ArtifactKey,
    source_info_builder: source_info::SourceInfoBuilder,
    summary_requires: HashMap<ArcIntern<str>, Vec<SummarySpec>>,
    /// Global variables
    toplevel_vars: HashSet<String>,
    /// Used for temporaries
    counter: Counter,
}

impl FlowyCtx {
    fn new(artifact_name: &str) -> Self {
        let artifact_key = source_info::ArtifactKey {
            path: artifact_name.to_string(),
            sub_artifact_id: 0,
            hash: Vec::new(),
            encoding: source_info::ArtifactEncoding::Utf8,
        };
        let artifact_metadata = source_info::ArtifactMetadata::new();
        Self {
            program: Default::default(),
            artifact_key,
            source_info_builder: source_info::SourceInfoBuilder::new(artifact_metadata),
            summary_requires: Default::default(),
            toplevel_vars: Default::default(),
            counter: Default::default(),
        }
    }

    /// Parse the program from a string
    fn parse(&mut self, contents: &str) -> Result<(), FlowyError> {
        let lines = LineIndex::new(contents);
        let defs = parse::flowy::program(contents, &lines)?;
        // Collect the globals and function names first, since a function may refer to either
        // before its definition.
        let mut func_defs = Vec::new();
        let mut defined_functions = HashSet::new();
        for def in defs {
            match def {
                parse::Def::Var(name) => {
                    self.toplevel_vars.insert(name.name.to_string());
                }
                parse::Def::Function(f) => {
                    defined_functions.insert(f.name.name.to_string());
                    func_defs.push(f);
                }
            }
        }
        for f in &func_defs {
            self.parse_function_def(f, &defined_functions)?;
        }
        Ok(())
    }

    /// The source info for a statement or terminator spanning `span`.
    fn source_info(&mut self, span: &std::ops::Range<usize>) -> SourceInfo {
        let start = span.start.try_into().unwrap();
        let len = source_info::SpanLen::ByteLen((span.end - span.start + 1).try_into().unwrap());
        SourceInfo::new(
            self.source_info_builder
                .span_for(self.artifact_key.clone(), start, len),
        )
    }

    /// Parse the formals into a map so that we can look them up as they are referenced.
    fn parse_function_def(
        &mut self,
        def: &parse::FunctionDef<'_>,
        defined_functions: &HashSet<String>,
    ) -> Result<(), FlowyError> {
        // Function we're building
        let function = self.program.new_function();
        let mut env: Env = Default::default();
        env.globals.extend(self.toplevel_vars.iter().cloned());

        let name = def.name.name;
        if ["source", "sink", "errsource", "errsink"].contains(&name) {
            return Err(compile_error(
                def.name.pos,
                format!("name is reserved: '{name}'"),
            ));
        }
        self.program[function].name = name.to_string();

        self.parse_formals(&def.params, &mut env, function);
        if let Some(arity) = def.return_arity {
            self.program.functions[function].set_return_type(ReturnType { arity });
        }
        if let Some(entries) = &def.summaries {
            self.parse_where_clause(entries, &env, function)?;
        }

        // Parse the blocks, associating block labels with their basic block number and goto
        // targets
        let mut terminators = HashMap::new();
        for b in &def.blocks {
            // New basic block to fill in
            let block = self.program[function]
                .blocks
                .blocks_mut()
                .push(BasicBlockData::new(None));
            let targets = self.parse_block(b, &env, function, block, defined_functions)?;
            terminators.insert(BlockLabel(b.label.name.to_string()), (block, targets));
        }

        // Once we've parsed all the blocks, validate the target labels and convert them to basic
        // block indices and generate the goto instructions.
        for (_, (block, info)) in terminators.iter() {
            let Some(info) = info else {
                continue;
            };
            let (targets, pos) = info;
            let source_info = targets.source_info;
            let targets = targets
                .targets
                .iter()
                .map(|target| match terminators.get(target) {
                    None => {
                        let BlockLabel(s) = target;
                        Err(compile_error(
                            *pos,
                            format!("goto refers to nonexistent block: '{s}'"),
                        ))
                    }
                    Some((target_block, _)) => Ok(*target_block),
                });
            let targets: Result<SmallVec<[_; 4]>, _> = targets.collect();
            let blocks = self.program[function].blocks.blocks_mut();
            blocks[*block].terminator = Some(Terminator::new(
                TerminatorKind::Goto { targets: targets? },
                source_info,
            ));
        }
        Ok(())
    }

    /// Parse formals and append them to the function. Adds to the locals map the variables for the
    /// parameters
    fn parse_formals(
        &mut self,
        formals: &[parse::Param<'_>],
        locals: &mut Env,
        function: FunctionIdx,
    ) {
        let params = &mut self.program[function].params;
        for formal in formals {
            params.parameters.push(formal.ty);
            let index = params.last_index().unwrap();
            locals.parameters.insert(
                formal.name.name.to_string(),
                VariableRef::new_var_ref(Variable::Param(index)),
            );
        }
    }

    /// Parses a basic block and returns any goto targets from the terminator, with the position of
    /// the goto.
    fn parse_block(
        &mut self,
        b: &parse::Block<'_>,
        locals: &Env,
        function: FunctionIdx,
        block: BasicBlockIdx,
        defined_functions: &HashSet<String>,
    ) -> Result<Option<(GotoTargets, Pos)>, FlowyError> {
        for stmt in &b.stmts {
            self.parse_stmt(stmt, locals, function, block, defined_functions)?;
        }
        let source_info = self.source_info(&b.terminator.span);
        match &b.terminator.kind {
            parse::TerminatorKind::Goto(targets) => {
                let targets = GotoTargets {
                    targets: targets
                        .iter()
                        .map(|t| BlockLabel(t.name.to_string()))
                        .collect(),
                    source_info,
                };
                Ok(Some((targets, b.terminator.pos)))
            }
            parse::TerminatorKind::Return(value) => {
                let func = &mut self.program[function];
                let data = &mut func.blocks[block];
                let local_table = &mut func.locals;
                let args = match value {
                    Some(value) => {
                        let r = parse_ref(locals, local_table, value, defined_functions)?;
                        vec![lower_ref(
                            &mut self.counter,
                            data,
                            local_table,
                            source_info,
                            r,
                        )]
                    }
                    None => vec![],
                };
                data.terminator = Some(Terminator::new(
                    TerminatorKind::Return { args: args.into() },
                    source_info,
                ));
                Ok(None)
            }
        }
    }

    /// Parses a statement and appends it to the basic block.
    fn parse_stmt(
        &mut self,
        stmt: &parse::Stmt<'_>,
        locals: &Env,
        function: FunctionIdx,
        block: BasicBlockIdx,
        defined_functions: &HashSet<String>,
    ) -> Result<(), FlowyError> {
        use StatementKind::*;
        let source_info = self.source_info(&stmt.span);
        // Disjoint borrow of the block being built and the function's locals table, so temporaries
        // and named locals can be interned while lowering statements.
        let func = &mut self.program[function];
        let data = &mut func.blocks[block];
        let local_table = &mut func.locals;
        match &stmt.kind {
            parse::StmtKind::Assign { dest, sources } => {
                let (dst_base, dst_segments) = parse_path(locals, local_table, dest)?;
                // src is comma-separated; a field read on the RHS lowers to loads.
                let src = {
                    let mut result = Vec::new();
                    for p in sources {
                        let r = parse_ref(locals, local_table, p, defined_functions)?;
                        result.push(lower_ref(
                            &mut self.counter,
                            data,
                            local_table,
                            source_info,
                            r,
                        ));
                    }
                    result
                };
                if dst_segments.is_empty() {
                    data.push_back(Statement::new(
                        StatementKind::assign(dst_base, src),
                        source_info,
                    ));
                } else if src.len() > 1 {
                    return Err(compile_error(
                        stmt.pos,
                        "cannot update a field with multiple sources",
                    ));
                } else {
                    // A field/offset write. A location with intermediate symbolic dereferences
                    // needs loads before the store, so lower it with `store_access_path`.
                    check_store_target(&dst_segments, stmt.pos)?;
                    let mut stmts = Vec::new();
                    let counter = &mut self.counter;
                    ctadl_ir::mir::store_access_path(
                        dst_base,
                        dst_segments,
                        src[0].clone(),
                        &mut stmts,
                        || {
                            VariableRef::new_local_idx(
                                local_table.get_or_intern(&format!("t{}?", counter.next())),
                            )
                        },
                    );
                    for mut s in stmts {
                        s.source_info = source_info;
                        data.push_back(s);
                    }
                }
            }
            parse::StmtKind::Update {
                dest,
                source,
                value,
            } => {
                // A global is modeled as a symbolic field of the global heap, so `g` already
                // spends the one field an `Update` carries and `g.f` would need two.
                if names_global(locals, dest) || names_global(locals, source) {
                    return Err(compile_error(
                        stmt.pos,
                        "a global cannot be updated: a global is a field of the global heap, so \
                         an update of one would have to write two fields; use the assignment \
                         form `g.f = v`",
                    ));
                }
                let (dst_base, dst_segments) = parse_path(locals, local_table, dest)?;
                let (src_base, mut src_segments) = parse_path(locals, local_table, source)?;
                // The destination is the variable the update *defines* — the fresh version of the
                // aggregate — so it is a bare name. The path being written belongs to the source,
                // which is where it reads.
                if !dst_segments.is_empty() {
                    return Err(compile_error(
                        stmt.pos,
                        "an update's destination is the variable it defines, so it takes no \
                         field path: write `x = [y.f := v]`",
                    ));
                }
                // An `Update` writes exactly one symbolic field, so the source names exactly one.
                // A nested update would have to rebuild every enclosing aggregate, which is a
                // chain of updates rather than the single instruction this syntax denotes.
                let Some(i) = src_segments.iter().position(PathSegment::is_symbol) else {
                    return Err(compile_error(
                        stmt.pos,
                        "an update must write a field: write `x = [y.f := v]`",
                    ));
                };
                if src_segments[i + 1..].iter().any(PathSegment::is_symbol) {
                    return Err(compile_error(
                        stmt.pos,
                        "an update writes a single field, so its source names one: write \
                         `x = [y.f := v]`",
                    ));
                }
                let PathSegment::Symbol(field) = src_segments.remove(i) else {
                    unreachable!()
                };
                let value = {
                    let r = parse_ref(locals, local_table, value, defined_functions)?;
                    lower_ref(&mut self.counter, data, local_table, source_info, r)
                };
                // What is left of the source path is pure offsets: address arithmetic, which lives
                // on the update's destination address exactly as it lives on a store's. Lowering
                // it through `load_access_path` merges consecutive offsets and emits nothing.
                let mut offsets = Vec::new();
                let counter = &mut self.counter;
                let addr =
                    ctadl_ir::mir::load_access_path(src_base, src_segments, &mut offsets, || {
                        VariableRef::new_local_idx(
                            local_table.get_or_intern(&format!("t{}?", counter.next())),
                        )
                    });
                debug_assert!(offsets.is_empty(), "an offset-only path emits no loads");
                data.push_back(Statement::new(
                    StatementKind::update(
                        AccessPath {
                            base: dst_base,
                            accesses: addr.accesses,
                        },
                        addr.base,
                        field,
                        value,
                    ),
                    source_info,
                ));
            }
            parse::StmtKind::AssignCall { dest, callee, args } => {
                let (lhs_base, lhs_segments) = parse_path(locals, local_table, dest)?;
                let (variable, segments) = parse_path(locals, local_table, callee)?;
                let actuals = parse_actuals(locals, local_table, args, defined_functions)?;
                let style = if !segments.is_empty() {
                    // Indirect call: lower symbolic derefs to loads, leaving an offset-only callee.
                    let callee = lower_callee_addr(
                        &mut self.counter,
                        data,
                        local_table,
                        source_info,
                        variable,
                        segments,
                    );
                    CallStyle::FuncPtrCall {
                        callee,
                        signature: None,
                    }
                } else {
                    let is_direct = match &variable.variable {
                        Variable::Local(idx) => {
                            let name = local_table.name(*idx);
                            name == "source"
                                || name == "errsource"
                                || defined_functions.contains(name)
                        }
                        _ => false,
                    };

                    if is_direct {
                        let Variable::Local(idx) = &variable.variable else {
                            unreachable!()
                        };
                        let name = local_table.name(*idx);
                        CallStyle::DirectCall {
                            call_edges: CallEdges::Explicit(vec![name.to_string()].into()),
                        }
                    } else {
                        // Indirect call with parameter, global, or undefined function
                        CallStyle::FuncPtrCall {
                            callee: AccessPath::without_fields(variable),
                            signature: None,
                        }
                    }
                };

                let is_source = matches!(
                    &style,
                    CallStyle::DirectCall {
                        call_edges: CallEdges::Explicit(edges),
                    } if edges[0] == "source" || edges[0] == "errsource"
                );
                let mut args: ThinVec<Exp> = ThinVec::new();
                for (i, x) in actuals.into_iter().enumerate() {
                    if is_source && i == 0 {
                        // Stringify only the label (index 0); pass any trailing actuals
                        // (e.g. the path-count int in `source(Label, n)`) through, lowering
                        // any field reads, so they reach `ExtractSpec` as-is.
                        args.push(Exp::Str(label_string(local_table, &x).into()));
                    } else {
                        args.push(lower_ref(
                            &mut self.counter,
                            data,
                            local_table,
                            source_info,
                            x,
                        ));
                    }
                }

                // use a temporary for the result of the call
                let tmp = VariableRef::new_local_idx(
                    local_table.get_or_intern(&format!("t{}?", self.counter.next())),
                );
                let call = CallAssign {
                    style,
                    rets: thin_vec![tmp.clone()],
                    args,
                };
                data.push_back(Statement::new(call, source_info));

                // assign the temporary to the field (if applicable), lowering any intermediate
                // dereferences into loads plus a store.
                check_store_target(&lhs_segments, stmt.pos)?;
                let mut stmts = Vec::new();
                let counter = &mut self.counter;
                ctadl_ir::mir::store_access_path(
                    lhs_base,
                    lhs_segments,
                    Exp::Variable(tmp),
                    &mut stmts,
                    || {
                        VariableRef::new_local_idx(
                            local_table.get_or_intern(&format!("t{}?", counter.next())),
                        )
                    },
                );
                for mut s in stmts {
                    s.source_info = source_info;
                    data.push_back(s);
                }
            }
            parse::StmtKind::Call { callee, args } => {
                let (variable, segments) = parse_path(locals, local_table, callee)?;
                let actuals = parse_actuals(locals, local_table, args, defined_functions)?;

                let style = if !segments.is_empty() {
                    // Indirect call: lower symbolic derefs to loads, leaving an offset-only callee.
                    let callee = lower_callee_addr(
                        &mut self.counter,
                        data,
                        local_table,
                        source_info,
                        variable,
                        segments,
                    );
                    CallStyle::FuncPtrCall {
                        callee,
                        signature: None,
                    }
                } else {
                    let is_direct = match &variable.variable {
                        Variable::Local(idx) => {
                            let name = local_table.name(*idx);
                            name == "sink" || name == "errsink" || defined_functions.contains(name)
                        }
                        _ => false,
                    };

                    if is_direct {
                        let Variable::Local(idx) = &variable.variable else {
                            unreachable!()
                        };
                        let name = local_table.name(*idx);
                        CallStyle::DirectCall {
                            call_edges: CallEdges::Explicit(vec![name.to_string()].into()),
                        }
                    } else {
                        // Indirect call with parameter, global, or undefined function
                        CallStyle::FuncPtrCall {
                            callee: AccessPath::without_fields(variable),
                            signature: None,
                        }
                    }
                };

                let is_sink = matches!(
                    &style,
                    CallStyle::DirectCall {
                        call_edges: CallEdges::Explicit(edges),
                    } if edges[0] == "sink" || edges[0] == "errsink"
                );
                let args: ThinVec<Exp> = if is_sink {
                    // Lowers `sink(x.y.z, Test)` into `t0 = x.y.z; sink(t0, Test)` so that when
                    // the sink call is removed, x.y.z remains in the program.
                    let tmp = VariableRef::new_local_idx(
                        local_table.get_or_intern(&format!("t{}?", self.counter.next())),
                    );
                    let mut args = ThinVec::with_capacity(actuals.len());
                    let mut port_ref: Option<ParsedRef> = None;
                    for (i, x) in actuals.into_iter().enumerate() {
                        if i == 0 {
                            // use a temporary for the sink argument
                            args.push(Exp::Variable(tmp.clone()));
                            port_ref = Some(x);
                        } else if i == 1 {
                            args.push(Exp::Str(label_string(local_table, &x).into()));
                        } else {
                            args.push(lower_ref(
                                &mut self.counter,
                                data,
                                local_table,
                                source_info,
                                x,
                            ));
                        }
                    }
                    // `t0 = x.y.z` (loading the port's field path if any), keeping the reference
                    // alive after the sink call is stripped.
                    if let Some(port) = port_ref {
                        let port_val =
                            lower_ref(&mut self.counter, data, local_table, source_info, port);
                        let assign_tmp = StatementKind::assign(tmp.clone(), [port_val]);
                        data.push_back(Statement::new(assign_tmp, source_info));
                    }
                    args
                } else {
                    let mut args = ThinVec::with_capacity(actuals.len());
                    for x in actuals {
                        args.push(lower_ref(
                            &mut self.counter,
                            data,
                            local_table,
                            source_info,
                            x,
                        ));
                    }
                    args
                };
                let rets = thin_vec![];
                //let args = actuals.into_iter().map(|x| Exp::AccessPath(x));
                let call = CallAssign { style, rets, args };
                data.push_back(Statement::new(call, source_info));
            }
        }
        Ok(())
    }

    fn parse_where_clause(
        &mut self,
        entries: &[parse::SummaryEntry<'_>],
        locals: &Env,
        function: FunctionIdx,
    ) -> Result<(), FlowyError> {
        // TODO this can just parse into a local type instead of indexing by function name
        let target = ArcIntern::<str>::from(self.program[function].name.clone());
        for entry in entries {
            let spec = SummarySpec {
                dest: parse_summary_ap(locals, &entry.dest)?,
                flow: entry.flow.clone(),
                source: parse_summary_ap(locals, &entry.source)?,
            };
            self.summary_requires
                .entry(target.clone())
                .or_default()
                .push(spec);
        }
        Ok(())
    }
}

#[derive(Debug)]
struct GotoTargets {
    targets: SmallVec<[BlockLabel; 4]>,
    source_info: SourceInfo,
}

#[derive(Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
struct BlockLabel(String);

/// A summary access path is parsed into a port, which is a spec for the parameters or return value
/// of a function.
fn parse_summary_ap(env: &Env, path: &parse::Path<'_>) -> Result<Port, FlowyError> {
    let name = path.base.name;
    let field_accesses = path_segments(&path.segments)?;
    if name == "return" {
        return Ok(Port {
            base: PortBase::Return,
            fields: field_accesses,
        });
    }
    // The port has to refer to a formal, so error if it doesn't
    if let Some(v) = env.parameters.get(name) {
        Ok(Port {
            base: PortBase::Var(v.clone()),
            fields: field_accesses,
        })
    } else if env.globals.contains(name) {
        // A global `name` is modeled as a symbolic field of the global heap.
        let mut global_field_accesses: ThinVec<PathSegment> = thin_vec![PathSegment::symbol(name)];
        global_field_accesses.extend(field_accesses);
        Ok(Port {
            base: PortBase::Var(VariableRef::new_global()),
            fields: global_field_accesses,
        })
    } else {
        panic!("In summary requires, found nonexistent formal reference: {name}")
    }
}

/// Converts the segments of a path, rejecting `.*`.
///
/// The grammar accepts `.*`, but flowy has no wildcard field, so there is no [`PathSegment`] to
/// return for one. It used to *panic* — `x.* = y;` in a `.flowy` file crashed the tool — so
/// rejecting it gives the user a positioned error instead of a backtrace.
fn path_segments(segments: &[parse::Segment<'_>]) -> Result<ThinVec<PathSegment>, FlowyError> {
    segments
        .iter()
        .map(|s| match s {
            parse::Segment::Field(name) => Ok(PathSegment::symbol(*name)),
            parse::Segment::Offset(offset) => Ok(PathSegment::offset(*offset)),
            parse::Segment::Star(pos) => Err(compile_error(
                *pos,
                "'.*' is not an access-path segment: there is no wildcard field. Write the field \
                 name, or '.[n]' for an offset",
            )),
        })
        .collect()
}

fn compile_error(pos: Pos, message: impl Into<String>) -> FlowyError {
    FlowyError::Compile {
        message: message.into(),
        line: pos.line,
        col: pos.col,
    }
}

/// A parsed operand: either a non-path value (constant / function pointer) or an access path
/// (`ident ~ p*`). A field read is not expressible as an [`Exp`], so an access path with fields
/// must be lowered into loads (see [`lower_ref`]) before it can be used as an rvalue; when used
/// as an lvalue or callee its path is consumed directly.
enum ParsedRef {
    Value(Exp),
    /// A base variable plus a (possibly mixed) sequence of path segments. Symbolic segments are
    /// resolved into loads by [`lower_ref`] (rvalue) or lowered to a store/callee elsewhere.
    Ap(VariableRef, ThinVec<PathSegment>),
}

impl std::fmt::Display for ParsedRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParsedRef::Value(e) => write!(f, "{e}"),
            ParsedRef::Ap(base, segments) => {
                write!(f, "{base}")?;
                for s in segments {
                    write!(f, ".{s}")?;
                }
                Ok(())
            }
        }
    }
}

/// Renders a source/sink label from a parsed operand. Local bases are resolved to their source
/// *name* (`%S`) rather than their interned display (`%L{idx}`): a label like `S` is a taint
/// category shared across functions, but interning assigns it a different [`LocalIdx`] in each
/// function, so the raw index display would make `source(S)` and `sink(_, S)` disagree.
fn label_string(local_table: &Locals, r: &ParsedRef) -> String {
    use std::fmt::Write as _;
    match r {
        ParsedRef::Value(e) => format!("{e}"),
        ParsedRef::Ap(base, segments) => {
            let mut s = String::new();
            match &base.variable {
                Variable::Local(idx) => {
                    let _ = write!(s, "%{}", local_table.name(*idx));
                    if let Some(v) = base.version {
                        let _ = write!(s, "_{v}");
                    }
                }
                _ => {
                    let _ = write!(s, "{base}");
                }
            }
            for seg in segments {
                let _ = write!(s, ".{seg}");
            }
            s
        }
    }
}

/// Lowers a parsed operand into an rvalue [`Exp`]. A value passes through; an access path is
/// lowered into a sequence of loads (appended to `data`, tagged with `source_info`) and the
/// loaded variable is returned. Fresh temporaries come from `counter`.
fn lower_ref(
    counter: &mut Counter,
    data: &mut BasicBlockData,
    local_table: &mut Locals,
    source_info: SourceInfo,
    r: ParsedRef,
) -> Exp {
    match r {
        ParsedRef::Value(e) => e,
        ParsedRef::Ap(base, segments) => {
            let mut loads = Vec::new();
            let addr = ctadl_ir::mir::load_access_path(base, segments, &mut loads, || {
                VariableRef::new_local_idx(
                    local_table.get_or_intern(&format!("t{}?", counter.next())),
                )
            });
            for mut s in loads {
                s.source_info = source_info;
                data.push_back(s);
            }
            Exp::access_path(addr)
        }
    }
}

/// Lowers a parsed callee `base ~ segments` into an offset-only callee [`AccessPath`], emitting
/// loads (appended to `data`) for any symbolic dereferences (a function pointer read from a field).
fn lower_callee_addr(
    counter: &mut Counter,
    data: &mut BasicBlockData,
    local_table: &mut Locals,
    source_info: SourceInfo,
    base: VariableRef,
    segments: ThinVec<PathSegment>,
) -> AccessPath {
    let mut loads = Vec::new();
    let addr = ctadl_ir::mir::load_access_path(base, segments, &mut loads, || {
        VariableRef::new_local_idx(local_table.get_or_intern(&format!("t{}?", counter.next())))
    });
    for mut s in loads {
        s.source_info = source_info;
        data.push_back(s);
    }
    addr
}

/// Rejects a store whose target is an offset-only location with no symbolic field (e.g. `x.[10]`).
/// A store always writes a symbolic field; a write to a bare offset address is a memory write that
/// must spell its dereference explicitly (e.g. `x.[10].deref = ..`).
fn check_store_target(segments: &[PathSegment], pos: Pos) -> Result<(), FlowyError> {
    if !segments.is_empty() && segments.iter().all(PathSegment::is_offset) {
        return Err(compile_error(
            pos,
            "cannot store to an offset-only address without a field; a store writes a symbolic \
             field, so spell the dereference explicitly (e.g. `x.[10].deref = ..`)",
        ));
    }
    Ok(())
}

/// Whether a path's variable is a global. A global is modeled as a symbolic field of the global
/// heap, so its name contributes a leading path segment that belongs to the variable itself rather
/// than to any field path spelled after it.
fn names_global(env: &Env, path: &parse::Path<'_>) -> bool {
    let name = path.base.name;
    !env.parameters.contains_key(name) && env.globals.contains(name)
}

/// A regular access path is variable + fields (as opposed to a summary access path)
fn parse_path(
    env: &Env,
    local_table: &mut Locals,
    path: &parse::Path<'_>,
) -> Result<(VariableRef, ThinVec<PathSegment>), FlowyError> {
    let name = path.base.name;
    let field_accesses = path_segments(&path.segments)?;
    Ok(if let Some(v) = env.parameters.get(name) {
        (v.clone(), field_accesses)
    } else if env.globals.contains(name) {
        // A global `name` is modeled as a symbolic field of the global heap.
        let mut global_field_accesses: ThinVec<PathSegment> = thin_vec![PathSegment::symbol(name)];
        global_field_accesses.extend(field_accesses);
        (VariableRef::new_global(), global_field_accesses)
    } else {
        // treat it as local
        (
            VariableRef::new_local_idx(local_table.get_or_intern(name)),
            field_accesses,
        )
    })
}

fn parse_ref(
    env: &Env,
    local_table: &mut Locals,
    ap: &Ap<'_>,
    defined_functions: &HashSet<String>,
) -> Result<ParsedRef, FlowyError> {
    Ok(match ap {
        Ap::Int(i) => ParsedRef::Value(Exp::new_int(*i)),
        Ap::Str(s) => ParsedRef::Value(Exp::Str((*s).into())),
        Ap::FunctionPtr(f) => {
            if !defined_functions.contains(f.name) {
                log::warn!("function '{}' is not defined", f.name);
            }
            ParsedRef::Value(Exp::ObjectRef(CallObject::FunctionPtr(ArcIntern::from(
                f.name,
            ))))
        }
        Ap::Path(path) => {
            let (base, segments) = parse_path(env, local_table, path)?;
            ParsedRef::Ap(base, segments)
        }
    })
}

fn parse_actuals(
    locals: &Env,
    local_table: &mut Locals,
    args: &[Ap<'_>],
    defined_functions: &HashSet<String>,
) -> Result<Vec<ParsedRef>, FlowyError> {
    args.iter()
        .map(|ap| parse_ref(locals, local_table, ap, defined_functions))
        .collect()
}

/// Reads a trailing integer argument, such as the `2` in `sink(x, Label, 2)`, as a path count.
/// Returns `None` for anything that is not a whole number of zero or more. A negative number
/// counts nothing, so it returns `None` too.
fn exp_to_count(e: &Exp) -> Option<usize> {
    usize::try_from(e.as_int()?).ok()
}

/// Visits the source/sink/errsource/errsink instructions and collect specs
#[derive(Debug, Default)]
struct ExtractSpec {
    function: ArcIntern<str>,
    endpoint_requires: HashMap<ArcIntern<str>, Vec<(Endpoint, FlowSpec)>>,
    /// Maps a variable in the current function to the parameter index it (transitively)
    /// copies, populated as `Assign` statements are visited. The front-end lowers
    /// `sink(c, ...)` to `t? = c; sink(t?, ...)`, so the sink's port is a temp; this lets
    /// the endpoint recover that the temp denotes parameter `c`, which is what anchors the
    /// endpoint at the function's call sites. Cleared per function.
    param_of: HashMap<VariableRef, i16>,
}

impl ExtractSpec {
    fn set_function_name(&mut self, function: ArcIntern<str>) {
        self.function = function;
        self.param_of.clear();
    }

    /// The parameter index `var` (transitively) holds, if known.
    fn formal_of(&self, var: &VariableRef) -> Option<i16> {
        match &var.variable {
            Variable::Param(idx) => idx.index().try_into().ok(),
            _ => self.param_of.get(var).copied(),
        }
    }
}

impl MutVisitor for ExtractSpec {
    fn visit_statement(&mut self, statement: &mut Statement, location: Location) {
        use StatementKind::*;
        self.super_statement(statement, location);
        let stmt = &mut statement.kind;
        // Track simple variable copies so a sink/source on a parameter-derived temp can
        // recover the parameter index (see `param_of`). Only single-source, field-free
        // copies of a parameter (or of an already-tracked variable) carry the index.
        if let Assign { dest, sources } = stmt
            && sources.len() == 1
            && let Exp::Variable(v) = &sources[0]
            && let Some(formal) = self.formal_of(v)
        {
            self.param_of.insert(dest.clone(), formal);
        }
        if let CallAssign {
            style:
                CallStyle::DirectCall {
                    call_edges: CallEdges::Explicit(edges),
                },
            rets,
            args,
        } = stmt
            && edges.len() == 1
        {
            let endpoint_name = edges[0].as_ref();
            match endpoint_name {
                "source" | "errsource" => {
                    let infunc = &self.function;
                    let port = (rets[0].clone(), OffsetAccesses::empty());
                    let endpoint = Endpoint {
                        infunc: infunc.clone(),
                        port,
                        direction: EndpointDirection::Source,
                        label: args[0].str().unwrap().to_string(),
                        // A `source`'s port is the call's return temp, not a parameter,
                        // so this is normally `None` (the source stays function-anchored).
                        formal: self.formal_of(&rets[0]),
                        source_info: statement.source_info,
                        // Optional trailing integer: `source(Label, n)`.
                        path_count: args.get(1).and_then(exp_to_count),
                    };
                    let spec = if endpoint_name == "source" {
                        FlowSpec::FlowPresent
                    } else if endpoint_name == "errsource" {
                        FlowSpec::FlowAbsent
                    } else {
                        unreachable!()
                    };
                    self.endpoint_requires
                        .entry(infunc.clone())
                        .or_default()
                        .push((endpoint, spec));
                }
                "sink" | "errsink" => {
                    let infunc = &self.function;
                    let port = (
                        args[0].variable_ref().unwrap().clone(),
                        OffsetAccesses::empty(),
                    );
                    // The port is the `t? = x` temp the front-end sinks; recover the
                    // parameter index it copies so the endpoint anchors at call sites.
                    let formal = self.formal_of(&port.0);
                    let endpoint = Endpoint {
                        infunc: infunc.clone(),
                        port,
                        direction: EndpointDirection::Sink,
                        label: args[1].str().unwrap().to_string(),
                        formal,
                        source_info: statement.source_info,
                        // Optional trailing integer: `sink(x, Label, n)`.
                        path_count: args.get(2).and_then(exp_to_count),
                    };
                    let spec = if endpoint_name == "sink" {
                        FlowSpec::FlowPresent
                    } else if endpoint_name == "errsink" {
                        FlowSpec::FlowAbsent
                    } else {
                        unreachable!()
                    };
                    self.endpoint_requires
                        .entry(infunc.clone())
                        .or_default()
                        .push((endpoint, spec));
                    // Clear the edges because this call is not a real call. It should be safe to
                    // leave them in because we disallow defining these functions.
                    edges.clear();
                }
                _ => (),
            }
        }
        // If we found a source/sink spec, nop it out because it is not a real function call.
        // if replace {
        //     *stmt = Nop;
        // }
    }
}

#[derive(Debug, Clone, Default)]
struct Counter {
    value: u32,
}

impl Counter {
    #[inline]
    fn next(&mut self) -> u32 {
        let v = self.value;
        self.value += 1u32;
        v
    }
}

#[cfg(test)]
mod path_grammar_tests {
    use super::*;
    use ctadl_ir::mir::path_syntax;

    fn compile(src: &str) -> Result<FlowyProgram, FlowyError> {
        compile_program_contents("test.tnt", src)
    }

    /// Every access-path segment flowy parses, in source order.
    fn segments_of(prog: &FlowyProgram) -> Vec<PathSegment> {
        let mut segs = Vec::new();
        for reqs in prog.requirements.summary_requires.requires.values() {
            for spec in reqs {
                segs.extend(spec.dest.fields.iter().cloned());
                segs.extend(spec.source.fields.iter().cloned());
            }
        }
        segs.sort();
        segs.dedup();
        segs
    }

    /// Anything flowy accepts, the canonical grammar also accepts and agrees on.
    ///
    /// flowy's `ident` (`[A-Za-z_][A-Za-z0-9_]*`) is a strict *subset* of the canonical symbol
    /// production and its `offset_p` is already the canonical offset, so every segment flowy
    /// produces must print and re-parse unchanged. Otherwise a path written in a `.flowy` file
    /// and the same path written in a model port would denote different things.
    #[test]
    fn flowy_segments_agree_with_the_canonical_grammar() {
        let src = r#"
def F(a, b): 1
where summaries [a.foo.bar <- b.f1.[12].f2]
{
s:
    a.foo.bar = b.f1.[12].f2;
    return a;
}
"#;
        let prog = compile(src).expect("flowy program should compile");
        let segs = segments_of(&prog);
        assert!(!segs.is_empty(), "expected segments, got none");
        assert!(
            segs.contains(&PathSegment::offset(12)),
            "flowy should parse .[12] as an offset, got {segs:?}"
        );

        for seg in &segs {
            let text = path_syntax::segment_to_string(seg);
            let back = path_syntax::parse_segment(&text).unwrap_or_else(|e| {
                panic!("flowy produced {seg:?} -> {text:?}, which failed: {e}")
            });
            assert_eq!(&back, seg, "round trip through {text:?}");
        }

        // And the whole path, not just each segment.
        let printed = path_syntax::path_to_string(&segs);
        assert_eq!(path_syntax::parse_segments(&printed).unwrap(), segs);
    }

    /// Every offset spelling flowy's `offset_p` accepts is a valid canonical offset, and both
    /// read it as the same number.
    #[test]
    fn flowy_offsets_agree_with_the_canonical_grammar() {
        for (text, expect) in [(".[0]", 0i64), (".[8]", 8), (".[255]", 255)] {
            // A store to an offset-only address must spell its dereference.
            let src = format!(
                "def F(a, b): 1\nwhere summaries [a{text}.deref <- b]\n\
                 {{\ns:\n    a{text}.deref = b;\n    return a;\n}}\n"
            );
            let prog =
                compile(&src).unwrap_or_else(|e| panic!("flowy should accept {text:?}: {e}"));
            let segs = segments_of(&prog);
            assert!(
                segs.contains(&PathSegment::offset(expect)),
                "flowy should read {text:?} as Offset({expect}), got {segs:?}"
            );
            // The canonical grammar reads the identical text the identical way.
            assert_eq!(
                path_syntax::parse_segments(text).unwrap(),
                vec![PathSegment::offset(expect)],
                "canonical reading of {text:?}"
            );
        }
    }

    /// flowy rejects the bracket forms the canonical grammar rejects, so neither can be written
    /// by accident in a `.flowy` file and mean something else in a model port.
    #[test]
    fn flowy_rejects_what_the_canonical_grammar_rejects() {
        for bad in [".[foo]", ".[]", ".[0x2a]"] {
            assert!(
                path_syntax::parse_segments(bad).is_err(),
                "{bad:?} should be canonically invalid"
            );
            let src = format!("def F(a, b): 1\n{{\ns:\n    a = b{bad};\n    return a;\n}}\n");
            assert!(compile(&src).is_err(), "flowy should also reject {bad:?}");
        }
    }

    /// `.*` used to *panic*, so `x.* = y;` in a `.flowy` file crashed the tool.
    #[test]
    fn star_path_is_a_compile_error_not_a_panic() {
        let err = compile("def F(a, b): 1\n{\ns:\n    a = b.*;\n    return a;\n}\n")
            .expect_err("'.*' should be rejected");
        match err {
            FlowyError::Compile { message, line, col } => {
                assert!(
                    message.contains("wildcard"),
                    "message should explain: {message}"
                );
                assert_eq!(line, 4, "should point at the offending line");
                assert!(col > 0);
            }
            other => panic!("expected a Compile error, got: {other:?}"),
        }
    }

    /// The same on the store side, which reaches a different walker.
    #[test]
    fn star_path_on_a_store_is_a_compile_error() {
        let err = compile("def F(a, b): 1\n{\ns:\n    a.* = b;\n    return a;\n}\n")
            .expect_err("'.*' should be rejected");
        assert!(matches!(err, FlowyError::Compile { .. }), "got {err:?}");
    }
}
