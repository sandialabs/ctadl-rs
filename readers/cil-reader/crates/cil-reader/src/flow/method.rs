//! Layer 2 (`docs/cil-flow-api.md` §4): a method's CFG, its stack heights and
//! kinds, and the Layer 1 effects with stack positions bound to depth-named
//! variables.
//!
//! An instruction at entry height `h` that pops `p` binds `In(i)` to
//! `Stack(h - p + i)` and `Out(j)` to `Stack(h - p + j)`. The bound code is
//! ordinary register-machine code: mutable variables, no phis, no value
//! numbering. What the layer computes is what a client cannot easily recover
//! from it: the CFG with exception edges, entry heights, each depth's
//! verification kind, and managed-pointer targets.
//!
//! [`MethodFlow::build`] never fails. Whatever does not add up (heights that
//! disagree at a join, an underflow, a branch to nowhere) is a
//! [`FlowWarning`], and the method is still bound as well as it can be.

use super::operands::Operands;
use super::prefix::is_prefix;
use super::{
    Const, Control, ConvType, Edge, Effect, Field, InsFlow, NewKind, Op, Place, Prefixes, Slot,
    Transfer, TypeOp,
};
use crate::cil::{Code, opcodes};
use crate::model::module::{Entity, lookup_token};
use crate::model::{ExceptionHandlerType, MethodBody, MethodSig, TypeSig};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;

/// A location-named variable. All of these are ordinary mutable variables that
/// may be assigned many times.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Var {
    Arg(u16),
    Local(u16),
    /// The stack slot at this depth (0 = bottom).
    Stack(u16),
    /// An instruction-local temporary. Numbered per instruction and reused
    /// across instructions.
    Temp(u8),
    /// The exception caught by handler `h` (an index into
    /// [`MethodFlow::handlers`]), set on entry to the handler and read by
    /// `rethrow`.
    Caught(u16),
    /// The exceptional result of the current instruction.
    Thrown,
}

impl fmt::Display for Var {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Var::Arg(n) => write!(f, "arg{n}"),
            Var::Local(n) => write!(f, "loc{n}"),
            Var::Stack(n) => write!(f, "s{n}"),
            Var::Temp(n) => write!(f, "t{n}"),
            Var::Caught(h) => write!(f, "caught{h}"),
            Var::Thrown => f.write_str("thrown"),
        }
    }
}

/// ECMA-335 §III.1.1 verification type of a stack slot. It is what decides
/// whether `ldfld`'s base is an object or a managed pointer to a struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StackKind {
    Int32,
    Int64,
    NativeInt,
    Float,
    ObjRef,
    ManagedPtr,
    ValueType(TypeOp),
    Unknown,
}

pub type BlockId = usize;

/// An instruction of a [`MethodFlow`]: its block, and its index in the
/// block's `instrs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InsId {
    pub block: BlockId,
    pub index: usize,
}

/// A set of locals and arguments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BitSet {
    locals: Vec<u64>,
    args: Vec<u64>,
}

impl BitSet {
    fn words(&self, v: Var) -> Option<(&Vec<u64>, u16)> {
        match v {
            Var::Local(n) => Some((&self.locals, n)),
            Var::Arg(n) => Some((&self.args, n)),
            _ => None,
        }
    }

    /// Adds a local or argument; other variables are ignored.
    pub fn insert(&mut self, v: Var) {
        let (words, n) = match v {
            Var::Local(n) => (&mut self.locals, n),
            Var::Arg(n) => (&mut self.args, n),
            _ => return,
        };
        let (w, b) = (n as usize / 64, n % 64);
        if words.len() <= w {
            words.resize(w + 1, 0);
        }
        words[w] |= 1 << b;
    }

    pub fn contains(&self, v: Var) -> bool {
        self.words(v).is_some_and(|(words, n)| {
            words
                .get(n as usize / 64)
                .is_some_and(|w| w & (1 << (n % 64)) != 0)
        })
    }

    /// The members, locals first, each in order.
    pub fn iter(&self) -> impl Iterator<Item = Var> + '_ {
        let bits = |words: &Vec<u64>, make: fn(u16) -> Var| {
            (0..words.len() * 64)
                .filter(move |&i| words[i / 64] & (1 << (i % 64)) != 0)
                .map(move |i| make(i as u16))
                .collect::<Vec<_>>()
        };
        bits(&self.locals, Var::Local)
            .into_iter()
            .chain(bits(&self.args, Var::Arg))
    }

    pub fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }
}

/// The kind of an exception handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HandlerKind {
    Catch,
    Filter,
    Finally,
    /// A fault clause, and any clause kind ECMA does not name (Cecil reads
    /// those as it reads a finally or fault one).
    Fault,
}

/// An exception handler, with the blocks where it starts.
#[derive(Debug, Clone, PartialEq)]
pub struct Handler {
    pub kind: HandlerKind,
    pub try_start: u32,
    pub try_end: u32,
    pub handler_start: u32,
    pub handler_end: u32,
    /// Filter clauses: where the filter starts.
    pub filter_start: Option<u32>,
    /// Catch clauses: the caught type.
    pub catch_type: Option<TypeOp>,
    /// The block an exception from the try range goes to: the filter's for
    /// a filter clause, the handler's otherwise.
    pub entry: Option<BlockId>,
    /// The handler's own first block (for a filter clause, where the filter
    /// leads when it accepts).
    pub handler_block: Option<BlockId>,
}

impl Handler {
    fn try_contains(&self, offset: u32) -> bool {
        (self.try_start..self.try_end).contains(&offset)
    }

    fn handler_contains(&self, offset: u32) -> bool {
        (self.handler_start..self.handler_end).contains(&offset)
    }

    fn filter_contains(&self, offset: u32) -> bool {
        self.filter_start
            .is_some_and(|f| (f..self.handler_start).contains(&offset))
    }

    /// Whether the handler (or filter) starts with the exception on the stack.
    fn receives_exception(&self) -> bool {
        matches!(self.kind, HandlerKind::Catch | HandlerKind::Filter)
    }
}

/// Something about a method that does not add up. The method is still
/// bound.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FlowWarning {
    /// The method's signature did not decode; there is no flow.
    Signature(String),
    /// The code stopped decoding at `offset`.
    Decode { offset: u32, message: String },
    /// Predecessors reach the block at `offset` with different heights; the
    /// smaller one is kept.
    HeightMismatch { offset: u32, heights: (u16, u16) },
    /// The instruction at `offset` pops more than the stack holds.
    Underflow { offset: u32, height: u16, pops: u16 },
    /// The stack at `offset` is deeper than the body's `max_stack`.
    ExceedsMaxStack { offset: u32, height: u16 },
    /// A prefix at `offset` does not apply to its instruction.
    StrayPrefix { offset: u32 },
    /// The instruction at `offset` has an [`Effect::Opaque`].
    Opaque { offset: u32 },
    /// Control falls off the end of the code after `offset`.
    FallsOffEnd { offset: u32 },
    /// A branch at `offset` to `target`, which is not the start of an
    /// instruction (a target inside one goes to that instruction).
    BadTarget { offset: u32, target: u32 },
    /// Handler `index` has a boundary that is not an instruction start.
    BadHandler { index: u16 },
    /// A `rethrow` at `offset` outside any catch or filter handler.
    RethrowOutsideHandler { offset: u32 },
}

