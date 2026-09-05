# Line Map Format (JVM / jvm-reader)

A line map is a JSON file produced by `jvm-reader [--jar] --linemap-json <path> <class-or-jar>` (or the
`collect_line_map_entries` / `write_line_map_json` API). It maps bytecode positions that have a recorded
source line in the class file’s **`LineNumberTable`** to their source file and line number.

The JSON shape matches dex-reader’s `linemap-format.md` so the same consumers can process both DEX and JVM output; the `dex_offset` field name is kept for parity (see below).

## Top-level structure

The file is a JSON array of objects:

```json
[
  {"method":"Lcom/example/Foo;->bar(I)V","source_file":"Foo.java","dex_offset":1234,"line":42},
  {"method":"Lcom/example/Foo;->bar(I)V","source_file":"Foo.java","dex_offset":1238,"line":43}
]
```

## Fields

| Field | Type | Description |
|---|---|---|
| `method` | string | Fully-qualified method reference in the same style as DEX: `"Lpkg/Class;->name(descriptor)"`. The class uses internal JVM name with `/` separators, wrapped in `L…;`; the part after `->` is the method name plus the raw JVM method descriptor (parameters and return type). |
| `source_file` | string | Source file name from the class-level `SourceFile` attribute (e.g. `"Foo.java"`). Empty string when that attribute is absent. |
| `dex_offset` | integer | **Byte offset of the opcode** from the start of the **raw `.class` file bytes** for this class (same basis as `InstructionFlowInfo::file_byte_offset` without adding `pc`). For a class loaded from a JAR, this is relative to the start of that entry’s decompressed `.class` data, not the ZIP archive offset. |
| `line` | integer | Source line number from `LineNumberTable` (typically ≥ 1; the JVM stores `u16`). |

## Ordering

Objects are ordered by:

1. **Class file order** when using a JAR: the order of `.class` entries processed by `JarFileParser` (same order as `class_parsers()`).
2. Within one class: **method declaration order** in the class file.
3. Within one method: ascending `dex_offset` (after sorting `LineNumberTable` rows by `start_pc`).

## Coverage

Only positions listed in the **`LineNumberTable`** attribute of each method’s `Code` attribute are included. Therefore:

- Methods with no `Code` attribute (abstract/native) are omitted.
- Methods with code but **no** `LineNumberTable` (stripped or certain generators) are omitted.
- Not every bytecode offset appears; compilers usually emit one row per logical line change, not per instruction.

## Relationship to bytecode offsets

For a method with `Code` attribute, let `code_base` be the absolute file offset of the first byte of the method’s `code` array (`CodeAttribute::code_byte_offset_in_classfile`). Each row `(start_pc, line_number)` in `LineNumberTable` becomes one JSON object with:

```text
dex_offset = code_base + start_pc
```

The method-local bytecode index is `start_pc` (the same space as `pc` in the verifier and in `InstructionFlowInfo::pc`).

## Correlating with jvm-reader

Use `file_byte_offset` on `InstructionFlowInfo` for a decoded instruction: it equals `code_byte_offset_in_classfile + pc` and should match a `dex_offset` in the line map when `pc` equals a `start_pc` from `LineNumberTable` for that method.

## Example

```json
[
  {
    "method": "LHelloWorld;->main([Ljava/lang/String;)V",
    "source_file": "HelloWorld.java",
    "dex_offset": 42,
    "line": 5
  }
]
```

The numeric `dex_offset` is illustrative; real values depend on the class file layout.
