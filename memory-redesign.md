# Memory in the IR: named states, so summaries run from entry to exit

**Status:** proposal, as a change to `origin/main`. Nothing here is implemented. A prototype that
orders memory below formals outside the IR, the memory-order pass, is on the branch
`problem/memory-order` (`--memory-order`, codegen side tables plus extra index relations). This
design takes what the prototype learned into the IR, and in time replaces `Update`'s use for
memory and codegen's `cap_path`. The last section, "What it does for problem.md", covers the
blowups that motivated it.

The analysis computes function summaries: a relation from what a function sees on entry to what
it leaves on exit, instantiated at every call site. It should stay what it is, reachability over
vertices in Datalog. This proposal changes what the vertices mean, so that the entry state, the
exit state, and memory between two statements each have a vertex of their own.

## How memory works today

The pipeline has four layers, and each makes its own choice about memory: the IR
(`ctadl-ir`), codegen, which turns the IR into facts (`ctadl-ascent/src/codegen`), the index rules,
which compute `locals` and `summary` from the facts (`ctadl-index/src/index_engine`), and the query
engine, which searches the resulting graph (`ctadl-ascent/src/query_engine`).

**The IR versions variables; memory gets versions only when a frontend writes it with `Update`.**
- **Variables:** SSA (`ctadl-ir/src/ssa`) gives each variable a version per definition, so a use
  names the definition that reaches it. A parameter's unversioned name `@pN` is its value on
  entry, and the entry copy `@pN_0 = @pN` starts its versions.
- **Memory through `Store`:** `Load` and `Store` take an access path and a field, and a `Store`
  defines nothing. No operand names "the memory at `h.[8].deref` after this store".
- **Memory through `Update`:** `x = update (y.f := v)` writes one field like a `Store`, but it
  defines `x`, so SSA gives the written aggregate a new version. A frontend that emits `Update`
  for heap writes gets memory versioned this way: `$globals = update ($globals.[A].deref := v)`
  makes each write a new version of the global heap, so a later load sees the write and an
  earlier one does not. Today the p-code frontend writes the stack frame this way
  (`__stack_top = update (__stack_top.[k].deref := v)`; mem2reg promotes some slots to locals,
  and the frame pass can turn the rest back into stores), and flowy (`frontends/ctadl-flowy`)
  has `update` syntax; the `$globals` experiment in `problem.md` rewrote heap stores into it.
  Each version is of the whole variable, so it carries everything below it. Whether a
  version reaches the caller is up to `param-flow` (see "`param-flow` drifted").
- **Calls:** `CallAssign { rets, args }` has one operand per argument, the variable version
  passed. It defines only `rets`, so nothing names an argument's memory after the call.
- **Exit:** `param-flow`, before the `return`, lists each parameter at version 0, and `$globals`,
  which SSA renames to its last version. Nothing names the memory below a parameter at exit.

**Codegen makes one vertex per variable version and path.** A vertex is a `(variable, path)`
pair. `store x.[k].deref := v` becomes an edge into `(x, .[k].deref)` and `load` an edge out of
it, wherever the two are. `x = update (y.f := v)` becomes a copy of `y` into `x` except at `.f`
and below (`update_sans_path`), plus the write `x.f <- v`. The new version holds `v` at `.f` and
not what earlier writes put there. It still holds what `y` held as a whole: a flow into `y` at
the empty path carries every path, `.f` included. After `$globals_1 = update ($globals_0.[A].deref
:= v)`, a load of `$globals_1.[A].deref` sees `v` and `GLOBALS.[A].deref` from entry. A parameter's entry name and its `param-flow` write-back are the same
vertex, the formal `@pN`: the entry copy is an edge `@pN_0 <- @pN`, and `param-flow` an edge
`@pN <- @pN_0`. A call becomes one `actual_param(site, n, vertex)` fact per argument.

**The index rules close over each function's edges without order.** `locals` is reachability
from the formals over every edge in the function, so for memory written with `Store`:
- **every load sees every store** to the same vertex, before or after it, on any path;
- **every store is weak:** nothing is ever overwritten;
- **a cell has as many vertices as it has names.** `h->head` reached through `h`, through a copy
  of `h`, and through a pointer loaded from `h` are different vertices, connected only where an
  edge or a rule happens to connect them.

