/*!
The flowy language.

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

A single field may be updated on the left-hand side. For a field update, the comma operator may not
be used on the right hand side. To express multiple flows into a field, first assign a variable,
then update the field: `tmp = a, b; c.foo = tmp;`. The comma operator is used on the right-hand
side to merge multiple flows into a variable.

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

use crate::parse::{LineIndex, Operand, Pos};
use ctadl_ir::index::idx::Idx;
use ctadl_ir::mir::call::VirtualMethodTable;
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

/// A port is an access path in a summary or source-sink requirement: a [`PortBase`] (a variable,
/// or the return value) and its path, `fields`. See [`SummaryRequires`] and [`EndpointRequires`]
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
    pub port: AccessPath,
    /// The taint label
    pub label: String,
    pub direction: EndpointDirection,
    /// The parameter index this endpoint's port denotes, if it is (a copy of) a parameter.
    /// `Some` anchors the endpoint at the function's call sites (the call-arg for this
    /// formal); `None` (a local such as a `source`'s returned value, or a global) keeps it
    /// function-anchored.
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
            "{}@{}: {} is a {} label '{}'",
            infunc, source_info, port, direction, label
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
    /// Parameters by name.
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

/// A sink of a flowy program, named as the semantics corpus names it.
#[derive(Debug, Clone)]
pub struct NamedSink<'p> {
    pub function: String,
    pub endpoint: &'p Endpoint,
    /// `sink(<label>)#<n>` or `errsink(<label>)#<n>`, `n` numbering the function's sinks of
    /// that kind in source order.
    pub name: String,
    /// Whether the flow is required (`sink`) or forbidden (`errsink`).
    pub present: bool,
}

impl FlowyProgram {
    /// Every source endpoint, with its function.
    pub fn sources(&self) -> Vec<(String, &Endpoint)> {
        let mut out: Vec<(String, &Endpoint)> = self
            .requirements
            .endpoint_requires
            .requires
            .iter()
            .flat_map(|(f, eps)| eps.iter().map(move |(e, _)| (f.to_string(), e)))
            .filter(|(_, e)| matches!(e.direction, EndpointDirection::Source))
            .collect();
        out.sort_by_key(|(f, e)| (f.clone(), e.source_info));
        out
    }

    /// Every sink endpoint, named as [`NamedSink::name`] says, in function then source order.
    pub fn named_sinks(&self) -> Vec<NamedSink<'_>> {
        let mut functions: Vec<_> = self
            .requirements
            .endpoint_requires
            .requires
            .iter()
            .collect();
        functions.sort_by(|a, b| a.0.cmp(b.0));
        let mut out = Vec::new();
        for (function, endpoints) in functions {
            let mut endpoints: Vec<_> = endpoints
                .iter()
                .filter(|(e, _)| matches!(e.direction, EndpointDirection::Sink))
                .collect();
            endpoints.sort_by_key(|(e, _)| e.source_info);
            let mut n: HashMap<&str, usize> = HashMap::new();
            for (endpoint, spec) in endpoints {
                let present = matches!(spec, FlowSpec::FlowPresent);
                let kind = if present { "sink" } else { "errsink" };
                let k = n.entry(kind).or_default();
                out.push(NamedSink {
                    function: function.to_string(),
                    endpoint,
                    name: format!("{kind}({})#{k}", endpoint.label),
                    present,
                });
                *k += 1;
            }
        }
        out
    }
}

/// Compiles a flowy program to the IR it lowers to, before SSA conversion and before any pass.
/// This is the input of the IR pass pipeline; [`compile_program_contents`] is the analysis's.
pub fn compile_ir(artifact_name: &str, contents: &str) -> Result<Program, FlowyError> {
    let mut ctx = FlowyCtx::new(artifact_name);
    ctx.parse(contents)?;
    ctx.program.verify()?;
    Ok(ctx.program)
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
        vmt: VirtualMethodTable::named_by_ir(&ctx.program),
        program: ctx.program,
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

    /// Builds the function `def` defines: its formals, where clause, blocks and gotos.
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
                VariableRef::new_parameter(index),
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
                        vec![lower_ref(r)]
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
                let dest = parse_access_path(locals, local_table, dest)?;
                // src is comma-separated; each source is an access path, a `value(..)` of one,
                // or a constant. An address destination `p.[k] = e` is rewritten by
                // `StatementKind::assign` into `p = e.[-k]`.
                let mut src = Vec::new();
                for p in sources {
                    src.push(match p {
                        Operand::Value(ap, _) => {
                            Exp::Value(parse_access_path(locals, local_table, ap)?)
                        }
                        p => lower_ref(parse_ref(locals, local_table, p, defined_functions)?),
                    });
                }
                data.push_back(Statement::new(
                    StatementKind::assign(dest, src),
                    source_info,
                ));
            }
            parse::StmtKind::AssignCall { dest, callee, args } => {
                let dest = parse_access_path(locals, local_table, dest)?;
                let callee = parse_access_path(locals, local_table, callee)?;
                let actuals = parse_actuals(locals, local_table, args, defined_functions)?;
                let style = lower_call_style(
                    local_table,
                    callee,
                    &["source", "errsource"],
                    defined_functions,
                );

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
                        args.push(lower_ref(x));
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

                // assign the temporary to the destination: a copy, or a store to a path.
                data.push_back(Statement::new(
                    StatementKind::assign(dest, [Exp::variable(tmp)]),
                    source_info,
                ));
            }
            parse::StmtKind::Call { callee, args } => {
                let callee = parse_access_path(locals, local_table, callee)?;
                let actuals = parse_actuals(locals, local_table, args, defined_functions)?;

                let style =
                    lower_call_style(local_table, callee, &["sink", "errsink"], defined_functions);

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
                            args.push(Exp::variable(tmp.clone()));
                            port_ref = Some(x);
                        } else if i == 1 {
                            args.push(Exp::Str(label_string(local_table, &x).into()));
                        } else {
                            args.push(lower_ref(x));
                        }
                    }
                    // `t0 = x.y.z`, the port read into a temporary.
                    if let Some(port) = port_ref {
                        let port_val = lower_ref(port);
                        let assign_tmp = StatementKind::assign(tmp.clone(), [port_val]);
                        data.push_back(Statement::new(assign_tmp, source_info));
                    }
                    args
                } else {
                    let mut args = ThinVec::with_capacity(actuals.len());
                    for x in actuals {
                        args.push(lower_ref(x));
                    }
                    args
                };
                let rets = thin_vec![];
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
fn parse_summary_ap(env: &Env, ap: &parse::AccessPath<'_>) -> Result<Port, FlowyError> {
    let name = ap.base.name;
    let field_accesses = path_segments(&ap.segments)?;
    if name == "return" {
        return Ok(Port {
            base: PortBase::Return,
            fields: field_accesses,
        });
    }
    // The port has to refer to a formal or a global.
    if let Some(v) = env.parameters.get(name) {
        Ok(Port {
            base: PortBase::Var(v.clone()),
            fields: field_accesses,
        })
    } else if env.globals.contains(name) {
        let mut global_field_accesses: ThinVec<PathSegment> = thin_vec![PathSegment::field(name)];
        global_field_accesses.extend(field_accesses);
        Ok(Port {
            base: PortBase::Var(VariableRef::new_global()),
            fields: global_field_accesses,
        })
    } else {
        panic!("In summary requires, found nonexistent formal reference: {name}")
    }
}

/// Converts a path (the segments of a syntactic access path), rejecting `.*`.
///
/// The grammar accepts `.*`, but flowy has no wildcard field, so there is no [`PathSegment`] to
/// return for one: it is a positioned compile error.
fn path_segments(segments: &[parse::Segment<'_>]) -> Result<ThinVec<PathSegment>, FlowyError> {
    segments
        .iter()
        .map(|s| match s {
            parse::Segment::Field(name) => Ok(PathSegment::field(*name)),
            parse::Segment::Deref => Ok(PathSegment::Deref),
            parse::Segment::Value => Ok(PathSegment::Value),
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

/// A parsed operand: either a value (constant / function pointer) or an [`AccessPath`]. A field
/// read is not expressible as an [`Exp`], so an access path with fields must be lowered into loads
/// (see [`lower_ref`]) before it can be used as an rvalue; when used as an lvalue or callee its
/// path is consumed directly.
enum ParsedRef {
    Value(Exp),
    /// An access path, whose path may mix offsets and symbolic fields. Symbolic segments are
    /// resolved into loads by [`lower_ref`] (rvalue) or lowered to a store/callee elsewhere.
    AccessPath(AccessPath),
}

impl std::fmt::Display for ParsedRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParsedRef::Value(e) => write!(f, "{e}"),
            ParsedRef::AccessPath(AccessPath { base, segments }) => {
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
        ParsedRef::AccessPath(AccessPath { base, segments }) => {
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

/// Lowers a parsed operand into an rvalue [`Exp`]: a value passes through, and an access path is
/// the operand itself.
fn lower_ref(r: ParsedRef) -> Exp {
    match r {
        ParsedRef::Value(e) => e,
        ParsedRef::AccessPath(ap) => Exp::Path(ap),
    }
}

/// Chooses how to call `callee`: directly when it names one of `builtins` or a function defined in
/// this program, otherwise through a function pointer read at the callee's access path.
fn lower_call_style(
    local_table: &mut Locals,
    callee: AccessPath,
    builtins: &[&str],
    defined_functions: &HashSet<String>,
) -> CallStyle {
    if !callee.is_pathless() {
        return CallStyle::FuncPtrCall {
            callee,
            signature: None,
        };
    }
    let variable = callee.base;
    if let Variable::Local(idx) = &variable.variable {
        let name = local_table.name(*idx);
        if builtins.contains(&name) || defined_functions.contains(name) {
            return CallStyle::DirectCall {
                call_edges: CallEdges::Explicit(vec![name.to_string()].into()),
            };
        }
    }
    CallStyle::FuncPtrCall {
        callee: AccessPath::from(variable),
        signature: None,
    }
}

/// Resolves a syntactic access path in a function body (as opposed to a summary access path, see
/// [`parse_summary_ap`]) to an [`AccessPath`]. A global becomes the global heap with the global's
/// name as the path's leading field.
fn parse_access_path(
    env: &Env,
    local_table: &mut Locals,
    ap: &parse::AccessPath<'_>,
) -> Result<AccessPath, FlowyError> {
    let name = ap.base.name;
    let field_accesses = path_segments(&ap.segments)?;
    Ok(if let Some(v) = env.parameters.get(name) {
        AccessPath::new(v.clone(), field_accesses)
    } else if env.globals.contains(name) {
        let mut global_field_accesses: ThinVec<PathSegment> = thin_vec![PathSegment::field(name)];
        global_field_accesses.extend(field_accesses);
        AccessPath::new(VariableRef::new_global(), global_field_accesses)
    } else {
        // treat it as local
        AccessPath::new(
            VariableRef::new_local_idx(local_table.get_or_intern(name)),
            field_accesses,
        )
    })
}

fn parse_ref(
    env: &Env,
    local_table: &mut Locals,
    operand: &Operand<'_>,
    defined_functions: &HashSet<String>,
) -> Result<ParsedRef, FlowyError> {
    Ok(match operand {
        Operand::Int(i) => ParsedRef::Value(Exp::new_int(*i)),
        Operand::Str(s) => ParsedRef::Value(Exp::Str((*s).into())),
        Operand::FunctionPtr(f) => {
            if !defined_functions.contains(f.name) {
                log::warn!("function '{}' is not defined", f.name);
            }
            ParsedRef::Value(Exp::ObjectRef(CallObject::FunctionPtr(ArcIntern::from(
                f.name,
            ))))
        }
        Operand::AccessPath(ap) => ParsedRef::AccessPath(parse_access_path(env, local_table, ap)?),
        Operand::Value(_, pos) => {
            return Err(compile_error(
                *pos,
                "`value(..)` is only an assignment source: write `t = value(x); f(t)`",
            ));
        }
    })
}

fn parse_actuals(
    locals: &Env,
    local_table: &mut Locals,
    args: &[Operand<'_>],
    defined_functions: &HashSet<String>,
) -> Result<Vec<ParsedRef>, FlowyError> {
    args.iter()
        .map(|operand| parse_ref(locals, local_table, operand, defined_functions))
        .collect()
}

/// Reads a trailing integer argument, such as the `2` in `sink(x, Label, 2)`, as a path count.
/// Returns `None` for anything that is not a whole number of zero or more. A negative number
/// counts nothing, so it returns `None` too.
fn exp_to_count(e: &Exp) -> Option<usize> {
    usize::try_from(e.as_int()?).ok()
}

/// Visits the source/sink/errsource/errsink instructions and collects specs.
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
            && dest.is_pathless()
            && sources.len() == 1
            && let Some(v) = sources[0].variable_ref()
            && let Some(formal) = self.formal_of(v)
        {
            self.param_of.insert(dest.base.clone(), formal);
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
                    let port = AccessPath::from(rets[0].clone());
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
                    let port = AccessPath::from(args[0].variable_ref().unwrap().clone());
                    let formal = self.formal_of(&port.base);
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

    /// Every path segment flowy parses, in source order.
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

    /// An address destination `p.[8] = e` is accepted and lowered by the S5 rewrite into the
    /// copy `p = e.[-8]`, and `p.[8] = e.[8]` into `p = e`.
    #[test]
    fn address_destination_is_rewritten() {
        let src = "def F(e): 1\n{\ns:\n    p.[8] = e;\n    q.[8] = e.[8];\n    return p;\n}\n";
        let prog = compile(src).expect("flowy accepts an address destination");
        let stmts: Vec<String> = prog
            .program_info
            .program
            .functions
            .iter()
            .flat_map(|f| f.blocks.iter().flat_map(|b| b.statements.iter()))
            .map(|s| s.kind.to_string())
            .collect();
        assert!(
            stmts
                .iter()
                .any(|s: &String| s.ends_with(" = @p0_0.[-8]") && s.starts_with("assign ")),
            "{stmts:?}"
        );
        assert!(
            stmts
                .iter()
                .any(|s: &String| s.ends_with(" = @p0_0") && s.starts_with("assign %")),
            "{stmts:?}"
        );
        prog.program_info
            .program
            .verify()
            .expect("the rewritten program verifies");
    }

    /// `.deref` and `.#value` are the segments, not fields, and `.*` is a compile error.
    #[test]
    fn deref_and_value_are_segments() {
        let src = "def F(a, b): 1\n{\ns:\n    a.deref = b;\n    c = a.#value;\n    return c;\n}\n";
        let prog = compile(src).expect("flowy accepts .deref and .#value");
        let segs: Vec<PathSegment> = prog
            .program_info
            .program
            .functions
            .iter()
            .flat_map(|f| f.blocks.iter().flat_map(|b| b.statements.iter()))
            .flat_map(|s| match &s.kind {
                StatementKind::Assign { dest, sources } => std::iter::once(dest)
                    .chain(sources.iter().filter_map(Exp::access_path))
                    .flat_map(|ap| ap.segments.iter().cloned())
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect();
        assert!(segs.contains(&PathSegment::Deref), "{segs:?}");
        assert!(segs.contains(&PathSegment::Value), "{segs:?}");
        assert!(!segs.contains(&PathSegment::field("deref")), "{segs:?}");
        let bad = "def F(a, b): 1\n{\ns:\n    a.* = b;\n    return a;\n}\n";
        assert!(compile(bad).is_err(), "`.*` is not a segment");
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

    /// `.*` is a positioned compile error, not a panic.
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
