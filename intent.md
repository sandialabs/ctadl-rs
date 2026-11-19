# Intent: a per-result SARIF diff

Comparing two `ctadl query` outputs is as common as comparing two index runs,
and every way of doing it today is wrong. `diff` and `jq` compare arrays in
the order they were written, and compare index references
(`logicalLocations[].index`, `artifactLocation.index`) that are numbered per
run. Microsoft's `sarif diff` compares counts per rule, not results: a result
that moves is reported as no change, and reordering a file can report a new
issue.

Ship `ctadl diff-sarif A B`, which compares the results of two SARIF files as
multisets, matched by content: references replaced by what they name,
per-run ids dropped, order ignored where SARIF gives it no meaning (results,
locations, code flows) and kept where it does (the steps of a thread flow).
Like `ctadl diff-index`, it uses `multiset-diff` and writes output meant for
an agent.

Open questions:

- Only ctadl's SARIF, or any tool's?
- Does message text count? The `machine` profile truncates it.
- Refuse two different `--sarif-profile`s, or compare what both emit?