The formal `@pN` seeds `locals` at the empty path, and `summary` reads what reaches `@pN` at path
`p1` from formal `N2` at `p2`. So one vertex is the entry (the seed) and the exit (what the summary
reads).

**At a call the rules make one vertex per argument.** From each `actual_param` fact the rules make
a call-argument vertex (`call_arg`) and edges both ways between it and the actual. The callee's
summary is instantiated as edges from that vertex to itself: a row `0.head <- 1` becomes
`cv0.head <- cv1`. So the call's write lands on the vertex its reads come from, and a callee's
write through an argument feeds the same call's read through it. The `$globals` argument is not
even a call-argument vertex: it is the caller's own `$globals` formal.

**Several mechanisms each recover a piece of that.** Nothing in the pipeline can say "`h->head` as
of this statement", the way an SSA version says "`p` as of this definition". Each mechanism below
approximates that for one case:

| Mechanism | Layer | Stands in for | Where it stops |
|---|---|---|---|
| one vertex per `(var, path)` | codegen | memory at every point at once | a load sees a store that follows it |
| `param-flow` (`StatementKind::ParamFlow`) | IR | a parameter's state at exit | the formal is also the entry; see below |
| `Update` | IR | one field's write, by versioning the whole aggregate | each version copies the whole subtree |
| mem2reg, frame `Update`s | IR | stack slots, as variables | stack only; an escaped slot keeps every version's contents |
| `cap_path` | codegen | which object a load chain names | per block, so a write through a loaded pointer is lost across blocks |
| `alias_of_formal` and the alias rule | rules | a store through a copy of a formal | copies only, not loads |

**The common shape is in/out conflation.** One vertex is both the state before something and the
state after it:
- **in a body,** a location's vertex before and after a store. The IR has no memory versions,
  and codegen makes one vertex per path.
- **at a call,** the argument before and after the callee. The IR has one operand, codegen one
  fact, and the rules one vertex, which the callee's summary both reads and writes.
- **at a formal,** the entry and the exit. The IR's `@pN` is both the entry copy's source and
  what `param-flow` writes, and codegen keeps them one vertex.

