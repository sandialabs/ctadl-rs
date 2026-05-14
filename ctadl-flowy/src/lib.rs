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

use hashbrown::{hash_map::HashMap, hash_set::HashSet};
use internment::ArcIntern;
use pest::{Parser, Span, iterators::Pair};
use smallvec::{SmallVec, smallvec};
use thiserror::Error;

use crate::parse::{FlowyParser, Rule};
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
    pub fields: FieldAccesses,
}

impl Display for Port {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Port { base, fields } = self;
        write!(f, "{base}{fields}")
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
    pub port: (VariableRef, FieldAccesses),
    /// The taint label
    pub label: String,
    pub direction: EndpointDirection,
    pub source_info: SourceInfo,
}

impl Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Endpoint {
            infunc,
            port,
            label,
            direction,
            source_info,
        } = self;
        write!(
            f,
            "{}@{}: {}{} is a {} label '{}'",
            infunc, source_info, port.0, port.1, direction, label
        )?;
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
    #[error("pest parsing error")]
    Pest(#[from] pest::error::Error<crate::parse::Rule>),
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
    let contents = std::fs::read_to_string(file)?;
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
        let parse = FlowyParser::parse(Rule::top, contents)
            .map_err(FlowyError::Pest)?
            .next()
            .unwrap();
        // Parse the var defs first so we know the set of globals
        let mut func_defs = Vec::new();
        let mut defined_functions = HashSet::new();
        for p in parse
            .into_inner()
            .filter(|pair| pair.as_rule() == Rule::def)
        {
            let p = p.into_inner().next().unwrap();
            match p.as_rule() {
                Rule::function_def => {
                    let name = p.clone().into_inner().next().unwrap().as_str().to_string();
                    defined_functions.insert(name);
                    func_defs.push(p);
                }
                Rule::var_def => self.parse_var_def(p)?,
                _ => panic!("unexpected def"),
            }
        }
        for p in func_defs {
            self.parse_function_def(p, &defined_functions)?;
        }
        Ok(())
    }

    /// Parse the formals into a map so that we can look them up as they are referenced.
    fn parse_function_def(
        &mut self,
        pair: Pair<'_, Rule>,
        defined_functions: &HashSet<String>,
    ) -> Result<(), FlowyError> {
        assert!(pair.as_rule() == Rule::function_def);
        // Function we're building
        let function = self.program.new_function();
        let mut env: Env = Default::default();
        env.globals.extend(self.toplevel_vars.iter().cloned());

        let mut kids = pair.into_inner();
        let name_parse = kids.next().unwrap();
        let name: String = name_parse.as_str().into();
        if ["source", "sink", "errsource", "errsink"]
            .iter()
            .any(|s| *s == name)
        {
            let (line, col) = name_parse.line_col();
            return Err(FlowyError::Compile {
                message: format!("name is reserved: '{}'", &name),
                line,
                col,
            });
        }
        self.program[function].name = name;

        let params_parse = kids.next().unwrap();
        self.parse_formals(params_parse, &mut env, function)?;

        let mut pair = kids.next().unwrap();
        // ret declaration?
        if let Rule::return_arity = pair.as_rule() {
            self.parse_return_arity(pair, function);
            pair = kids.next().unwrap();
        }
        // where clause?
        if let Rule::where_clause = pair.as_rule() {
            self.parse_where_clause(pair, &env, function);
            pair = kids.next().unwrap();
        }
        let Rule::block_list = pair.as_rule() else {
            panic!("bug: expected block list");
        };

        // Parse the blocks, associating block labels with their basic block number and goto
        // targets
        // let mut terminators: HashMap<BlockLabel, (BasicBlockIdx, Option<GotoTargets>)> =
        //     HashMap::new();
        let mut terminators = HashMap::new();
        for p in pair.into_inner() {
            // New basic block to fill in
            let block = self.program[function]
                .blocks
                .blocks_mut()
                .push(BasicBlockData::new(None));
            let (label, targets) = self.parse_block(p, &env, function, block, defined_functions)?;
            terminators.insert(label, (block, targets));
        }

        // Once we've parsed all the blocks, validate the target labels and convert them to basic
        // block indices and generate the goto instructions.
        for (_, (block, info)) in terminators.iter() {
            let Some(info) = info else {
                continue;
            };
            let (targets, span) = info;
            let source_info = targets.source_info;
            let targets = targets
                .targets
                .iter()
                .map(|target| match terminators.get(target) {
                    None => {
                        let BlockLabel(s) = target;
                        let (line, col) = span.start_pos().line_col();
                        Err(FlowyError::Compile {
                            message: format!("goto refers to nonexistent block: '{s}'"),
                            line,
                            col,
                        })
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

    fn parse_var_def(&mut self, pair: Pair<'_, Rule>) -> Result<(), FlowyError> {
        // We just need to record this typing up references to this global
        let v = pair.into_inner().next().unwrap();
        self.toplevel_vars.insert(v.as_str().to_string());
        Ok(())
    }

    /// Parse formals and append them to the function. Adds to the locals map the variables for the
    /// parameters
    fn parse_formals(
        &mut self,
        pair: Pair<'_, Rule>,
        locals: &mut Env,
        function: FunctionIdx,
    ) -> Result<(), FlowyError> {
        let formals: Vec<(String, ParameterType)> = pair
            .into_inner()
            .map(|param| {
                let mut elts = param.into_inner();
                let ident = elts.next().unwrap();
                let ty = match elts.next() {
                    Some(style) => match style.as_str() {
                        "byref" => ParameterType::ByRef,
                        "byval" => ParameterType::ByVal,
                        _ => panic!("bug: unexpected param style"),
                    },
                    None => ParameterType::ByVal,
                };
                (ident.as_str().into(), ty)
            })
            .collect();
        let params = &mut self.program[function].params;
        for (formal, ty) in formals {
            params.parameters.push(ty);
            let index = params.last_index().unwrap();
            locals.parameters.insert(
                formal.clone(),
                VariableRef::new_var_ref(ArcIntern::new(Variable::Param(index))),
            );
        }
        Ok(())
    }

    /// Parses a basic block and returns its label and any goto targets from the terminator.
    fn parse_block<'p>(
        &mut self,
        pair: Pair<'p, Rule>,
        locals: &Env,
        function: FunctionIdx,
        block: BasicBlockIdx,
        defined_functions: &HashSet<String>,
    ) -> Result<(BlockLabel, Option<(GotoTargets, Span<'p>)>), FlowyError> {
        let mut block_pairs = pair.into_inner();
        let label = BlockLabel(block_pairs.next().unwrap().as_str().to_string());
        let mut goto_targets = None;
        for stmt_or_terminator in block_pairs {
            let stmt_or_terminator = stmt_or_terminator.into_inner().next().unwrap();
            match stmt_or_terminator.as_rule() {
                Rule::goto_stmt => {
                    goto_targets = Some(self.parse_goto(stmt_or_terminator, function, block));
                }
                _ => self.parse_stmt_or_terminator(
                    stmt_or_terminator,
                    locals,
                    function,
                    block,
                    defined_functions,
                )?,
            }
        }
        Ok((label, goto_targets))
    }

    /// Parses a statement and appends it to the basic block.
    ///
    /// Precondition: the statement is not a goto, since goto's require postprocessing.
    fn parse_stmt_or_terminator(
        &mut self,
        stmt_pair: Pair<'_, Rule>,
        locals: &Env,
        function: FunctionIdx,
        block: BasicBlockIdx,
        defined_functions: &HashSet<String>,
    ) -> Result<(), FlowyError> {
        use StatementKind::*;
        let data = &mut self.program[function][block];
        let source_info = SourceInfo::new({
            let span = stmt_pair.as_span();
            let start = span.start().try_into().unwrap();
            let len =
                source_info::SpanLen::ByteLen((span.end() - span.start() + 1).try_into().unwrap());
            self.source_info_builder
                .span_for(self.artifact_key.clone(), start, len)
        });
        match stmt_pair.as_rule() {
            Rule::assign_stmt => {
                let (line, col) = stmt_pair.line_col();
                let mut inner = stmt_pair.into_inner();
                let dst = parse_ap(locals, inner.next().unwrap(), defined_functions)?;
                // src is comma-separated
                let src = {
                    let mut result = Vec::new();
                    for p in inner.next().unwrap().into_inner() {
                        result.push(parse_exp(locals, p, defined_functions));
                    }
                    result
                };
                let assign = {
                    if dst.path.is_empty() {
                        StatementKind::assign(dst.variable_ref, src)
                    } else if src.len() > 1 {
                        return Err(FlowyError::Compile {
                            message: "cannot update a field with multiple sources".to_string(),
                            line,
                            col,
                        });
                    } else {
                        StatementKind::assign_or_update(dst, src[0].clone())
                    }
                };
                data.push_back(Statement::new(assign, source_info));
            }
            Rule::assign_call_stmt => {
                let (line, col) = stmt_pair.line_col();
                let mut inner = stmt_pair.into_inner();
                let lhs = parse_ap(locals, inner.next().unwrap(), defined_functions)?;
                let callee = match parse_exp(locals, inner.next().unwrap(), defined_functions) {
                    Exp::AccessPath(ap) => ap,
                    _ => {
                        return Err(FlowyError::Compile {
                            message: "bad call ap".to_string(),
                            line,
                            col,
                        });
                    }
                };
                let AccessPath {
                    variable_ref: variable,
                    path,
                } = callee;
                let actuals = parse_actuals(locals, inner.next().unwrap(), defined_functions);
                let style = if !path.is_empty() {
                    // Indirect call
                    CallStyle::FuncPtrCall {
                        callee: AccessPath {
                            variable_ref: variable,
                            path,
                        },
                        signature: None,
                    }
                } else {
                    let is_direct = match variable.variable.as_ref() {
                        Variable::Local(name) => {
                            name == "source"
                                || name == "errsource"
                                || defined_functions.contains(name)
                        }
                        _ => false,
                    };

                    if is_direct {
                        let Variable::Local(name) = variable.variable.as_ref() else {
                            unreachable!()
                        };
                        CallStyle::DirectCall {
                            call_edges: CallEdges::Explicit(vec![name.to_string()].into()),
                        }
                    } else {
                        // Indirect call with parameter, global, or undefined function
                        CallStyle::FuncPtrCall {
                            callee: AccessPath {
                                variable_ref: variable,
                                path,
                            },
                            signature: None,
                        }
                    }
                };

                let args: SmallVec<[Exp; 4]> = {
                    if let CallStyle::DirectCall {
                        call_edges: CallEdges::Explicit(edges),
                    } = &style
                        && (edges[0] == "source" || edges[0] == "errsource")
                    {
                        actuals
                            .into_iter()
                            .map(|x| Exp::Str(format!("{x}").into()))
                            .collect()
                    } else {
                        actuals.into_iter().collect()
                    }
                };

                // use a temporary for the result of the call
                let tmp = VariableRef::new_local(format!("t{}?", self.counter.next()));
                let call = CallAssign {
                    style,
                    rets: smallvec![tmp.clone()],
                    args,
                };
                data.push_back(Statement::new(call, source_info));

                // assign the temporary to the field (if applicable)
                let assign_lhs =
                    StatementKind::assign_or_update(lhs.clone(), Exp::AccessPath(tmp.into()));
                data.push_back(Statement::new(assign_lhs, source_info));
            }
            Rule::call_stmt => {
                let (line, col) = stmt_pair.line_col();
                let mut inner = stmt_pair.into_inner();
                let callee = match parse_exp(locals, inner.next().unwrap(), defined_functions) {
                    Exp::AccessPath(ap) => ap,
                    _ => {
                        return Err(FlowyError::Compile {
                            message: "bad call ap".to_string(),
                            line,
                            col,
                        });
                    }
                };
                let AccessPath {
                    variable_ref: variable,
                    path,
                } = callee;
                let actuals = parse_actuals(locals, inner.next().unwrap(), defined_functions);

                let style = if !path.is_empty() {
                    // Indirect call
                    CallStyle::FuncPtrCall {
                        callee: AccessPath {
                            variable_ref: variable,
                            path,
                        },
                        signature: None,
                    }
                } else {
                    let is_direct = match variable.variable.as_ref() {
                        Variable::Local(name) => {
                            name == "sink" || name == "errsink" || defined_functions.contains(name)
                        }
                        _ => false,
                    };

                    if is_direct {
                        let Variable::Local(name) = variable.variable.as_ref() else {
                            unreachable!()
                        };
                        CallStyle::DirectCall {
                            call_edges: CallEdges::Explicit(vec![name.to_string()].into()),
                        }
                    } else {
                        // Indirect call with parameter, global, or undefined function
                        CallStyle::FuncPtrCall {
                            callee: AccessPath {
                                variable_ref: variable,
                                path,
                            },
                            signature: None,
                        }
                    }
                };

                let args: SmallVec<[Exp; 4]> = if let CallStyle::DirectCall {
                    call_edges: CallEdges::Explicit(edges),
                } = &style
                    && (edges[0] == "sink" || edges[0] == "errsink")
                {
                    // Parses `sink(x.y.z, Test)` into `t0 = x.y.z; sink(t0, Test)` so that when
                    // the sink call is removed, x.y.z remains in the program
                    let tmp = VariableRef::new_local(format!("t{}?", self.counter.next()));
                    let mut args = SmallVec::with_capacity(actuals.len());
                    // first two original args
                    let mut orig_args: SmallVec<[Exp; 4]> = SmallVec::new();
                    for (i, x) in actuals.into_iter().enumerate() {
                        if i == 0 {
                            // use a temporary for the sink argument
                            args.push(Exp::AccessPath(AccessPath::without_fields(tmp.clone())));
                            orig_args.push(x);
                        } else if i == 1 {
                            args.push(Exp::Str(format!("{x}").into()));
                            orig_args.push(x);
                        } else {
                            args.push(x)
                        }
                    }
                    let assign_tmp =
                        StatementKind::assign_or_update(tmp.into(), orig_args[0].clone());
                    data.push_back(Statement::new(assign_tmp, source_info));
                    args
                } else {
                    actuals.into_iter().collect()
                };
                let rets = smallvec![];
                //let args = actuals.into_iter().map(|x| Exp::AccessPath(x));
                let call = CallAssign { style, rets, args };
                data.push_back(Statement::new(call, source_info));
            }
            Rule::return_stmt => {
                let mut inner = stmt_pair.into_inner();
                let terminator = inner
                    .next()
                    .map(|var| {
                        let src = parse_exp(locals, var, defined_functions);
                        TerminatorKind::Return {
                            args: vec![src].into(),
                        }
                    })
                    .unwrap_or_else(|| TerminatorKind::Return {
                        args: vec![].into(),
                    });
                data.terminator = Some(Terminator::new(terminator, source_info));
            }
            Rule::goto_stmt => panic!("bug: unexpected goto"),
            _ => log::warn!("skipping instruction: {}", stmt_pair.as_str()),
        }
        Ok(())
    }

    /// Parses a goto statement into its target labels. Returns the span of the labels for
    /// reporting error messages.
    fn parse_goto<'p>(
        &mut self,
        stmt_pair: Pair<'p, Rule>,
        _function: FunctionIdx,
        _block: BasicBlockIdx,
    ) -> (GotoTargets, Span<'p>) {
        let source_info = SourceInfo::new({
            let span = stmt_pair.as_span();
            let start = span.start().try_into().unwrap();
            let len =
                source_info::SpanLen::ByteLen((span.end() - span.start() + 1).try_into().unwrap());
            self.source_info_builder
                .span_for(self.artifact_key.clone(), start, len)
        });
        match stmt_pair.as_rule() {
            Rule::goto_stmt => {
                let span = stmt_pair.as_span();
                let targets = stmt_pair.into_inner();
                (
                    GotoTargets {
                        targets: targets
                            .into_iter()
                            .map(|t| BlockLabel(t.as_str().to_string()))
                            .collect(),
                        source_info,
                    },
                    span,
                )
            }
            _ => panic!("bug: not a goto: {}", stmt_pair.as_str()),
        }
    }

    fn parse_return_arity(&mut self, pair: Pair<'_, Rule>, function: FunctionIdx) {
        assert!(pair.as_rule() == Rule::return_arity);
        let arity: u8 = <str>::parse(pair.into_inner().next().unwrap().as_str().trim()).unwrap();
        let return_type = ReturnType { arity };
        self.program.functions[function].set_return_type(return_type);
    }

    fn parse_where_clause(&mut self, pair: Pair<'_, Rule>, locals: &Env, function: FunctionIdx) {
        assert!(pair.as_rule() == Rule::where_clause);
        pair.into_inner()
            .next()
            .unwrap()
            .into_inner()
            .for_each(|e| self.parse_summary_entry(e, locals, function))
    }

    fn parse_summary_entry(&mut self, pair: Pair<'_, Rule>, locals: &Env, function: FunctionIdx) {
        // TODO this can just parse into a local type instead of indexing by function name
        let mut inner = pair.into_inner();
        let lhs = parse_summary_ap(locals, inner.next().unwrap());
        let flow = parse_summary_op(&inner.next().unwrap());
        let rhs = parse_summary_ap(locals, inner.next().unwrap());

        let target = ArcIntern::<str>::from(self.program[function].name.clone());

        self.summary_requires
            .entry(target.clone())
            .or_default()
            .push(SummarySpec {
                dest: lhs,
                flow,
                source: rhs,
            });
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
fn parse_summary_ap(env: &Env, pair: Pair<'_, Rule>) -> Port {
    assert!(pair.as_rule() == Rule::summary_ap);
    let arm = pair.into_inner().next().unwrap();
    // let ap = parse_ap(locals, arm);
    let mut inner = arm.into_inner();
    // name could be a number, like 3, in which case the P vec is empty
    let name: String = inner.next().unwrap().as_str().into();
    let ps: Vec<FieldAccess> = inner.map(parse_p).collect();
    let field_accesses = FieldAccesses::from_iter(ps.clone());
    if name == "return" {
        Port {
            base: PortBase::Return,
            fields: field_accesses,
        }
    } else {
        // The port has to refer to a formal, so error if it doesn't
        env.parameters
            .get(&name)
            .map(|v| Port {
                base: PortBase::Var(v.clone()),
                fields: field_accesses.clone(),
            })
            // try the global
            .or_else(|| {
                if env.globals.contains(&name) {
                    let mut global_field_accesses = FieldAccesses::from_iter(std::iter::once(
                        FieldAccess::Symbol(ArcIntern::from(name.clone())),
                    ));
                    global_field_accesses
                        .fields
                        .extend(field_accesses.fields.clone());
                    Some(Port {
                        base: PortBase::Var(VariableRef::new_global()),
                        fields: global_field_accesses,
                    })
                } else {
                    None
                }
            })
            .unwrap_or_else(|| {
                panic!("In summary requires, found nonexistent formal reference: {name}")
            })
    }
}

fn parse_summary_op(pair: &Pair<'_, Rule>) -> FlowSpec {
    let op = pair.as_str();
    match op {
        "<-" => FlowSpec::FlowPresent,
        "</-" => FlowSpec::FlowAbsent,
        _ => panic!("bug: unexpected summary op: {op}"),
    }
}

fn parse_p(pair: Pair<'_, Rule>) -> FieldAccess {
    let inner = pair.into_inner().next().unwrap();
    match inner.as_rule() {
        Rule::field_p => {
            // skip the leading "."
            let field_name = inner.into_inner().next().unwrap().as_str();
            FieldAccess::Symbol(ArcIntern::from(field_name))
        }
        Rule::offset_p => {
            // Parse the numeric offset from .[int] syntax
            let offset_str = inner.into_inner().next().unwrap().as_str();
            let offset: i64 = offset_str.parse().unwrap();
            FieldAccess::Offset(Offset(offset))
        }
        _ => panic!("Unexpected field access rule"),
    }
}

fn parse_ap(
    parameters: &Env,
    pair: Pair<'_, Rule>,
    defined_functions: &HashSet<String>,
) -> Result<AccessPath, FlowyError> {
    let (line, col) = pair.line_col();
    match parse_exp(parameters, pair, defined_functions) {
        Exp::AccessPath(ap) => Ok(ap),
        _ => Err(FlowyError::Compile {
            message: "bad lhs ap".to_string(),
            line,
            col,
        }),
    }
}

/// A regular access path is variable + fields (as opposed to a summary access path)
fn parse_exp(env: &Env, pair: Pair<'_, Rule>, defined_functions: &HashSet<String>) -> Exp {
    // int | string | ident ~ p* | function_ptr
    let mut iter = pair.into_inner();
    let first = iter.next().unwrap();
    match first.as_rule() {
        Rule::int => {
            let i: u32 = <str>::parse(first.as_str()).unwrap();
            Exp::Bytes(i.to_be_bytes().to_vec())
        }
        Rule::string => Exp::Str(first.into_inner().next().unwrap().as_str().into()),
        Rule::function_ptr => {
            let name = first.into_inner().next().unwrap().as_str();
            if !defined_functions.contains(name) {
                log::warn!("function '{}' is not defined", name);
            }
            Exp::ObjectRef(CallObject::FunctionPtr(ArcIntern::from(name)))
        }
        _ => {
            let name: String = first.as_str().into();
            let ps: Vec<FieldAccess> = iter.map(parse_p).collect();
            let field_accesses = FieldAccesses::from_iter(ps.clone());
            env.parameters
                .get(&name)
                // try the parameter
                .map(|v| {
                    Exp::AccessPath(AccessPath {
                        variable_ref: v.clone(),
                        path: field_accesses.clone(),
                    })
                })
                // try the global
                .or_else(|| {
                    if env.globals.contains(&name) {
                        let mut global_field_accesses = FieldAccesses::from_iter(std::iter::once(
                            FieldAccess::Symbol(ArcIntern::from(name.clone())),
                        ));
                        global_field_accesses
                            .fields
                            .extend(field_accesses.fields.clone());
                        Some(Exp::AccessPath(AccessPath {
                            variable_ref: VariableRef::new_global(),
                            path: global_field_accesses,
                        }))
                    } else {
                        None
                    }
                })
                // treat it as local
                .unwrap_or_else(|| {
                    Exp::AccessPath(AccessPath {
                        variable_ref: VariableRef::new_local(name.clone()),
                        path: field_accesses,
                    })
                })
        }
    }
}

fn parse_actuals(
    locals: &Env,
    pair: Pair<'_, Rule>,
    defined_functions: &HashSet<String>,
) -> Vec<Exp> {
    assert!(pair.as_rule() == Rule::actuals);
    pair.into_inner()
        .map(|ap| parse_exp(locals, ap, defined_functions))
        .collect()
}

/// Visits the source/sink/errsource/errsink instructions and collect specs
#[derive(Debug, Default)]
struct ExtractSpec {
    function: ArcIntern<str>,
    endpoint_requires: HashMap<ArcIntern<str>, Vec<(Endpoint, FlowSpec)>>,
}

impl ExtractSpec {
    fn set_function_name(&mut self, function: ArcIntern<str>) {
        self.function = function;
    }
}

impl MutVisitor for ExtractSpec {
    fn visit_statement(&mut self, statement: &mut Statement, location: Location) {
        use StatementKind::*;
        self.super_statement(statement, location);
        let stmt = &mut statement.kind;
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
                    let port = (rets[0].clone(), FieldAccesses::empty());
                    let endpoint = Endpoint {
                        infunc: infunc.clone(),
                        port,
                        direction: EndpointDirection::Source,
                        label: args[0].str().unwrap().to_string(),
                        source_info: statement.source_info,
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
                        args[0].access_path().unwrap().variable_ref.clone(),
                        args[0].access_path().unwrap().path.clone(),
                    );
                    let endpoint = Endpoint {
                        infunc: infunc.clone(),
                        port,
                        direction: EndpointDirection::Sink,
                        label: args[1].str().unwrap().to_string(),
                        source_info: statement.source_info,
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
