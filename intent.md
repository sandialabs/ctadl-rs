# Intent: Pull a small relation out of a large self-join

## Problem

For code like

```text
object = Global.session
object.address = input
```

the function summary must record that `input` reaches `Global.session.address`. The current rule finds this by joining `locals` with itself and then filtering for rows whose base was loaded from the global heap. Most `locals` rows are irrelevant to that question, yet they are scanned repeatedly. On one large application the early baseline had 16.7 million local-flow rows, against a much smaller set of global bases.

## Change

Derive a filtered relation first:

```text
global_base(function, variable, global_parameter, global_path)
```

A row belongs there when a whole local variable comes from a nonempty path of the global parameter. The summary rule starts from `global_base` and probes `locals` by function and variable to find writes through the base, then combines the global path and stored path using the existing path rules. `global_base` must come first in the join, because an equivalent-looking rule can otherwise generate a full scan.

## Constraints

- Keep the original aliasing switch, path filters, store checks and result tuple.
- Results must not change. Any change to which stores are summarized is out of scope and needs its own correctness review. Use `index-diff` as a gate that must pass.