**`param-flow` drifted.** It was put there as an anchor that picks up a parameter's most recent
version, so summaries could read the exit state. `f65bb6c4` (#154) made it write back version 0
always, so that a rebinding (`p = q`) stays local to the callee. That conflates two things a new
version of a parameter can mean:
- **rebinding,** where the caller should see the entry binding (version 0);
- **new contents of the same object,** where the caller should see the latest version.

Under `Store` the two coincide at version 0, because stores never make versions, so nothing breaks
today. An `Update` on a parameter would lose its write silently. Writing back the latest version
instead runs into the formal-as-entry loop: the entry copy `@p0_0 = @p0` reads the vertex that
`param-flow` writes, so the exit state flows back into the entry. `$globals` has the same loop
through `GLOBALS <- $globals_n` and `$globals_0 <- GLOBALS`.

## What it costs

**Lost order is a precision loss that callers pay for.** A spurious row in a summary is not one
row. It is instantiated at every call site, composes with the caller's own flows into the
caller's summary, and goes around every call-graph cycle it is on. Most of the rows in a native
index are summary instances (95-99% of `assign_like` in the binaries checked), so a small summary
error can be most of an index.

**Patterns that order would fix:**
- **Read before write.** A push onto an intrusive list, `t = h->head; n->next = t; h->head = n`,
  gives `h.head <- n` and `n.next <- h.head`, which compose to `n.next <- n`: the node points to
  itself. Free lists, open-file lists, child lists, parser and scope stacks, and GC gray lists all
  do this. It is usually harmless, and expensive when the linked field is one the program reads a
  lot.
- **Write through one name, read through another.** A callee writes through a pointer loaded
  from a formal, and the caller reads the same cell later through its own load chain. Today the
  two are different vertices, so the flow is missed. That is unsound, not imprecise.

**Patterns that kills would fix** (strong updates; not order alone):
- **Save and restore,** `old = g->cur; g->cur = x; ...; g->cur = old`, leaves `g.cur <- x` in the
  summary.
- **Initialize, then overwrite,** where the first value is a placeholder.

**What it does not fix:** flows merged by arithmetic, offsets multiplied by loops (each variant
of a path is still its own cell), and summaries that are large because the function really moves
that much data.

## The design

### The IR

1. **Formals are immutable.** A rebinding of a parameter (`p = q`) defines a fresh local, and later
   uses read it. A new version of a parameter can then only mean "same object", which removes
   #154's problem at its source.

2. **Memory below a root is a set of cells,** each named by a root and an access path: `(h, .head)`,
   or `(p, .[16].deref.[36].deref)`.
   - **Roots:** formals, `$globals`, allocation results and call results.
   - **Canonical names:** a cell is named function-wide through copies, address arithmetic and load
     chains, so `h->head` through `h`, through a copy of `h`, or through a pointer loaded from `h`
     is one cell. The prototype's `canonicalize` does this
     (`ctadl-ascent/src/codegen/memory_order.rs` on `problem/memory-order`).
   - **Cell SSA:** a store defines a new version of its cell, a load uses the version that reaches
     it, and joins get phis. This is mem2reg, generalized from stack slots to rooted paths.

3. **Entry and exit are explicit.**
   - **Entry:** version 0 of a cell is an entry definition: its value is formal `j` at path `π` on
     entry.
   - **Exit:** `return` takes the final version of every cell below a formal or `$globals` as
     operands. Those operands are the anchor; `param-flow` goes away.

4. **Calls are memory effects.** A call that passes root `r` takes the current version of each cell
   below `r` as an input. It defines a new version of each as an output, which is the input plus
   whatever the callee writes there (weak). The call has its own input and output, so its write
   cannot feed its own read.

A frontend keeps emitting `Load` and `Store`. Cells are a pass over the IR after SSA, as mem2reg
is, so no frontend changes except where a frontend relies on rebinding a parameter.

### The analysis

Cell versions are ordinary variables, so loads, stores and phis become ordinary edges. Only three
things change:
- **summary inputs** are the entry definitions, which are vertices of their own;
- **summary outputs** are what reaches the exit operands;
- **call instantiation** connects a call's input versions to the callee's formal paths at entry,
  and the callee's exit operands to the call's output versions.

Entry, exit, a call's input and a call's output are always different vertices, so the in/out
conflation cannot occur. The prototype needed four extra relations and a second vertex per call
argument to get the same effect from outside the IR (`assign_sans`, `sans_rest`,
`actual_in_sans`, and the `CallArgId` out bit). Queries see cell versions as variables, and a result reported at a
cell version is reported at the location it names.

### The example

```
push(@p0, @p1):                        // void push(list *h, node *n) { n->next = h->head; h->head = n; }
  h.head#0 = entry(0, .head)
  n.next#0 = entry(1, .next)
  t        = h.head#0                  // the load uses the reaching version
  n.next#1 = t                         // a store defines a version
  h.head#1 = @p1
  return [h.head#1, n.next#1]          // the exit operands
// summary: 0.head <- 1, 1.next <- 0.head.  h.head#1 never reaches t, so no 1.next <- 1.

caller(@p0, @p1):                      // void caller(list *h, node *n) { push(h, n); }
  h.head#0 = entry(0, .head)
  n.next#0 = entry(1, .next)
  h.head#1, n.next#1 = call push(@p0 {h.head#0}, @p1 {n.next#0})
  return [h.head#1, n.next#1]
// summary: the same rows; the call reads #0 and writes #1.
```

Pushing twice gives `h.head#2` from the second call. The second call reads `h.head#1`, which holds
`n`, so `1.next <- 1` is derived, as it should be. A push in a loop gets a phi at the loop head
that merges the back edge, with the same result.

With strong updates, save and restore works the same way:

```
  old     = g.cur#0
  g.cur#1 = x
  ...
  g.cur#2 = old
  return [g.cur#2]                     // summary: 0.cur <- 0.cur only; x does not escape
```

## Policies the IR has to state

These are decided implicitly today by which vertices happen to be shared. The redesign makes each
one explicit.

- **Aliasing within a function.** Different roots do not alias. This is the engine's behavior today.
  Aliasing created by a caller is handled at the call: passing the same root twice makes that
  call's cells below it weak (one input and output shared by both arguments).
- **Offsets.** A cell's path has constant offsets. An address argument (`p.[k]` passed to a call),
  or a non-constant offset, makes the cells below that root weak: one flow-insensitive cell that
  every access reads and writes, as today's vertex does. The prototype's poisoning is this rule.
- **Unknown writes.** A call whose callee's writes are unknown (no body, a skipped body, an
  indirect or virtual call) makes the cells strictly below each argument it is passed weak. A
  callee handed a pointer can write what it points to, never the slot the pointer was loaded from.
  Poisoning the whole root instead is much coarser: one unknown call anywhere below a root leaves
  nothing below it ordered.