impl fmt::Display for FlowWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FlowWarning::Signature(e) => write!(f, "signature does not decode: {e}"),
            FlowWarning::Decode { offset, message } => {
                write!(f, "IL_{offset:04x}: code does not decode: {message}")
            }
            FlowWarning::HeightMismatch { offset, heights } => write!(
                f,
                "IL_{offset:04x}: reached with heights {} and {}",
                heights.0, heights.1
            ),
            FlowWarning::Underflow {
                offset,
                height,
                pops,
            } => write!(f, "IL_{offset:04x}: pops {pops} from a stack of {height}"),
            FlowWarning::ExceedsMaxStack { offset, height } => {
                write!(f, "IL_{offset:04x}: height {height} exceeds max-stack")
            }
            FlowWarning::StrayPrefix { offset } => {
                write!(
                    f,
                    "IL_{offset:04x}: prefix does not apply to its instruction"
                )
            }
            FlowWarning::Opaque { offset } => write!(f, "IL_{offset:04x}: opaque effects"),
            FlowWarning::FallsOffEnd { offset } => {
                write!(f, "IL_{offset:04x}: control falls off the end of the code")
            }
            FlowWarning::BadTarget { offset, target } => {
                write!(
                    f,
                    "IL_{offset:04x}: branch to IL_{target:04x}, not an instruction"
                )
            }
            FlowWarning::BadHandler { index } => {
                write!(f, "handler {index}: a boundary is not an instruction")
            }
            FlowWarning::RethrowOutsideHandler { offset } => {
                write!(f, "IL_{offset:04x}: rethrow outside a catch handler")
            }
        }
    }
}

/// One instruction with its slots bound.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundIns {
    /// The Layer 1 record, unchanged, for reference. For the exception a
    /// handler starts with, a record of no size that pushes [`Slot::Caught`].
    pub flow: InsFlow,
    /// The same effects with slots bound, plus the copies of the collision
    /// rule at the end.
    pub effects: Vec<Effect<Var>>,
    /// The variable bound to each In(i).
    pub inputs: Vec<Var>,
    /// The variable each Out(i) ends up in.
    pub outputs: Vec<Var>,
    /// The kind of each In(i).
    pub input_kinds: Vec<StackKind>,
    /// For each In(i), the place its address points at, when that can be
    /// traced (see [`MethodFlow::resolve_address`]).
    pub input_places: Vec<Option<Place<Var>>>,
    /// The stack height before the instruction.
    pub height: u16,
}

impl BoundIns {
    /// Whether this is the exception assignment a handler starts with.
    pub fn is_handler_entry(&self) -> bool {
        self.flow.size == 0
    }
}

/// A basic block.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub start: u32,
    pub end: u32,
    /// Stack height on entry, and the kind at each depth below it. A catch,
    /// filter, or filtered handler's entry counts the exception: its first
    /// instruction is `Stack(0) := Caught(h)`, prepended by the binder.
    pub entry_height: u16,
    pub entry_kinds: Vec<StackKind>,
    /// Stack height after the last instruction (0 after leave, endfinally
    /// and endfilter, which empty it).
    pub exit_height: u16,
    pub instrs: Vec<BoundIns>,
    /// Normal successors.
    pub succs: Vec<BlockId>,
    /// Normal predecessors: the blocks with this one among their `succs`.
    pub preds: Vec<BlockId>,
    /// Handler entries reachable from here by an exception.
    pub exc_succs: Vec<BlockId>,
    /// Set when the block is a handler's or filter's entry: the handler's
    /// index.
    pub handler_entry: Option<u16>,
}

/// Options for [`MethodFlow::build_with`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FlowOptions {
    /// End a block after every `Call` or `Throw` inside a protected range,
    /// so each such point has its own exception edge.
    pub split_after_throwing_in_try: bool,
}

/// The per-method view of Layer 2.
#[derive(Debug, Clone, PartialEq)]
pub struct MethodFlow {
    pub blocks: Vec<Block>,
    pub handlers: Vec<Handler>,
    /// Height mismatches, underflows, stray prefixes, Opaque effects, and
    /// the like, sorted and without duplicates.
    pub warnings: Vec<FlowWarning>,
    pub max_stack: u32,
    address_taken: BitSet,
    escaping: HashSet<InsId>,
}

impl MethodFlow {
    /// Layer 2 for `body`. Never fails: problems are [`FlowWarning`]s.
    pub fn build(body: &MethodBody<'_>) -> MethodFlow {
        MethodFlow::build_with(body, FlowOptions::default())
    }

    pub fn build_with(body: &MethodBody<'_>, options: FlowOptions) -> MethodFlow {
        Builder::new(body, options).build()
    }

    /// The instruction `at`.
    pub fn ins(&self, at: InsId) -> &BoundIns {
        &self.blocks[at.block].instrs[at.index]
    }

    /// Every instruction, in code order, with its id.
    pub fn instructions(&self) -> impl Iterator<Item = (InsId, &BoundIns)> + '_ {
        self.blocks.iter().enumerate().flat_map(|(block, b)| {
            b.instrs
                .iter()
                .enumerate()
                .map(move |(index, ins)| (InsId { block, index }, ins))
        })
    }

    /// The instruction whose record starts at `offset` (not a handler's
    /// exception assignment).
    pub fn at_offset(&self, offset: u32) -> Option<InsId> {
        self.instructions()
            .find(|(_, ins)| ins.flow.offset == offset && !ins.is_handler_entry())
            .map(|(id, _)| id)
    }

    /// What the pointer held in input `i` of instruction `at` points at,
    /// when that can be traced syntactically: the address comes from an
    /// AddressOf through copies only, on every path, and the place's own
    /// variables have not been reassigned since. Stack bases are traced back
    /// to the argument or local they were copied from, so `ldarg.0; ldflda
    /// f` resolves to `arg0.f`.
    pub fn resolve_address(&self, at: InsId, i: u8) -> Option<&Place<Var>> {
        self.ins(at).input_places.get(i as usize)?.as_ref()
    }

    /// Locals and args whose address is taken anywhere (ldloca, ldarga).
    pub fn address_taken(&self) -> &BitSet {
        &self.address_taken
    }

    /// Whether the address that the AddressOf at `at` produces reaches an
    /// input where [`resolve_address`](Self::resolve_address) returns None
    /// (a merge of two addresses, or a place whose variables changed), so a
    /// client knows that pointer may refer to more than one place.
    pub fn address_escapes_resolution(&self, at: InsId) -> bool {
        self.escaping.contains(&at)
    }

    /// The edge view (§4.3) of every bound effect, in code order.
    pub fn edges(&self) -> impl Iterator<Item = Edge<Var>> + '_ {
        self.blocks
            .iter()
            .flat_map(|b| b.instrs.iter())
            .flat_map(|ins| ins.effects.iter().flat_map(|e| e.edges()))
    }

    /// The block whose range holds `offset`.
    pub fn block_at(&self, offset: u32) -> Option<BlockId> {
        self.blocks
            .iter()
            .position(|b| (b.start..b.end).contains(&offset))
    }
}

