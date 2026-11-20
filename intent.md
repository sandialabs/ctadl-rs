# Intent: a per-result SARIF diff

Comparing two `ctadl query` outputs is as common as comparing two index runs,
and every way of doing it today is wrong. `diff` and `jq` compare arrays in
the order they were written, and compare index references
(`logicalLocations[].index`) that are numbered per
run. Microsoft's `sarif diff` compares counts per rule, not results: a result
that moves is reported as no change, and reordering a file can report a new
issue.

The goal is to alert when the results are different.

Ship `ctadl diff-query A B`, which compares the results of two ctadl SARIF files by content: references replaced by what they name,
per-run ids dropped, order ignored where SARIF gives it no meaning (results,
locations, code flows) and kept where it does (the steps of a thread flow).

One flag `--endpoints-only` will ignore the actual steps of a threadFlow, showing no difference as long as it has the same multiset of source-sink findings: two flows with the same endpoints count twice.

The comparison is `multiset-diff`, proved with Verus. The risk is
canonicalizing results before it: a mistake there makes the proved merge
report a wrong answer. Keep canonicalization small and separate, and check it
with metamorphic tests (shuffled or renumbered input reports nothing; a
deleted result or swapped steps are reported).

Message text is ignored.