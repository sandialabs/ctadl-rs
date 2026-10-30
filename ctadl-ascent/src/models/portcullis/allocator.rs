/*! The `allocator` relation: the functions that are a C heap allocator's entry points.

A statically linked allocator keeps its heap state in globals and derives bin addresses from the
request size, so its summary relates nearly every `$globals` path to the size and to each other,
and every transitive caller inherits that block (`problem.md`, cause 2). The defaults skip these
bodies. This relation says which functions they are, two ways:

- **By name** (`by = "name"`): a function whose simple name is in [`NAMED`].
- **By shape** (`by = "shape"`): a function with no allocator name that has dlmalloc's shape. This
  is what finds the allocator in a stripped binary, where it is `FUN_0042d400`.

Both are native code only: a Java method named `free` is not an allocator.

# The shape

dlmalloc keeps a header word before every block it hands out, so `free` and `realloc` read their
pointer argument at `-4` or `-8`. Hardly any other function reads before the start of a pointer
it was passed. Those two are the *anchors*.

The *heap state* is the globals the anchors write themselves. A write is the function's own when
no call shares its statement: the pcode frontend reloads and re-stores, at each call, the globals
the callee may change, and those copies say nothing about the function. A global that many
functions write (a thread's `errno` slot, a lock) is not heap state either.

A function is picked when it writes the heap state heavily ([`MIN_HEAP_GLOBALS`],
[`MIN_HEAP_STORES`]), is called directly, and has an allocator's signature:

| kind      | anchor | parameters | returns | direct call sites |
|-----------|--------|------------|---------|-------------------|
| `free`    | yes    | 1          | any     | at least 1        |
| `realloc` | yes    | 2          | a value | at least 1        |
| `malloc`  | no     | 1          | a value | at least 3        |

On the 15 sweep binaries that keep their allocator's names, with the names hidden, this finds
`malloc`, `free` and `realloc` in every one that links them statically, and nothing else. On the
15 stripped ones it finds the same three that were picked by hand. It misses the small wrappers
(`calloc`, `malloc_consolidate`), whose bodies do not carry the block, and entry-point aliases
nothing calls directly.

A false pick drops every flow through that function's body, so every pick is reported (see
`cli::index`), and `analyze(F)` keeps a body the defaults would skip.
*/

use hashbrown::{HashMap, HashSet};

use ctadl_ir::mir::call::{CallEdges, CallStyle};
use ctadl_ir::mir::{AccessPath, Exp, FunctionData, OffsetAccess, StatementKind, Variable};

use crate::facts::Str;

use super::relations::FunRow;

/// What an allocator entry point does, as far as the defaults care.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum AllocatorKind {
    /// Returns a fresh block.
    Malloc,
    /// Returns a block holding its first argument's contents.
    Realloc,
    /// Releases its argument.
    Free,
    /// Neither, but works on the heap state: `mallopt`, `malloc_trim`.
    Internal,
}

impl AllocatorKind {
    /// The `kind` attribute's value.
    pub fn name(self) -> &'static str {
        match self {
            AllocatorKind::Malloc => "malloc",
            AllocatorKind::Realloc => "realloc",
            AllocatorKind::Free => "free",
            AllocatorKind::Internal => "internal",
        }
    }
}

/// How a function was found to be an allocator.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Evidence {
    Name,
    Shape(Shape),
}

impl Evidence {
    /// The `by` attribute's value.
    pub fn name(self) -> &'static str {
        match self {
            Evidence::Name => "name",
            Evidence::Shape(_) => "shape",
        }
    }
}

/// The measurements a by-shape pick rests on, for its report.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Shape {
    /// Accesses at `-4`, `-8` or `-16` off a pointer the function was passed.
    pub header_accesses: u32,
    /// Distinct heap-state globals the function writes itself.
    pub heap_globals: u32,
    /// Its own stores into them.
    pub heap_stores: u32,
    /// Direct call sites in the import.
    pub call_sites: u32,
}

/// One row of `allocator`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AllocatorRow {
    pub function: Str,
    pub kind: AllocatorKind,
    pub by: Evidence,
}