// ---- building ----------------------------------------------------------------

/// Stack state at a program point: the kind at each depth.
type Stack = Vec<StackKind>;

struct Builder<'b, 'a> {
    body: &'b MethodBody<'a>,
    options: FlowOptions,
    sig: Option<MethodSig>,
    records: Vec<InsFlow>,
    handlers: Vec<Handler>,
    warnings: BTreeSet<FlowWarning>,
    arg_kinds: Vec<StackKind>,
    local_kinds: Vec<StackKind>,
}

impl<'b, 'a> Builder<'b, 'a> {
    fn new(body: &'b MethodBody<'a>, options: FlowOptions) -> Self {
        Builder {
            body,
            options,
            sig: None,
            records: Vec::new(),
            handlers: Vec::new(),
            warnings: BTreeSet::new(),
            arg_kinds: Vec::new(),
            local_kinds: Vec::new(),
        }
    }

    fn warn(&mut self, w: FlowWarning) {
        self.warnings.insert(w);
    }

    fn build(mut self) -> MethodFlow {
        match self.body.method().signature() {
            Ok(sig) => self.sig = Some(sig),
            Err(e) => {
                self.warn(FlowWarning::Signature(e.to_string()));
                return self.finish(Vec::new(), BitSet::default(), HashSet::new());
            }
        }
        for flow in self.body.flows() {
            match flow {
                Ok(flow) => self.records.push(flow),
                // flows() fails only for the signature (handled above) or,
                // last, for the body's decode error.
                Err(e) => {
                    let offset = self.body.decode_error.as_ref().map_or(0, |(o, _)| *o);
                    self.warn(FlowWarning::Decode {
                        offset,
                        message: e.to_string(),
                    });
                }
            }
        }
        for i in 0..self.records.len() {
            let r = &self.records[i];
            let (offset, opaque) = (r.offset, r.is_opaque());
            let stray = !r.prefixes.is_empty()
                && (is_prefix(r.opcode.code) || !r.prefixes.apply_to(r.opcode.code));
            if opaque {
                self.warn(FlowWarning::Opaque { offset });
            }
            if stray {
                self.warn(FlowWarning::StrayPrefix { offset });
            }
        }
        self.arg_kinds = self.arg_kinds();
        self.local_kinds = self
            .body
            .variables
            .iter()
            .map(|t| kind_of_sig(self.body, t))
            .collect();
        self.handlers = self.handlers();

        let mut blocks = self.blocks();
        self.edges(&mut blocks);
        let states = self.simulate(&blocks);
        let mut built = self.bind(&blocks, &states);
        let (address_taken, escaping) = addresses(&mut built);
        self.finish(built, address_taken, escaping)
    }

    fn finish(
        self,
        blocks: Vec<Block>,
        address_taken: BitSet,
        escaping: HashSet<InsId>,
    ) -> MethodFlow {
        MethodFlow {
            blocks,
            handlers: self.handlers,
            warnings: self.warnings.into_iter().collect(),
            max_stack: self.body.max_stack,
            address_taken,
            escaping,
        }
    }

    fn arg_kinds(&self) -> Vec<StackKind> {
        let Some(sig) = &self.sig else {
            return Vec::new();
        };
        let mut kinds = Vec::new();
        if sig.has_this && !sig.explicit_this {
            let value_type = self
                .body
                .method()
                .declaring_type()
                .ok()
                .flatten()
                .is_some_and(|t| t.is_value_type().unwrap_or(false));
            kinds.push(if value_type {
                StackKind::ManagedPtr
            } else {
                StackKind::ObjRef
            });
        }
        kinds.extend(sig.params.iter().map(|p| kind_of_sig(self.body, p)));
        kinds
    }

    fn handlers(&mut self) -> Vec<Handler> {
        self.body
            .exception_handlers
            .iter()
            .map(|h| Handler {
                kind: match h.handler_type {
                    ExceptionHandlerType::Catch => HandlerKind::Catch,
                    ExceptionHandlerType::Filter => HandlerKind::Filter,
                    ExceptionHandlerType::Finally => HandlerKind::Finally,
                    ExceptionHandlerType::Fault | ExceptionHandlerType::Other(_) => {
                        HandlerKind::Fault
                    }
                },
                try_start: h.try_start,
                try_end: h.try_end,
                handler_start: h.handler_start,
                handler_end: h.handler_end,
                filter_start: h.filter_start,
                catch_type: h.catch_type.map(|token| TypeOp { token }),
                entry: None,
                handler_block: None,
            })
            .collect()
    }

    /// The index of the record whose bytes hold `offset`.
    fn record_at(&self, offset: u32) -> Option<usize> {
        let i = self.records.partition_point(|r| r.offset <= offset);
        let r = self.records.get(i.checked_sub(1)?)?;
        (offset < r.end_offset()).then_some(i - 1)
    }

    /// The record a branch to `target` from `from` goes to, with a warning
    /// when the target is not a record start.
    fn target_record(&mut self, from: u32, target: u32) -> Option<usize> {
        let r = self.record_at(target);
        if r.is_none_or(|r| self.records[r].offset != target) {
            self.warn(FlowWarning::BadTarget {
                offset: from,
                target,
            });
        }
        r
    }