- **What a callee writes.** A call either defines every cell below an argument (output = input ∪
  callee writes, sound and simple), or only the cells the callee may write. Bottom-up write sets
  over direct calls (the prototype's `WriteSets`) make the second available before the
  fixpoint.
- **What a call writes back.** A call writes back only cells below an argument, never the
  argument's own address. An address argument (`&s->f`) names a location; it is not one.
- **Unbounded paths.** A loop that walks a list (`p = p->next`) and recursion that passes
  `p->next` make paths without end. Past a depth bound, a cell becomes weak. Offset variants from
  a loop-incremented pointer need a bound for a similar reason, so the two should share one
  widening policy.
- **Strong updates.** A store to a cell whose root names one object kills the previous version,
  entry value included. `Update` kills only what earlier writes put at the field, not what the
  variable held on entry. It comes free in this representation, and it is a separate precision
  gain from ordering.

## Frontends

- **C and p-code** put objects behind offsets and `.deref`. Cells are offset paths, and most of the
  policies above (offsets, address arguments, unknown writes) are about them.
- **Java, Dalvik and CIL** put objects behind fields, so cells are field paths and offsets do not
  arise. Virtual calls make writes unknown at most sites until resolution, so less order survives.
  How much is unmeasured. CIL's value types (structs copied by value, `docs/limitations.md`) want
  a copied root, not a shared one.
- **Lua** (the language) puts objects behind table keys. A key that is not a constant makes the
  table's cells weak.
- **`$globals`** is a root every call reaches. Giving each call an input and output for every
  global cell is the cost the `$globals`-as-`Update` experiment measured. It probably wants weak
  global cells by default, with only the cells a function both reads and writes ordered.

## Cost

- **Cells, not whole variables.** A store copies one value. `Update` copies the aggregate per
  version, and an escaped stack frame under mem2reg copies every kept slot per write. The
  prototype is the evidence that cells are cheap: its writer variables held 81 K rows in the
  largest function measured, against 119 M for the function.
- **Calls with many cells below an argument** need one input and one output per cell. A shared
  edge per argument, with the cells' paths left out of it, kept that cheap in the prototype. In
  the IR the same thing is one call operand per cell. Weak cells share one operand.
- **Fewer vertices per cell.** A cell with three names today is three vertices; canonical naming
  makes it one, plus a version per write.
- **More flows where names are unified.** A cell reached by two names now connects a write under
  one to a read under the other. Those are flows the old engine missed, and they can add rows.

## Plan

1. **Formals immutable, `return` carries the exit state, `param-flow` removed.** Semantics only,
   and independent of the rest.
   - **SSA:** renames a rebinding of a parameter to a fresh local.
   - **Exit:** `return` takes each parameter's final version, and the summary reads that.
   - **Entry:** the entry copy reads only the formal's entry seed, never the exit.
   - **Tests:** a versioned global read before a write sees the old value (the test
     `a_versioned_global_read_before_the_write_sees_the_old_value`, from the `$globals`
     experiment's patch, not in the tree). Keep `param_passing.tnt` passing: a rebinding still must
     not reach the caller. Fix C's out parameters in the frontend (`store out.deref := x`; #154
     left `C:outparam` failing).
2. **Cells in the IR.**
   - **Pass:** canonicalization, cell SSA with phis, entry definitions, call inputs and outputs, and
     the policies above.
   - **Analysis:** reads the new operands.
   - **Tests:** the prototype's `frontends/ctadl-c/src/memory_order_tests.rs` (on
     `problem/memory-order`) carries over unchanged, plus tests for each policy above and the Java
     and Lua frontends.
