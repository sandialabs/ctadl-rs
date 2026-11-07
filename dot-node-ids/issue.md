# DOT node ids can collide, silently merging distinct vertices

`IndexGraphViz::node_id` (`ctadl-ascent/src/graphviz.rs:246`) builds a node's DOT identifier by
taking the node's label and replacing every non-alphanumeric character with `_`. The label is
`function(<name>)\n<variable><access-path>`, and the access path is what distinguishes one vertex
from another inside the same function, so `local(t).x` becomes `local_t__x` — and so does
`local(t)_x`, and so does any other vertex whose label differs only in punctuation. Graphviz keys
nodes by id, so two vertices that collide are drawn as one node with one set of incident edges: the
picture claims a data-flow connection that the index does not contain. The node *set* is built
correctly (a `BTreeSet` over the `(FunctionId, FlowVariable, Path)` triple in `render_index_graph`),
so this is purely a rendering-layer defect — the fix is to generate ids from something injective,
such as the function id plus a counter into the sorted node vector, and leave the human-readable
text in `node_label` where it belongs. The same `node_id` pattern appears in the taint-graph
renderer at `ctadl-ascent/src/graphviz.rs:107` and should be fixed with it. This predates the move
of `--dump-index-graph` to `inspect` and is not caused by it, but that change makes the index graph
much easier to produce, so more people will hit it.