/// The allocator names, without leading underscores: the pcode frontend strips them from a
/// function's simple name, so `__libc_malloc` arrives as `libc_malloc`.
pub const NAMED: &[(&str, AllocatorKind)] = &[
    ("malloc", AllocatorKind::Malloc),
    ("calloc", AllocatorKind::Malloc),
    ("memalign", AllocatorKind::Malloc),
    ("posix_memalign", AllocatorKind::Malloc),
    ("aligned_alloc", AllocatorKind::Malloc),
    ("valloc", AllocatorKind::Malloc),
    ("pvalloc", AllocatorKind::Malloc),
    ("libc_malloc", AllocatorKind::Malloc),
    ("libc_calloc", AllocatorKind::Malloc),
    ("libc_memalign", AllocatorKind::Malloc),
    ("libc_valloc", AllocatorKind::Malloc),
    ("libc_pvalloc", AllocatorKind::Malloc),
    ("int_malloc", AllocatorKind::Malloc),
    ("int_memalign", AllocatorKind::Malloc),
    ("heap_alloc", AllocatorKind::Malloc),
    ("heap_alloc_at", AllocatorKind::Malloc),
    ("malloc_from_heap", AllocatorKind::Malloc),
    ("realloc", AllocatorKind::Realloc),
    ("reallocarray", AllocatorKind::Realloc),
    ("reallocf", AllocatorKind::Realloc),
    ("libc_realloc", AllocatorKind::Realloc),
    ("int_realloc", AllocatorKind::Realloc),
    ("free", AllocatorKind::Free),
    ("cfree", AllocatorKind::Free),
    ("libc_free", AllocatorKind::Free),
    ("int_free", AllocatorKind::Free),
    ("heap_free", AllocatorKind::Free),
    ("malloc_usable_size", AllocatorKind::Internal),
    ("malloc_trim", AllocatorKind::Internal),
    ("mallopt", AllocatorKind::Internal),
    ("mallinfo", AllocatorKind::Internal),
    ("malloc_stats", AllocatorKind::Internal),
    ("malloc_consolidate", AllocatorKind::Internal),
    ("sysmalloc", AllocatorKind::Internal),
];

/// An anchor writes at least this many globals' worth of its own stores. `free` and `realloc`
/// make 130-1,800; a function that merely reads a header word makes a handful.
const MIN_ANCHOR_STORES: u32 = 20;

/// A global more functions than this write themselves is shared state (`errno`, a lock), not
/// the heap's.
const MAX_HEAP_GLOBAL_WRITERS: u32 = 10;

/// A pick writes at least this many heap-state globals: dlmalloc's entry points write 5-18.
pub const MIN_HEAP_GLOBALS: u32 = 3;

/// A pick makes at least this many of its own stores into the heap state: dlmalloc's entry
/// points make 130-1,800, and a function of a few dozen stores does not carry the block.
pub const MIN_HEAP_STORES: u32 = 100;

/// `malloc` has many callers. `free` and `realloc` are pinned down by their header access, so one
/// will do.
const MIN_MALLOC_CALL_SITES: u32 = 3;

/// The `allocator` rows of a native program: `funs` is its `fun` table and `lowered` its
/// functions with IR.
pub fn find(funs: &[FunRow], lowered: &[(Str, &FunctionData)]) -> Vec<AllocatorRow> {
    let named: HashMap<&str, AllocatorKind> = NAMED.iter().copied().collect();
    let mut rows: Vec<AllocatorRow> = funs
        .iter()
        .filter_map(|row| {
            let kind = row
                .name
                .into_iter()
                .chain([row.fq])
                .find_map(|n| named.get(n.as_str().trim_start_matches('_')))
                .copied()?;
            Some(AllocatorRow {
                function: row.fq,
                kind,
                by: Evidence::Name,
            })
        })
        .collect();
    let by_name: HashSet<Str> = rows.iter().map(|r| r.function).collect();
    rows.extend(
        by_shape(lowered)
            .into_iter()
            .filter(|r| !by_name.contains(&r.function)),
    );
    rows.sort_unstable_by(|a, b| a.function.content_cmp(&b.function));
    rows
}

/// What [`by_shape`] measures of one function body.
struct Body {
    function: Str,
    params: usize,
    returns: bool,
    header_accesses: u32,
    /// The function's own stores, by global address.
    own_stores: HashMap<i64, u32>,
}

