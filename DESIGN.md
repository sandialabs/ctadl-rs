# Model matching DSL - DO-NOT-MERGE


## DSL for describing sets of methods

This DSL looks like a Datalog language. It has built-in relations with defined meanings. It has
recursive rule evaluation.

Built-in input relations:

- `fun(id)` - `id` is the fully qualified id of a function, same name as FunctionData
- `name(id, name)` - `name` is the name of the associate function
- `parent(id, parent)` - the parent class
- `signature(id, sig)` - the signature of the function
- `subclass(sub, super)` - the sub and superclass
- `has_code(id)` - whether function has code or not

It has operators that can be used in atom position but aren't true relations:

- `regex_match(str, pattern)` - regex match
- `x < y` and others - numeric comparison
- `&&` `||` Boolean combination of Boolean-typed things

Output relations:

- `source_fun(id, index, path)`
- `sink_fun(id, index, path)`
- `propagation(id, dst_index, dst_path, src_index, src_path)`
- `bridge(dst_id, dst_index, dst_path, src_id, src_index, src_path)`

# Engine

These rules compile to a graph that we can write a single Datalog shape for. With each graph node,
associate a number, operator, and fan-in. Evaluate by populating each built in relation with the
associated information across the entire program.
