# Redesign IR with solid semantics and simplicity

- The IR is a translation target for *real languages* (which do arbitrary things)
- The IR is the source of a flow-insensitive, *unsound* static taint analysis. It needs to have
  solid semantics so that passes on the IR can obey some sort of correctness/abstraction criterion.
  Also, so that our analysis can approximate appropriately
- It is a data flow language whose semantics is labeling. Beginning with an initial labeling, the
  semantics says how labels propagate across various IR constructs.
- Offsets are inspired by offsets into data structures (in binaries), like how C lays out a struct. Offsets are *always* constants.
- Fields are symbolic field names and do not have any layout characteristics.
- There is no [*], no arbitrary offset or field.

Semantics of instructions:

- assignment: `y.d = x.s`. `labels(x.s)` propagates to `y.d`.
  - If `s` is an address, then `labels(x.s) = labels(x)`
  - For every suffix `t` such that `x.s.t` is a location, `y.d.t` gets `labels(x.s.t)`
  - This makes assigning *from* an address allowed and defined and propagate labels
- assignment to an address is allowed: `p.[k] = e`
  - Note that `p.[k] = e` is equivalent to `p = e.[-k]`
- `labels(p.[k]) = labels(p)`. If `q` was attacker-controlled and `p = q.[8]` then `p` is also
  attacker-controlled.


REVISE THESE IF NECESSARY:
- Cells. Every access path x.s1...sn names a cell. A cell carries a set of labels and has a sub-cell for every access path segment.
- Equations. These are what separate offsets from fields:
  - Offsets add: p.[i].[j] = p.[i+j].
  - A zero offset does nothing: p.[0] = p.
  - Fields obey no equations, so p.f ≠ p and p.f.g ≠ p.g.f.
  - deref is an ordinary field. It means "memory" only because frontends agree to use it that way.
  - #value is terminal and absorbs itself, as today.
