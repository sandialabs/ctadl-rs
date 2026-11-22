# What makes a good frontend

A frontend translates a real language into CTADL's ir (docs/ir.md). We refer to the input as the source language; the target language is CTADL IR.

A typical frontend has the make the following design decisions:

- How to parse the language.
- How to represent the space of functions or methods.
- How to generate IR functions from the constructs in the source language.
- How to translate individual instructions into data flow IR instructions.
- How to emit function calls so as to preserve data flow.

Dex, for instance:

- The language is parsed as bytecode using our `dex-reader` crate.
- The methods are represented as in the Dex language, where a virtual method table understands that methods live inside classes, that they have signatures, a simple name, and exist inside a classe hierarchy that can override them according to an algorithm.
- One function is generated per method; code whose method structure is typically implicit, like static initializers and constructor `.<init>` methods are translated uniformly into functions.
- All instructions parsed are evaluated as data flow, sometimes directly (just looking at the instruction), sometimes in concert with other things (e.g., exceptions are modeled by adding an extra return value, the exceptional return value, to functions, and all such returns have to know about this exceptional slot for the function to be well-formed).
- Functions are called with a uniform naming scheme so that method invocation resolves, at analysis time, to the appropriate functions. Given the dynamic nature of virtual calls, the analysis can't resolve every virtual call perfectly.

Thes document covers principles for writing good frontends that any new frontends should follow, and that existing frontends ought to follow.

Language frontends should avoid introducing outside-of-rust dependencies where possible. Pcode is a notable exception of this rule because it depends, at runtime, on Ghidra.

All functions exist in a global namespace. A frontend should come up with a naming scheme such that distinct procedures are named uniquely and translated to distinct functions. For instance, in a hypothetical frontend forPython source files, a reasonable choice is a unique name composed of the concatenation of:

- the relative path of the python module;
- the class in which a definition occurs (with nesting, if it's nested);
- the line at which the definition occurs;
- the name of the function, then a slash, then its arity;
-  ??