3. **Measure, then make it the default.** The firmware sweep, findings compared by rule, message
   and location, plus the regression suite and the apps.
4. **Strong updates,** then retire `Update` for memory, `cap_path`, and the alias rule's copy case.
   Each becomes a special case of cells.

## Ruled out

- **A flow-sensitive engine,** with program points in `locals`: it multiplies the largest relation
  by the number of points.
- **A single memory chain with clobber walks** (LLVM's MemorySSA): each load walks back over every
  definition, which is expensive as Datalog. The per-path sparse form gives the same answers for
  memory below roots.
- **More `Update`:** versioning a whole variable for one field's order copies the whole subtree per
  version.
- **Fixing `param-flow` alone:** it would make `Update` usable on parameters, but leaves the body's
  conflation, where a load sees a later store.
- **A rule against self-links:** unsound for real self-referencing structures, and the link comes
  back from the two rows that compose to it anyway.

## Open questions

- **Canonicalization's reach.** Unifying a load chain's names is what makes a write through a
  loaded pointer visible to a later read. It can also connect two names that the program never
  uses for the same object at the same time. Is a limit needed, and what is it?
- **Roots that are not formals.** Allocation and call results have no entry state. Should their
  cells start empty, or start from the defining call's output?
- **How much order survives** in Java, CIL and Lua, where most calls are virtual.
- **Context.** Hybrid inlining's conditional summaries carry a decision per row. Cell versions
  multiply the vertices a condition sits on; whether that matters is unmeasured.

## What it does for problem.md

`problem.md`, on the branch `problem/native-blowups`, lists native blowups by cause. The
prototype's measurements (cause 5, "The structural fix", checkpoints 0-4) are in its copy on
`problem/memory-order`. The redesign is aimed at cause 5, and touches four
others.

**Cause 5 (Lua's GC, `evLua` and `ipsWeb`, 21-22x).** `reallymarkobject(g, o)` pushes `o` onto
`g->gray` (`g->gray = o; o->gclist = g->gray`), and `atomic` pushes the running thread `L`. The
flow-insensitive heap makes `L->top`, Lua's stack pointer, an alias of `L`, and cause 6's offsets
then pile up on it through Lua's 73-function call-graph SCC. It is the read-before-write pattern
above.
- **Both conflations had to go.** Ordering loads alone changed nothing (checkpoint 2). Ordering
  calls too (checkpoint 3) took `evLua` to 1.42x and `ipsWeb` to 1.16x, with findings identical on
  all 35 sweep binaries (checkpoint 4).
- **The unknown-write policy came from it.** Poisoning whole roots left `atomic` with nothing
  ordered, because `g` is `L->l_G` and `propagateall` reaches an indirect call (checkpoint 3).
- **The redesign moves that fix into the IR** and adds the formal's entry/exit split, which the
  prototype did not need because it never versions a formal.

**`nginx` (3.07x to 3.67x under the prototype).** PCRE's `match()` gains sources
`1.([32].deref)^k.[28].deref`, which cause 6 multiplies by its offsets. They come from canonical
naming (a cell under two names becomes one), and may be real flows the old engine missed. This is
the first open question above, and it should be settled before step 3.

**Cause 6 (offsets on a loop-incremented pointer).** Not fixed: each offset variant is its own
cell. The widening bound in the policies should be the one cause 6 needs.

**Cause 9 (a call writes back onto an address argument).** The write-back policy (cells below an
argument, never the address) is the change that took `a2050` from 1.72x to 1.29x in cause 9's
patch.

**Cause 11 (escaped stack slots).** `--mem2reg above` costs 6x `locals` because each frame version
carries every kept slot. Kept slots as cells, versioned apart, is cause 11's own suggestion.

**`$globals` (causes 2 and 7, "Write `$globals` with `update`").** That experiment hit both of the
redesign's motivations: each version copied the whole heap (2-6x `locals`), and the
`GLOBALS`/`$globals_0` loop let a read see a later write. Step 1 removes the loop; weak global
cells by default avoid the copies. Cause 7's cross product into globals is a real flow, and the
redesign does not shrink it.

Scripts and outputs: `/Volumes/Shampoo/nb-work/c5/` (`scripts/idx.sh`, `scripts/findings.py`).