/// The functions with dlmalloc's shape; see the module docs.
fn by_shape(lowered: &[(Str, &FunctionData)]) -> Vec<AllocatorRow> {
    let mut call_sites: HashMap<Str, u32> = HashMap::new();
    let bodies: Vec<Body> = lowered
        .iter()
        .map(|&(function, data)| measure(function, data, &mut call_sites))
        .collect();

    let mut writers: HashMap<i64, u32> = HashMap::new();
    for body in &bodies {
        for &addr in body.own_stores.keys() {
            *writers.entry(addr).or_default() += 1;
        }
    }
    let is_anchor =
        |b: &Body| b.header_accesses > 0 && b.own_stores.values().sum::<u32>() >= MIN_ANCHOR_STORES;
    let heap: HashSet<i64> = bodies
        .iter()
        .filter(|b| is_anchor(b))
        .flat_map(|b| b.own_stores.keys().copied())
        .filter(|addr| writers[addr] <= MAX_HEAP_GLOBAL_WRITERS)
        .collect();
    if heap.is_empty() {
        return Vec::new();
    }

    bodies
        .iter()
        .filter_map(|b| {
            let shape = Shape {
                header_accesses: b.header_accesses,
                heap_globals: b.own_stores.keys().filter(|a| heap.contains(*a)).count() as u32,
                heap_stores: b
                    .own_stores
                    .iter()
                    .filter(|(a, _)| heap.contains(*a))
                    .map(|(_, n)| n)
                    .sum(),
                call_sites: call_sites.get(&b.function).copied().unwrap_or(0),
            };
            if shape.heap_globals < MIN_HEAP_GLOBALS
                || shape.heap_stores < MIN_HEAP_STORES
                || shape.call_sites == 0
            {
                return None;
            }
            let kind = match (is_anchor(b), b.params, b.returns) {
                (true, 1, _) => AllocatorKind::Free,
                (true, 2, true) => AllocatorKind::Realloc,
                (false, 1, true) if shape.call_sites >= MIN_MALLOC_CALL_SITES => {
                    AllocatorKind::Malloc
                }
                _ => return None,
            };
            Some(AllocatorRow {
                function: b.function,
                kind,
                by: Evidence::Shape(shape),
            })
        })
        .collect()
}

/// One pass over a body: its own global stores and header accesses, and the direct calls it
/// makes, which are added to `call_sites`.
fn measure(function: Str, data: &FunctionData, call_sites: &mut HashMap<Str, u32>) -> Body {
    let statements = || data.blocks.iter().flat_map(|b| b.statements.iter());

    let mut call_spans = HashSet::new();
    for stmt in statements() {
        if let StatementKind::CallAssign { style, .. } = &stmt.kind {
            call_spans.insert(stmt.source_info.span_id);
            if let CallStyle::DirectCall {
                call_edges: CallEdges::Explicit(edges),
            } = style
            {
                for edge in edges {
                    *call_sites.entry(Str::from(edge.as_str())).or_default() += 1;
                }
            }
        }
    }

    // The locals computed from a parameter. Pointer arithmetic is an assign, so this follows a
    // pointer from the parameter to wherever the body offsets it. Before SSA a local can be
    // assigned anywhere, hence the passes to a fixpoint.
    let mut derived: HashSet<u32> = HashSet::new();
    let from_param = |v: &Variable, derived: &HashSet<u32>| match v {
        Variable::Param(_) => true,
        Variable::Local(l) => derived.contains(&u32::from(*l)),
        Variable::GlobalHeap => false,
    };
    loop {
        let before = derived.len();
        for stmt in statements() {
            if let StatementKind::Assign { dest, sources } = &stmt.kind
                && let Variable::Local(l) = dest.variable
                && !derived.contains(&u32::from(l))
                && sources
                    .iter()
                    .filter_map(Exp::base_variable)
                    .any(|v| from_param(&v.variable, &derived))
            {
                derived.insert(u32::from(l));
            }
        }
        if derived.len() == before {
            break;
        }
    }

    let is_header = |path: &AccessPath| {
        from_param(&path.base.variable, &derived)
            && path.accesses.offsets.iter().any(|OffsetAccess::Offset(o)| {
                // Some lowerings carry the borrow into the high word (`.[17179869176]` is -8);
                // the low 32 bits are the offset either way.
                matches!(
                    (o.0 as u64) & 0xffff_ffff,
                    0xffff_fffc | 0xffff_fff8 | 0xffff_fff0
                )
            })
    };
    let mut header_accesses = 0;
    let mut own_stores: HashMap<i64, u32> = HashMap::new();
    for stmt in statements() {
        let paths: &mut dyn Iterator<Item = &AccessPath> = match &stmt.kind {
            StatementKind::Store { dest, .. } => {
                if dest.base.variable == Variable::GlobalHeap
                    && !call_spans.contains(&stmt.source_info.span_id)
                    && let Some(OffsetAccess::Offset(addr)) = dest.accesses.offsets.first()
                {
                    *own_stores.entry(addr.0).or_default() += 1;
                }
                &mut std::iter::once(dest)
            }
            StatementKind::Load { source, .. } => &mut std::iter::once(source),
            StatementKind::Update { dest, .. } => &mut std::iter::once(dest),
            StatementKind::Assign { sources, .. } => &mut sources.iter().filter_map(|e| match e {
                Exp::AccessPath(p) => Some(p),
                _ => None,
            }),
            _ => &mut std::iter::empty(),
        };
        header_accesses += paths.filter(|p| is_header(p)).count() as u32;
    }

    Body {
        function,
        params: data.num_parameters(),
        returns: data.return_type.arity > 0,
        header_accesses,
        own_stores,
    }
}
