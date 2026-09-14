# Intent - use CHA with expensive, surgical fallback algorithm - DO-NOT-MERGE

Change the bulk of the call graph to use CHA, but intelligently handle the small percentage of the
call graph using other techniques: either skip the analysis (e.g., does `.equals()` matter for data
flow) or use hybrid inlining but ONLY on small parts of the graph.

There is an initial viability analysis in `cha-viability.md`.
