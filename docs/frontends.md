# What makes a good frontend

A frontend translates a real language into CTADL IR ([ir.md](ir.md)). We refer to the input as the source language; the target language is CTADL IR.

A typical frontend has to make the following design decisions:

- How to parse the language.
- How to represent the space of functions or methods.
- How to generate IR functions from the constructs in the source language.
- How to translate individual instructions into data flow IR instructions.
- How to emit function calls so as to preserve data flow.

Dex, for instance:

- The language is parsed as bytecode using our `dex-reader` crate.
- The methods are represented as in the Dex language, where a virtual method table understands that methods live inside classes, that they have signatures, a simple name, and exist inside a class hierarchy that can override them according to an algorithm.
- One function is generated per method; code whose method structure is typically implicit, like static initializers and constructor `.<init>` methods are translated uniformly into functions.
- All instructions parsed are evaluated as data flow, sometimes directly (just looking at the instruction), sometimes in concert with other things (e.g., exceptions are modeled by adding an extra return value, the exceptional return value, to functions, and all such returns have to know about this exceptional slot for the function to be well-formed).
- Functions are called with a uniform naming scheme so that method invocation resolves, at analysis time, to the appropriate functions. Given the dynamic nature of virtual calls, the analysis can't resolve every virtual call perfectly.

This document covers principles for writing good frontends that any new frontends should follow, and that existing frontends ought to follow. It doesn't cover the constraints of the IR or how data structures should be defined; just look at the code for that.

## Avoid external dependencies

Language frontends should avoid introducing non-Rust dependencies where possible. Pcode is a notable exception to this rule because it depends, at runtime, on Ghidra.

## Name every function uniquely

All functions exist in a global namespace. A frontend should implement a naming scheme such that distinct procedures are named uniquely and translated to distinct functions. For instance, in a hypothetical frontend for Python source files, a reasonable choice is a unique name composed of the concatenation, separated by `:`, of:

- the relative path of the python module;
- the class in which a definition occurs (with nesting, if it's nested);
- the line at which the definition occurs;
- the column at which the definition occurs;
- the name of the function, then a slash, then its arity.

When the input defines one name twice, such as a class in two jar entries or a `static` helper in two C files, decide deliberately whether the definitions are the same function or different ones. Never let them silently merge into one.

## Declare every function a program calls

Programs are open: they call library and platform code the input doesn't define. A frontend should declare a body-less function for each such callee, with the right parameters and return arity, so the call still resolves.

## Translate deterministically

The same input should produce the same IR, regardless of thread count or hash order. A frontend that translates in parallel should number functions, globals and source locations in program order, and sort whatever it collects in a hash table before numbering it. Otherwise indexes and diffs of the same input aren't comparable.

## Trace every statement to its source

Every statement and global should carry its location in the input, as a byte range in the file it came from. This is the express purpose of the `source-info` crate; *all* frontends must use it. Inside an archive, the location names the entry, such as a dex file in an APK. A finding is useful only if it can be traced back to the code.

## Emit simple IR

Translate each construct into the obvious statements, keeping every copy and temporary. The shared pipeline in `ctadl index` does SSA and copy cleanup for every frontend; doing it again in one frontend duplicates work and is harder to check. Keep any precision tricks separate from the conventions.

## Make each convention once, and write it down

Every choice the IR doesn't force, such as how fields, globals or exceptions are spelled, is a convention. Make each one in one place in the code, and list it in the crate docs, as S19 of [ir.md](ir.md) requires.

## Translate what you can, and leave out what's costly

A translation needn't be complete or exact: the analysis is already unsound, and real programs are messy. A frontend should translate what it reasonably can, and leave out what would cost far more to model than it's worth.

## Record what you leave out

Leaving something out is fine; leaving it out silently is not. Report each kind of gap once, coarsely, in ctadl's notifications log (`notifications.sarif`; see `ctadl-import/src/notifications.rs`): one notification with a count, not one per site, and never the same one twice. A file with many parse errors gets one warning, not a ton. Per-site detail belongs in the debug log.

## Never fail hard

A frontend should not abort on bad input. When it can't make a decision, or ends up in an inconsistent state, it should record the problem (see above) and keep what it has translated so far. An error in one place must not cost independent results: a parse error at the end of a function still yields that function, with the data flow translated before the error. A frontend may offer a debug switch, such as an environment variable, that turns these warnings into errors for hunting frontend gaps; it must be off by default.

# The principles in practice

Dex is the most mature frontend, so it is the example wherever it meets a principle.

- **Avoid external dependencies.** Dex, JVM and CIL read their formats with our own `dex-reader`, `jvm-reader` and `cil-reader` crates; C and Lua parse with tree-sitter's Rust bindings.
- **Name every function uniquely.** Dex names each method by its full signature, `Lcls;->name(desc)`. When a jar defines a class twice, JVM keeps the copy every runtime loads and warns about the rest; C lowers definitions with identical text once and renames the others after their file (`g$util.c`).
- **Declare every function a program calls.** Dex records each method a body calls and gives each one no class defines a body-less function, with parameters from its descriptor and the two-value return.
- **Translate deterministically.** Dex lowers method bodies in parallel, then interns their spans and globals in program order, and sorts its callees before giving them functions.
- **Trace every statement to its source.** Dex tags each statement with its instruction's offset in the dex file, names the dex file inside the APK, and places each class's global at its class definition.
- **Emit simple IR.** Dex lowers each instruction to the statements its data flow implies, and keeps the splitting of R8-merged classes, a precision trick, in its own module.
- **Make each convention once, and write it down.** Dex's crate docs list its conventions for fields, globals, arrays, method handles and exceptions, and each field spelling is made by one function.
- **Translate what you can, and leave out what's costly.** Dex calls an `invoke-custom` site with an unknown target instead of modeling bootstrap methods.
- **Record what you leave out.** Dex reports its splitting of R8-merged classes as one notification, with the counts as properties.
- **Never fail hard.** C keeps lowering past a construct tree-sitter can't parse, and `CTADL_ERROR_ON_AST` turns those reports into errors.