    /// Splits the records into blocks; edges come later.
    fn blocks(&mut self) -> Vec<Block> {
        let n = self.records.len();
        if n == 0 {
            return Vec::new();
        }
        let mut leaders = BTreeSet::from([0usize]);
        for i in 0..n {
            let control = self.records[i].control.clone();
            let offset = self.records[i].offset;
            for &t in control.targets() {
                if let Some(r) = self.target_record(offset, t) {
                    leaders.insert(r);
                }
            }
            let throws = self.records[i]
                .effects
                .iter()
                .any(|e| matches!(e, Effect::Call(_) | Effect::Throw { .. }));
            let split = self.options.split_after_throwing_in_try
                && throws
                && self.handlers.iter().any(|h| h.try_contains(offset));
            if control != Control::Next || split {
                leaders.insert(i + 1);
            }
        }
        let code_end = self.records[n - 1].end_offset();
        for index in 0..self.handlers.len() {
            let h = &self.handlers[index];
            let bounds = [
                Some(h.try_start),
                Some(h.try_end),
                Some(h.handler_start),
                Some(h.handler_end),
                h.filter_start,
            ];
            let mut bad = false;
            for b in bounds.into_iter().flatten() {
                match self.record_at(b) {
                    Some(r) if self.records[r].offset == b => {
                        leaders.insert(r);
                    }
                    _ if b == code_end => {}
                    _ => bad = true,
                }
            }
            if bad {
                self.warn(FlowWarning::BadHandler {
                    index: index as u16,
                });
            }
        }
        leaders.retain(|&l| l < n);
        let leaders: Vec<usize> = leaders.into_iter().collect();
        leaders
            .iter()
            .enumerate()
            .map(|(k, &first)| {
                let last = leaders.get(k + 1).map_or(n, |&l| l) - 1;
                Block {
                    start: self.records[first].offset,
                    end: self.records[last].end_offset(),
                    entry_height: 0,
                    entry_kinds: Vec::new(),
                    exit_height: 0,
                    // Filled by bind; for now, the record range.
                    instrs: Vec::new(),
                    succs: Vec::new(),
                    preds: Vec::new(),
                    exc_succs: Vec::new(),
                    handler_entry: None,
                }
            })
            .collect()
    }

    fn block_of_record(blocks: &[Block], offset: u32) -> Option<BlockId> {
        let i = blocks.partition_point(|b| b.start <= offset);
        i.checked_sub(1).filter(|&b| offset < blocks[b].end)
    }

    /// Records of a block, as an index range into `records`.
    fn records_of(&self, block: &Block) -> std::ops::Range<usize> {
        let first = self.records.partition_point(|r| r.offset < block.start);
        let end = self.records.partition_point(|r| r.offset < block.end);
        first..end
    }

    fn edges(&mut self, blocks: &mut [Block]) {
        // Handler entries.
        for (index, h) in self.handlers.iter_mut().enumerate() {
            let entry_offset = h.filter_start.unwrap_or(h.handler_start);
            h.entry = Self::block_of_record(blocks, entry_offset)
                .filter(|&b| blocks[b].start == entry_offset);
            h.handler_block = Self::block_of_record(blocks, h.handler_start)
                .filter(|&b| blocks[b].start == h.handler_start);
            for b in [h.entry, h.handler_block].into_iter().flatten() {
                blocks[b].handler_entry.get_or_insert(index as u16);
            }
        }

        // Leaves: to the target, and through each finally they exit.
        let mut finally_exits: HashMap<usize, Vec<BlockId>> = HashMap::new();
        let mut succs: Vec<Vec<BlockId>> = vec![Vec::new(); blocks.len()];
        for b in 0..blocks.len() {
            let range = self.records_of(&blocks[b]);
            let last = &self.records[range.end - 1];
            let (offset, end, control) = (last.offset, last.end_offset(), last.control.clone());
            let mut out = Vec::new();
            for &t in control.targets() {
                if let Some(r) = self.target_record(offset, t) {
                    out.extend(Self::block_of_record(blocks, self.records[r].offset));
                }
            }
            if control.falls_through() {
                match Self::block_of_record(blocks, end) {
                    Some(next) => out.push(next),
                    None => self.warn(FlowWarning::FallsOffEnd { offset }),
                }
            }
            match control {
                Control::Leave(target) => {
                    // The finallys this leave exits, innermost first: each
                    // ends by going on to the next, and the last to the
                    // target. The leave reaches all of them and the target.
                    let target_block = self
                        .record_at(target)
                        .and_then(|r| Self::block_of_record(blocks, self.records[r].offset));
                    let mut exited: Vec<(usize, &Handler)> = self
                        .handlers
                        .iter()
                        .enumerate()
                        .filter(|(_, h)| {
                            h.kind == HandlerKind::Finally
                                && h.try_contains(offset)
                                && !h.try_contains(target)
                        })
                        .collect();
                    exited.sort_by_key(|(_, h)| h.try_end - h.try_start);
                    for (k, (index, h)) in exited.iter().enumerate() {
                        out.extend(h.handler_block);
                        let then = match exited.get(k + 1) {
                            Some((_, outer)) => outer.handler_block,
                            None => target_block,
                        };
                        finally_exits.entry(*index).or_default().extend(then);
                    }
                }
                Control::EndFilter => {
                    if let Some(h) = self.handlers.iter().find(|h| h.filter_contains(offset)) {
                        out.extend(h.handler_block);
                    }
                }
                _ => {}
            }
            succs[b] = out;
        }
        // endfinally: to wherever the leaves through that finally go.
        for (b, block) in blocks.iter().enumerate() {
            let range = self.records_of(block);
            let last = &self.records[range.end - 1];
            if last.control != Control::EndFinally {
                continue;
            }
            let innermost = self
                .handlers
                .iter()
                .enumerate()
                .filter(|(_, h)| {
                    matches!(h.kind, HandlerKind::Finally | HandlerKind::Fault)
                        && h.handler_contains(last.offset)
                })
                .min_by_key(|(_, h)| h.handler_end - h.handler_start);
            if let Some((index, h)) = innermost
                && h.kind == HandlerKind::Finally
            {
                succs[b].extend(finally_exits.get(&index).into_iter().flatten());
            }
        }
        for (b, out) in succs.into_iter().enumerate() {
            let mut seen = HashSet::new();
            blocks[b].succs = out.into_iter().filter(|s| seen.insert(*s)).collect();
        }
        for b in 0..blocks.len() {
            for s in blocks[b].succs.clone() {
                if !blocks[s].preds.contains(&b) {
                    blocks[s].preds.push(b);
                }
            }
        }
        // Exception edges: every block in a try range to its handler entry.
        for h in &self.handlers {
            let Some(entry) = h.entry else { continue };
            for block in blocks.iter_mut() {
                if h.try_contains(block.start) && !block.exc_succs.contains(&entry) {
                    block.exc_succs.push(entry);
                }
            }
        }
    }

    /// The fixed entry state of a handler entry block, if it is one.
    fn handler_entry_state(&self, block: &Block) -> Option<Stack> {
        let h = &self.handlers[block.handler_entry? as usize];
        Some(if h.receives_exception() {
            vec![StackKind::ObjRef]
        } else {
            Vec::new()
        })
    }

