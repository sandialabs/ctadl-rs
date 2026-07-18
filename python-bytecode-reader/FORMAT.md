# Stable Python-bytecode text format

This is the version-independent boundary between Python and CTADL. The Python
tool `bytecode_text` (embedded in this crate, staged at run time) compiles source
(or reads `.pyc`), normalizes each `dis.Instruction` into a version-independent
record, and emits **this** text. The `pest` grammar in `src/grammar.pest` parses
it into the typed records in `src/model.rs`.

The format is **brace-delimited** (not indentation-sensitive): `pest` has no
INDENT/DEDENT, so explicit `{ }` blocks keep the grammar trivial and indentation
is cosmetic (silent) whitespace. `#` begins a line comment.

## Shape

```
bytecode_format 1

code_object {
  name       "<module>"
  qualname   "<module>"
  filename   "example.py"
  first_line 1
  flags      0
  arg_count  0
  kwonly_count 0

  names    ["print", "x"]
  varnames ["x"]
  consts   [none, int 1, str "hi", code 1]   # `code N` links to nested code object id N

  instruction {
    offset       2
    opname       LOAD_FAST
    opcode       124
    arg          0
    argval       str "x"       # resolved operand (name/const/target); `none` when absent
    argrepr      "x"
    starts_line  2
    is_jump_target false
    jump_targets []            # resolved successor offsets for branches; [] otherwise
    position     2:11-2:12     # or: position none
  }

  code_object {                # nested; id assigned in document order (pre-order)
    name "outer"
    ...
  }
}
```

## Rules

- Every field is `key value`; every record is a keyword plus a `{ ... }` block.
  Fields may appear in any order and may be omitted (the reader supplies
  defaults). Absent scalars are the literal token `none` (`arg none`,
  `starts_line none`, `position none`, `first_line none`).
- **Dataflow enrichments** beyond raw `dis` (the frontend rebuilds the CFG and
  stack effects):
  - `argval` — the *resolved* operand: a jump-target offset (`int`), a const
    value, or a local/global/attr name (`str`), version-normalized. `arg` /
    `argrepr` alone don't identify operands.
  - `jump_targets` — the explicit successor offset(s) for branch/jump ops (empty
    otherwise), so the frontend needn't re-parse `argrepr` strings like `"to 34"`.
  - Per-code-object symbol tables `names` / `varnames` / `consts`, plus
    `arg_count` / `kwonly_count` so the frontend knows which `varnames` are
    parameters. A const that is itself a code object is `code N`, linking to the
    nested `code_object` with document-order id `N`.
- **Values** (`consts` entries and `argval`) are tagged:
  `none`, `bool true|false`, `int <i64>`, `float "<repr>"`, `str "<s>"`,
  `bytes "<latin-1>"`, `code <id>`, `other "<repr>"`. Integers outside `i64`,
  and any other object, normalize to `other`.
- **Strings** use JSON escaping (`\" \\ \/ \b \f \n \r \t \uXXXX`, surrogate
  pairs for astral code points). The serializer emits them via `json.dumps`
  (`ensure_ascii`), so the text is ASCII.
- **Code-object ids** are assigned pre-order over the code-const tree: the module
  is id 0, then each code object found in `co_consts` (in order) gets the next id,
  recursively. Nested `code_object` blocks are emitted in that same order, so the
  reader reconstructs identical ids without them being written out.

## Deviation from the design sketch

The design sketch wrote code references as `code #N`. Because `#` begins a
comment and `pest` inserts implicit comment/whitespace between tokens, `code #N`
would swallow the id as a comment. This format uses `code N` (no `#`) instead.

## Versioning

The header is `bytecode_format <version>`. This reader supports version `1` only;
any other version is a located extraction error. Version robustness across Python
interpreters comes from the **normalizer**, not the parser: one grammar parses
every supported interpreter's output. Never assert identical instruction
sequences across Python versions — only that each version's output parses and
matches *that version's* JSON oracle.
