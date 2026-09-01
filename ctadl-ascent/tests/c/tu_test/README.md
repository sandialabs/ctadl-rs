# tu_test -- does importing one translation unit at a time lose taint?

`read_c_source` imports a directory as **one buffer**: every `.h` and `.c` under it,
concatenated, parsed once. The justification was whole-program linking for free -- a call
in `a.c` to `g` defined in `b.c` resolves to `g`'s body because one parse holds both. This
directory tests whether that is actually needed, by importing the same two translation
units two ways and querying the same source/sink model against each:

* **big**: `ctadl import -l c tu_test/` -- `a.c` and `b.c` concatenated.
* **per-TU**: `ctadl import -l c a.c`, `ctadl import -l c b.c` -- two imports, then
  `ctadl index tu tu_a tu_b` co-indexes them as one project.

Each `.c` is written as its *preprocessed* form would be: the prototypes a header would
have supplied are inlined at the top, so each file is a complete translation unit and
never sees the other's definitions. `run.sh` does both imports and prints a table.

## The cases

One sink name per case, so a SARIF result's `sinkFunctions` names the case it came from.

| case | what crosses the TU boundary |
|---|---|
| `sink_intra` | nothing -- baseline, must be found both ways |
| `sink_forward` | a tainted **argument**, into `g` (b.c), whose body has the sink |
| `sink_return` | a **return value**, out of `h` (b.c), to a sink in a.c |
| `sink_fp` | a **function-pointer binding**: `int (*fp)(int) = h;` where `h` is defined only in b.c |
| `sink_global` | a **store to a file-scope object**, `shared`, written in a.c and read in b.c |
| `sink_reverse` | an argument in the other direction: b.c calls `k`, defined in a.c |

## Result

```
case          big buffer    per-TU
sink_intra    found         found
sink_forward  found         found
sink_return   found         found
sink_fp       found         -
sink_global   found         found
sink_reverse  found         found
DIFFERENT
```

Every **call**-carried flow is found both ways: the index keys functions by name across
co-indexed imports, so the bodyless `g` that `define_extern_functions` invents in `tu_a`
and the real `g` lowered in `tu_b` are one function, and taint crosses. The concatenation
buys nothing for direct calls, returns, or globals. The back end already does the linking.

The one divergence is the **function-pointer binding**, and it is a frontend lowering
decision, not a linking one. The IR of `case_fp` in each mode:

```
big:     assign %fp = ptr<h>            -- a function reference; the indirect call resolves
per-TU:  %<t0> = load $globals.h        -- a variable read; there is nothing to resolve
         assign %fp = %<t0>
```

`flatten_expr` lowers a bare identifier as a function reference only if the name is in
`Context::functions`, i.e. *defined* in the buffer. In the lone TU `h` is only declared,
so `h` is read as a global variable and the binding is lost. The prototype is evidence
enough that `h` is a function -- the same evidence `cast_shaped_call` already uses via
`Context::declared_functions` -- so the candidate fix is to consult that set here too.

## With the candidate fix

`flatten_expr` now also accepts a name in `Context::declared_functions` as a function
reference (one line, the commit after the one that added this directory):

```
case          big buffer    per-TU
sink_intra    found         found
sink_forward  found         found
sink_return   found         found
sink_fp       found         found
sink_global   found         found
sink_reverse  found         found
SAME
```

So, for these shapes, the concatenated buffer is not what makes cross-TU taint work. A
per-TU import model needs the frontend to know one thing a preprocessed TU always tells
it -- which names are functions -- and the back end does the rest.