    /// Entry stacks of every block: a worklist from the entry and the handler
    /// entries, then from any block not reached (as ECMA assumes after an
    /// unconditional branch, with an empty stack).
    fn simulate(&mut self, blocks: &[Block]) -> Vec<Stack> {
        let mut entry: Vec<Option<Stack>> = vec![None; blocks.len()];
        let mut queue = VecDeque::new();
        let mut seed = |entry: &mut Vec<Option<Stack>>, b: BlockId, s: Stack| {
            if entry[b].is_none() {
                entry[b] = Some(s);
                queue.push_back(b);
            }
        };
        if !blocks.is_empty() {
            seed(&mut entry, 0, Vec::new());
        }
        for (b, block) in blocks.iter().enumerate() {
            if let Some(s) = self.handler_entry_state(block) {
                seed(&mut entry, b, s);
            }
        }
        let mut next_unvisited = 0;
        loop {
            while let Some(b) = queue.pop_front() {
                let state = entry[b].clone().unwrap();
                let exit = self.run_block(&blocks[b], state, None);
                for &s in &blocks[b].succs {
                    // A handler starts with a stack of its own, whatever
                    // leads to it (endfilter to its handler).
                    if blocks[s].handler_entry.is_some() {
                        continue;
                    }
                    let merged = match &entry[s] {
                        None => Some(exit.clone()),
                        Some(old) => self.merge(blocks[s].start, old, &exit),
                    };
                    if let Some(m) = merged {
                        entry[s] = Some(m);
                        if !queue.contains(&s) {
                            queue.push_back(s);
                        }
                    }
                }
            }
            match (next_unvisited..blocks.len()).find(|&b| entry[b].is_none()) {
                Some(b) => {
                    next_unvisited = b + 1;
                    entry[b] = Some(Vec::new());
                    queue.push_back(b);
                }
                None => break,
            }
        }
        entry.into_iter().map(|s| s.unwrap_or_default()).collect()
    }

    /// `old` met with `incoming`, or None if that is `old`. Heights that
    /// differ keep the smaller; kinds that differ become Unknown.
    fn merge(&mut self, offset: u32, old: &Stack, incoming: &Stack) -> Option<Stack> {
        if old == incoming {
            return None;
        }
        if old.len() != incoming.len() {
            self.warn(FlowWarning::HeightMismatch {
                offset,
                heights: (
                    old.len().min(incoming.len()) as u16,
                    old.len().max(incoming.len()) as u16,
                ),
            });
        }
        let merged: Stack = old
            .iter()
            .zip(incoming)
            .map(|(a, b)| if a == b { *a } else { StackKind::Unknown })
            .collect();
        (merged != *old).then_some(merged)
    }

    /// Runs a block's records from `state`; returns the exit state. With
    /// `out`, also binds each record into it.
    fn run_block(
        &mut self,
        block: &Block,
        mut state: Stack,
        mut out: Option<&mut Vec<BoundIns>>,
    ) -> Stack {
        let range = self.records_of(block);
        for r in range {
            let flow = self.records[r].clone();
            let height = state.len() as u16;
            let pops = flow.inputs.len() as u16;
            if pops > height {
                self.warn(FlowWarning::Underflow {
                    offset: flow.offset,
                    height,
                    pops,
                });
            }
            let base = height.saturating_sub(pops);
            let input_kinds: Vec<StackKind> = (0..pops)
                .map(|i| {
                    state
                        .get((base + i) as usize)
                        .copied()
                        .unwrap_or(StackKind::Unknown)
                })
                .collect();
            let out_kinds = self.out_kinds(&flow, &input_kinds);
            state.truncate(base as usize);
            state.extend(out_kinds);
            if state.len() as u32 > self.body.max_stack {
                self.warn(FlowWarning::ExceedsMaxStack {
                    offset: flow.offset,
                    height: state.len() as u16,
                });
            }
            if matches!(
                flow.control,
                Control::Leave(_) | Control::EndFinally | Control::EndFilter
            ) {
                state.clear();
            }
            if let Some(out) = out.as_deref_mut() {
                let bound = self.bind_one(flow, height, base, input_kinds);
                out.push(bound);
            }
        }
        state
    }

    /// The final pass: every block bound from its entry state.
    fn bind(&mut self, blocks: &[Block], states: &[Stack]) -> Vec<Block> {
        let mut built = Vec::with_capacity(blocks.len());
        for (b, block) in blocks.iter().enumerate() {
            let mut instrs = Vec::new();
            let mut state = states[b].clone();
            if let Some(h) = block.handler_entry
                && self.handlers[h as usize].receives_exception()
            {
                instrs.push(handler_entry(block.start, h));
                state = vec![StackKind::ObjRef];
            }
            let exit = self.run_block(block, state.clone(), Some(&mut instrs));
            built.push(Block {
                entry_height: state.len() as u16,
                entry_kinds: state,
                exit_height: exit.len() as u16,
                instrs,
                ..block.clone()
            });
        }
        built
    }

    /// The handler whose `rethrow` reads at `offset`: the innermost catch or
    /// filter handler around it.
    fn caught_at(&self, offset: u32) -> Option<u16> {
        self.handlers
            .iter()
            .enumerate()
            .filter(|(_, h)| h.receives_exception() && h.handler_contains(offset))
            .min_by_key(|(_, h)| h.handler_end - h.handler_start)
            .map(|(i, _)| i as u16)
    }

    fn bind_one(
        &mut self,
        flow: InsFlow,
        height: u16,
        base: u16,
        input_kinds: Vec<StackKind>,
    ) -> BoundIns {
        let caught = if flow.effects.iter().any(|e| {
            let mut reads = false;
            e.for_each_read(|s| reads |= *s == Slot::Caught);
            reads
        }) {
            let c = self.caught_at(flow.offset);
            if c.is_none() {
                self.warn(FlowWarning::RethrowOutsideHandler {
                    offset: flow.offset,
                });
            }
            c
        } else {
            None
        };
        let (effects, inputs, outputs) = bind_effects(&flow, base, caught);
        BoundIns {
            input_places: vec![None; inputs.len()],
            flow,
            effects,
            inputs,
            outputs,
            input_kinds,
            height,
        }
    }

