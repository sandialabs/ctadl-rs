"""Serialize normalized records to stable text or to the JSON oracle.

Both outputs derive from the *same* normalized records (see :mod:`.collect`), so
the JSON oracle is a faithful ground truth for the differential test: the reader
parses the stable text and must reproduce exactly the JSON.

The stable text is brace-delimited; indentation is cosmetic. Strings are emitted
via ``json.dumps`` (``ensure_ascii``), so every string is ASCII with ``\\uXXXX``
escapes (surrogate pairs for astral code points) — exactly what the grammar and
the reader's unescaper expect.

The JSON shape matches serde's default (externally-tagged) representation of the
reader's model, so a ``BytecodeFile`` deserializes from it directly.
"""

import json

_INDENT = "  "


def _s(text):
    """A stable-text string literal (quoted, JSON-escaped, ASCII)."""
    return json.dumps(text, ensure_ascii=True)


# --- Stable text ----------------------------------------------------------


def to_stable(bytecode_file):
    out = []
    out.append("bytecode_format %d" % bytecode_file.format_version)
    out.append("")
    for code in bytecode_file.code_objects:
        _emit_code(out, code, 0)
    return "\n".join(out) + "\n"


def _emit_code(out, code, depth):
    pad = _INDENT * depth
    inner = _INDENT * (depth + 1)
    out.append("%scode_object {" % pad)
    out.append("%sname       %s" % (inner, _s(code.name)))
    out.append("%squalname   %s" % (inner, _s(code.qualname)))
    out.append("%sfilename   %s" % (inner, _s(code.filename)))
    out.append("%sfirst_line %s" % (inner, _int_or_none(code.first_line)))
    out.append("%sflags      %d" % (inner, code.flags))
    out.append("%sarg_count  %d" % (inner, code.arg_count))
    out.append("%skwonly_count %d" % (inner, code.kwonly_count))
    out.append("%snames    %s" % (inner, _string_list(code.names)))
    out.append("%svarnames %s" % (inner, _string_list(code.varnames)))
    out.append("%sconsts   %s" % (inner, _value_list(code.consts)))
    for instr in code.instructions:
        _emit_instruction(out, instr, depth + 1)
    for nested in code.nested:
        _emit_code(out, nested, depth + 1)
    out.append("%s}" % pad)


def _emit_instruction(out, instr, depth):
    pad = _INDENT * depth
    inner = _INDENT * (depth + 1)
    out.append("%sinstruction {" % pad)
    out.append("%soffset       %d" % (inner, instr.offset))
    out.append("%sopname       %s" % (inner, instr.opname))
    out.append("%sopcode       %d" % (inner, instr.opcode))
    out.append("%sarg          %s" % (inner, _int_or_none(instr.arg)))
    out.append("%sargval       %s" % (inner, _value(instr.argval)))
    out.append("%sargrepr      %s" % (inner, _string_or_none(instr.argrepr)))
    out.append("%sstarts_line  %s" % (inner, _int_or_none(instr.starts_line)))
    out.append("%sis_jump_target %s" % (inner, "true" if instr.is_jump_target else "false"))
    out.append("%sjump_targets %s" % (inner, _int_list(instr.jump_targets)))
    out.append("%sposition     %s" % (inner, _position(instr.position)))
    out.append("%s}" % pad)


def _int_or_none(value):
    return "none" if value is None else str(value)


def _string_or_none(value):
    return "none" if value is None else _s(value)


def _string_list(items):
    return "[" + ", ".join(_s(x) for x in items) + "]"


def _int_list(items):
    return "[" + ", ".join(str(x) for x in items) + "]"


def _value_list(items):
    return "[" + ", ".join(_value(v) for v in items) + "]"


def _value(value):
    kind, payload = value
    if kind == "none":
        return "none"
    if kind == "bool":
        return "bool true" if payload else "bool false"
    if kind == "int":
        return "int %d" % payload
    if kind == "float":
        return "float %s" % _s(payload)
    if kind == "str":
        return "str %s" % _s(payload)
    if kind == "bytes":
        return "bytes %s" % _s(payload)
    if kind == "code":
        return "code %d" % payload
    if kind == "other":
        return "other %s" % _s(payload)
    raise ValueError("unknown value kind: %r" % (kind,))


def _position(pos):
    if pos is None:
        return "none"
    return "%d:%d-%d:%d" % (pos.start_line, pos.start_column, pos.end_line, pos.end_column)


# --- JSON oracle ----------------------------------------------------------


def to_json(bytecode_file):
    doc = {
        "format_version": bytecode_file.format_version,
        "code_objects": [_code_json(c) for c in bytecode_file.code_objects],
    }
    return json.dumps(doc, ensure_ascii=True)


def _code_json(code):
    return {
        "name": code.name,
        "qualname": code.qualname,
        "filename": code.filename,
        "first_line": code.first_line,
        "flags": code.flags,
        "arg_count": code.arg_count,
        "kwonly_count": code.kwonly_count,
        "names": list(code.names),
        "varnames": list(code.varnames),
        "consts": [_value_json(v) for v in code.consts],
        "instructions": [_instruction_json(i) for i in code.instructions],
        "nested_code_objects": [_code_json(n) for n in code.nested],
    }


def _instruction_json(instr):
    return {
        "offset": instr.offset,
        "opname": instr.opname,
        "opcode": instr.opcode,
        "arg": instr.arg,
        "argval": _value_json(instr.argval),
        "argrepr": instr.argrepr,
        "starts_line": instr.starts_line,
        "is_jump_target": instr.is_jump_target,
        "jump_targets": list(instr.jump_targets),
        "position": _position_json(instr.position),
    }


def _value_json(value):
    """Match serde's default externally-tagged representation of ``ConstEntry``."""
    kind, payload = value
    if kind == "none":
        return "None"
    if kind == "bool":
        return {"Bool": payload}
    if kind == "int":
        return {"Int": payload}
    if kind == "float":
        return {"Float": payload}
    if kind == "str":
        return {"Str": payload}
    if kind == "bytes":
        return {"Bytes": payload}
    if kind == "code":
        return {"Code": payload}
    if kind == "other":
        return {"Other": payload}
    raise ValueError("unknown value kind: %r" % (kind,))


def _position_json(pos):
    if pos is None:
        return None
    return {
        "start_line": pos.start_line,
        "start_column": pos.start_column,
        "end_line": pos.end_line,
        "end_column": pos.end_column,
    }