    /// The kinds of the values `flow` pushes, given the kinds it pops.
    fn out_kinds(&self, flow: &InsFlow, inputs: &[StackKind]) -> Vec<StackKind> {
        let mut kinds = vec![StackKind::Unknown; flow.outputs as usize];
        let slot_kind = |s: &Slot| match *s {
            Slot::In(i) => inputs
                .get(i as usize)
                .copied()
                .unwrap_or(StackKind::Unknown),
            Slot::Arg(n) => self
                .arg_kinds
                .get(n as usize)
                .copied()
                .unwrap_or(StackKind::Unknown),
            Slot::Local(n) => self
                .local_kinds
                .get(n as usize)
                .copied()
                .unwrap_or(StackKind::Unknown),
            Slot::Caught => StackKind::ObjRef,
            Slot::Out(_) | Slot::Temp(_) | Slot::Thrown => StackKind::Unknown,
        };
        for e in &flow.effects {
            let (dst, kind) = match e {
                Effect::Assign {
                    dst,
                    srcs,
                    transfer,
                } => (dst, self.assign_kind(flow, srcs, transfer, &slot_kind)),
                Effect::Const { dst, value } => (
                    dst,
                    match value {
                        Const::I4(_) | Const::SizeOf(_) => StackKind::Int32,
                        Const::I8(_) => StackKind::Int64,
                        Const::R4(_) | Const::R8(_) => StackKind::Float,
                        Const::Null | Const::String(_) => StackKind::ObjRef,
                        Const::Token(_) | Const::ArgList => StackKind::Unknown,
                    },
                ),
                Effect::Load { dst, src } => (dst, self.load_kind(flow, &src.field)),
                Effect::AddressOf { dst, .. } => (dst, StackKind::ManagedPtr),
                Effect::New { dst, kind, ty, .. } => (
                    dst,
                    match (kind, ty) {
                        (NewKind::ValueType, Some(t)) => StackKind::ValueType(*t),
                        (NewKind::ValueType, None) => StackKind::Unknown,
                        (NewKind::Object | NewKind::Array, _) => StackKind::ObjRef,
                        (NewKind::StackAlloc, _) => StackKind::NativeInt,
                    },
                ),
                Effect::FunctionPtr { dst, .. } => (dst, StackKind::NativeInt),
                Effect::Call(site) => match &site.result {
                    Some(dst) => (dst, kind_of_sig(self.body, &site.sig.return_type)),
                    None => continue,
                },
                _ => continue,
            };
            if let Slot::Out(j) = dst
                && let Some(k) = kinds.get_mut(*j as usize)
            {
                *k = kind;
            }
        }
        kinds
    }

    fn assign_kind(
        &self,
        flow: &InsFlow,
        srcs: &[Slot],
        transfer: &Transfer,
        slot_kind: &dyn Fn(&Slot) -> StackKind,
    ) -> StackKind {
        let first = srcs.first().map_or(StackKind::Unknown, slot_kind);
        match transfer {
            Transfer::Copy => first,
            Transfer::Cast(_) | Transfer::Box(_) => StackKind::ObjRef,
            Transfer::Unbox(t) => {
                if flow.opcode.code == Code::Unbox {
                    StackKind::ManagedPtr
                } else {
                    match self.body.is_value_type(t) {
                        Some(true) => value_kind(self.body, *t),
                        _ => StackKind::Unknown,
                    }
                }
            }
            Transfer::Derived(op) => match op {
                Op::Cmp(..) => StackKind::Int32,
                Op::Conv(c) => match c.ty {
                    ConvType::I1
                    | ConvType::I2
                    | ConvType::I4
                    | ConvType::U1
                    | ConvType::U2
                    | ConvType::U4 => StackKind::Int32,
                    ConvType::I8 | ConvType::U8 => StackKind::Int64,
                    ConvType::I | ConvType::U => StackKind::NativeInt,
                    ConvType::R4 | ConvType::R8 | ConvType::R => StackKind::Float,
                },
                Op::Neg | Op::Not | Op::Shl | Op::Shr(_) => first,
                Op::Add(_)
                | Op::Sub(_)
                | Op::Mul(_)
                | Op::Div(_)
                | Op::Rem(_)
                | Op::And
                | Op::Or
                | Op::Xor => {
                    let second = srcs.get(1).map_or(StackKind::Unknown, slot_kind);
                    binary_kind(first, second)
                }
                Op::RefAnyVal(_) => StackKind::ManagedPtr,
                Op::MkRefAny(_) | Op::RefAnyType | Op::Opaque => StackKind::Unknown,
            },
        }
    }

    fn load_kind(&self, flow: &InsFlow, field: &Field<Slot>) -> StackKind {
        use Code::*;
        match field {
            Field::Named(f) => kind_of_sig(self.body, &f.ty),
            Field::Length => StackKind::NativeInt,
            Field::Element { .. } | Field::Deref | Field::Block { .. } => match flow.opcode.code {
                Ldelem_I1 | Ldelem_U1 | Ldelem_I2 | Ldelem_U2 | Ldelem_I4 | Ldelem_U4
                | Ldind_I1 | Ldind_U1 | Ldind_I2 | Ldind_U2 | Ldind_I4 | Ldind_U4 => {
                    StackKind::Int32
                }
                Ldelem_I8 | Ldind_I8 => StackKind::Int64,
                Ldelem_I | Ldind_I => StackKind::NativeInt,
                Ldelem_R4 | Ldelem_R8 | Ldind_R4 | Ldind_R8 => StackKind::Float,
                Ldelem_Ref | Ldind_Ref => StackKind::ObjRef,
                Ldelem_Any | Ldobj => self.operand_type_kind(flow),
                // A multi-dimensional array's Get: its return type.
                Call | Callvirt => self
                    .instruction(flow)
                    .and_then(|ins| Operands::method(self.body, super::opcode::token(ins)).ok())
                    .map_or(StackKind::Unknown, |m| {
                        kind_of_sig(self.body, &m.op.sig.return_type)
                    }),
                _ => StackKind::Unknown,
            },
        }
    }

    /// The instruction `flow` records (after its prefixes).
    fn instruction(&self, flow: &InsFlow) -> Option<&'b crate::cil::Ins<'a>> {
        let i = self
            .body
            .instructions
            .partition_point(|i| i.end_offset() < flow.end_offset());
        self.body
            .instructions
            .get(i)
            .filter(|i| i.end_offset() == flow.end_offset())
    }

    /// The kind of the type operand of `ldelem T`/`ldobj T`.
    fn operand_type_kind(&self, flow: &InsFlow) -> StackKind {
        let Some(ins) = self.instruction(flow) else {
            return StackKind::Unknown;
        };
        let t = TypeOp {
            token: super::opcode::token(ins),
        };
        match self.body.is_value_type(&t) {
            Some(true) => value_kind(self.body, t),
            Some(false) => StackKind::ObjRef,
            None => StackKind::Unknown,
        }
    }
}

/// The stack kind of a value of value type `t`: an enum is its integer.
fn value_kind(body: &MethodBody<'_>, t: TypeOp) -> StackKind {
    match lookup_token(body.m, t.token) {
        Ok(Some(Entity::TypeDef(d))) if d.is_enum().unwrap_or(false) => StackKind::Int32,
        Ok(Some(Entity::TypeRef(r))) => match r.primitive() {
            Ok(Some(p)) => kind_of_sig(body, &p),
            _ => StackKind::ValueType(t),
        },
        _ => StackKind::ValueType(t),
    }
}

/// ECMA-335 §III.1.5, Table 2, loosely: the result kind of binary numeric
/// operations.
fn binary_kind(a: StackKind, b: StackKind) -> StackKind {
    use StackKind::*;
    match (a, b) {
        (Int32, Int32) => Int32,
        (Int64, Int64) => Int64,
        (Float, Float) => Float,
        (Int32 | NativeInt, Int32 | NativeInt) => NativeInt,
        (ManagedPtr, Int32 | NativeInt) | (Int32 | NativeInt, ManagedPtr) => ManagedPtr,
        (ManagedPtr, ManagedPtr) => NativeInt,
        _ => Unknown,
    }
}

/// The stack kind of a value of signature type `t`.
fn kind_of_sig(body: &MethodBody<'_>, t: &TypeSig) -> StackKind {
    match t.unwrap_modifiers() {
        TypeSig::Boolean
        | TypeSig::Char
        | TypeSig::I1
        | TypeSig::U1
        | TypeSig::I2
        | TypeSig::U2
        | TypeSig::I4
        | TypeSig::U4 => StackKind::Int32,
        TypeSig::I8 | TypeSig::U8 => StackKind::Int64,
        TypeSig::R4 | TypeSig::R8 => StackKind::Float,
        TypeSig::IntPtr | TypeSig::UIntPtr | TypeSig::Ptr(_) | TypeSig::FnPtr(_) => {
            StackKind::NativeInt
        }
        TypeSig::String | TypeSig::Object | TypeSig::SzArray(_) | TypeSig::Array { .. } => {
            StackKind::ObjRef
        }
        TypeSig::ByRef(_) => StackKind::ManagedPtr,
        TypeSig::Named {
            token,
            is_value_type,
        } => {
            let t = TypeOp { token: *token };
            match body.is_value_type(&t).unwrap_or(*is_value_type) {
                true => value_kind(body, t),
                false => StackKind::ObjRef,
            }
        }
        TypeSig::GenericInst {
            is_value_type, ty, ..
        } => {
            if *is_value_type {
                StackKind::ValueType(TypeOp { token: *ty })
            } else {
                StackKind::ObjRef
            }
        }
        TypeSig::Void
        | TypeSig::TypedByRef
        | TypeSig::Var(_)
        | TypeSig::MVar(_)
        | TypeSig::CModReq { .. }
        | TypeSig::CModOpt { .. }
        | TypeSig::Pinned(_)
        | TypeSig::Sentinel(_) => StackKind::Unknown,
    }
}

/// The record a catch or filter handler starts with: it pushes the
/// exception.
fn handler_entry(offset: u32, h: u16) -> BoundIns {
    let flow = InsFlow {
        offset,
        size: 0,
        opcode: opcodes::NOP,
        prefixes: Prefixes::default(),
        inputs: Vec::new(),
        outputs: 1,
        effects: vec![Effect::Assign {
            dst: Slot::Out(0),
            srcs: vec![Slot::Caught],
            transfer: Transfer::Copy,
        }],
        control: Control::Next,
    };
    BoundIns {
        flow,
        effects: vec![Effect::Assign {
            dst: Var::Stack(0),
            srcs: vec![Var::Caught(h)],
            transfer: Transfer::Copy,
        }],
        inputs: Vec::new(),
        outputs: vec![Var::Stack(0)],
        input_kinds: Vec::new(),
        input_places: Vec::new(),
        height: 0,
    }
}

/// Binds `flow`'s slots for an instruction whose operands start at depth
/// `base`, applying the collision rule: an Out(j) written before a later
/// effect reads In(j), which shares its depth, goes through a Temp, copied
/// to its depth at the end. A plain copy of In(j) to Out(j) leaves the depth
/// as it was and needs none.
///
/// Returns the bound effects, the inputs' and the outputs' variables.
pub(crate) fn bind_effects(
    flow: &InsFlow,
    base: u16,
    caught: Option<u16>,
) -> (Vec<Effect<Var>>, Vec<Var>, Vec<Var>) {
    let mut next_temp = 0u8;
    for e in &flow.effects {
        let mut note = |s: &Slot| {
            if let Slot::Temp(t) = s {
                next_temp = next_temp.max(t + 1);
            }
        };
        e.for_each_read(&mut note);
        e.for_each_write(&mut note);
    }
    let mut out_vars: Vec<Var> = (0..flow.outputs as u16)
        .map(|j| Var::Stack(base + j))
        .collect();
    let mut copies = Vec::new();
    for j in 0..flow.outputs {
        if (j as usize) >= flow.inputs.len() {
            continue;
        }
        let Some(k) = flow.effects.iter().position(|e| {
            let mut writes = false;
            e.for_each_write(|s| writes |= *s == Slot::Out(j));
            writes
        }) else {
            continue;
        };
        let identity = matches!(
            &flow.effects[k],
            Effect::Assign { dst, srcs, transfer: Transfer::Copy }
                if *dst == Slot::Out(j) && srcs == &[Slot::In(j)]
        );
        let read_later = flow.effects[k + 1..].iter().any(|e| {
            let mut reads = false;
            e.for_each_read(|s| reads |= *s == Slot::In(j));
            reads
        });
        if read_later && !identity {
            let t = Var::Temp(next_temp);
            next_temp += 1;
            copies.push(Effect::Assign {
                dst: Var::Stack(base + j as u16),
                srcs: vec![t],
                transfer: Transfer::Copy,
            });
            out_vars[j as usize] = t;
        }
    }
    let bind = |s: Slot| match s {
        Slot::In(i) => Var::Stack(base + i as u16),
        Slot::Out(j) => out_vars
            .get(j as usize)
            .copied()
            .unwrap_or(Var::Stack(base + j as u16)),
        Slot::Arg(n) => Var::Arg(n),
        Slot::Local(n) => Var::Local(n),
        Slot::Temp(t) => Var::Temp(t),
        Slot::Thrown => Var::Thrown,
        Slot::Caught => Var::Caught(caught.unwrap_or(u16::MAX)),
    };
    let mut effects: Vec<Effect<Var>> = flow.effects.iter().cloned().map(|e| e.map(bind)).collect();
    effects.extend(copies);
    let inputs = (0..flow.inputs.len() as u16)
        .map(|i| Var::Stack(base + i))
        .collect();
    let outputs = (0..flow.outputs as u16)
        .map(|j| Var::Stack(base + j))
        .collect();
    (effects, inputs, outputs)
}

// ---- managed pointers (§6.3) ------------------------------------------------

/// What the address analysis knows at a program point.
#[derive(Clone, Default, PartialEq)]
struct AddrState {
    /// Must: the variable holds the address of this place.
    addr: HashMap<Var, Place<Var>>,
    /// Must: the stack variable holds the same value as this arg or local.
    copy: HashMap<Var, Var>,
    /// May: the AddressOf instructions whose address the variable may hold.
    may: HashMap<Var, BTreeSet<InsId>>,
}

fn place_vars(p: &Place<Var>) -> Vec<Var> {
    match p {
        Place::Field(base, _) => vec![*base],
        Place::Element(base, indices) => std::iter::once(*base)
            .chain(indices.iter().copied())
            .collect(),
        Place::Local(_) | Place::Arg(_) | Place::Static(_) => Vec::new(),
    }
}

impl AddrState {
    /// Forgets what `v` held, and every fact that depends on its old value.
    fn kill(&mut self, v: Var) {
        self.addr.remove(&v);
        self.copy.remove(&v);
        self.may.remove(&v);
        self.addr.retain(|_, p| !place_vars(p).contains(&v));
        self.copy.retain(|_, src| *src != v);
    }

    /// Forgets the stack, which leave, endfinally and endfilter empty.
    fn clear_stack(&mut self) {
        let stack: Vec<Var> = self
            .addr
            .keys()
            .chain(self.copy.keys())
            .chain(self.may.keys())
            .copied()
            .filter(|v| matches!(v, Var::Stack(_)))
            .collect();
        for v in stack {
            self.kill(v);
        }
        self.addr
            .retain(|_, p| !place_vars(p).iter().any(|v| matches!(v, Var::Stack(_))));
    }

    /// Stack variables replaced by the arg or local they copy.
    fn canonical(&self, v: Var) -> Var {
        self.copy.get(&v).copied().unwrap_or(v)
    }

    /// The meet of two states: what both know.
    fn meet(&self, other: &AddrState) -> AddrState {
        let mut may = self.may.clone();
        for (v, s) in &other.may {
            may.entry(*v).or_default().extend(s.iter().copied());
        }
        AddrState {
            addr: self
                .addr
                .iter()
                .filter(|(v, p)| other.addr.get(v) == Some(p))
                .map(|(v, p)| (*v, p.clone()))
                .collect(),
            copy: self
                .copy
                .iter()
                .filter(|(v, s)| other.copy.get(v) == Some(s))
                .map(|(v, s)| (*v, *s))
                .collect(),
            may,
        }
    }

    /// Runs one instruction's bound effects.
    fn step(&mut self, id: InsId, ins: &BoundIns) {
        for e in &ins.effects {
            match e {
                Effect::AddressOf { dst, place } => {
                    let place = match place.clone() {
                        Place::Field(base, f) => Place::Field(self.canonical(base), f),
                        Place::Element(base, indices) => Place::Element(
                            self.canonical(base),
                            indices.into_iter().map(|i| self.canonical(i)).collect(),
                        ),
                        p => p,
                    };
                    self.kill(*dst);
                    if !place_vars(&place).contains(dst) {
                        self.addr.insert(*dst, place);
                    }
                    self.may.insert(*dst, BTreeSet::from([id]));
                }
                Effect::Assign {
                    dst,
                    srcs,
                    transfer: Transfer::Copy,
                } if srcs.len() == 1 => {
                    let src = srcs[0];
                    let addr = self.addr.get(&src).cloned();
                    let may = self.may.get(&src).cloned();
                    let canonical = match src {
                        Var::Arg(_) | Var::Local(_) => Some(src),
                        _ => self.copy.get(&src).copied(),
                    };
                    if *dst == src {
                        continue;
                    }
                    self.kill(*dst);
                    // As for AddressOf: a place that mentions `dst` named
                    // its old value, which the copy has just overwritten.
                    if let Some(a) = addr
                        && !place_vars(&a).contains(dst)
                    {
                        self.addr.insert(*dst, a);
                    }
                    if let Some(m) = may {
                        self.may.insert(*dst, m);
                    }
                    if let (Var::Stack(_), Some(c)) = (dst, canonical)
                        && c != *dst
                    {
                        self.copy.insert(*dst, c);
                    }
                }
                _ => e.for_each_write(|v| self.kill(*v)),
            }
        }
    }
}

/// The address analyses over bound blocks: fills each instruction's
/// `input_places`, and returns the address-taken locals and args and the
/// AddressOf instructions whose address escapes resolution.
fn addresses(blocks: &mut [Block]) -> (BitSet, HashSet<InsId>) {
    let mut taken = BitSet::default();
    for b in blocks.iter() {
        for ins in &b.instrs {
            for e in &ins.effects {
                if let Effect::AddressOf { place, .. } = e {
                    match place {
                        Place::Local(n) => taken.insert(Var::Local(*n)),
                        Place::Arg(n) => taken.insert(Var::Arg(*n)),
                        _ => {}
                    }
                }
            }
        }
    }

    // Must/may forward analysis. A handler entry starts knowing nothing.
    let n = blocks.len();
    let mut entry: Vec<Option<AddrState>> = vec![None; n];
    let mut queue = VecDeque::new();
    for (b, block) in blocks.iter().enumerate() {
        if b == 0 || block.handler_entry.is_some() || block.preds.is_empty() {
            entry[b] = Some(AddrState::default());
            queue.push_back(b);
        }
    }
    while let Some(b) = queue.pop_front() {
        let mut state = entry[b].clone().unwrap();
        for (index, ins) in blocks[b].instrs.iter().enumerate() {
            state.step(InsId { block: b, index }, ins);
        }
        if matches!(
            blocks[b].instrs.last().map(|i| &i.flow.control),
            Some(Control::Leave(_) | Control::EndFinally | Control::EndFilter)
        ) {
            state.clear_stack();
        }
        for &s in &blocks[b].succs {
            let new = match &entry[s] {
                None => state.clone(),
                Some(old) => old.meet(&state),
            };
            if entry[s].as_ref() != Some(&new) {
                entry[s] = Some(new);
                if !queue.contains(&s) {
                    queue.push_back(s);
                }
            }
        }
    }

    let mut escaping = HashSet::new();
    for (b, block) in blocks.iter_mut().enumerate() {
        let mut state = entry[b].clone().unwrap_or_default();
        for (index, ins) in block.instrs.iter_mut().enumerate() {
            for (i, v) in ins.inputs.iter().enumerate() {
                let place = state.addr.get(v).cloned();
                if place.is_none()
                    && let Some(sites) = state.may.get(v)
                {
                    escaping.extend(sites.iter().copied());
                }
                ins.input_places[i] = place;
            }
            state.step(InsId { block: b, index }, ins);
        }
    }
    (taken, escaping)
}
