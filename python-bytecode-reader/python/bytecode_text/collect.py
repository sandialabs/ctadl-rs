"""Recursive code-object collection with document-order id assignment.

Code objects are numbered in **pre-order** over the code-const tree: the module
gets id 0, then each code object found in ``co_consts`` (in order) gets the next
id, recursively. A code constant is serialized as ``code #N`` referencing that id,
and nested code objects are emitted (and, on the reader side, re-numbered) in the
same order — so the ids match without being written out explicitly.
"""

import dis
import types

from .model import BytecodeFile, CodeObject
from .normalize import normalize_instruction, normalize_value

FORMAT_VERSION = 1


def collect_file(top_code):
    """Collect a whole file (the module code object and everything nested)."""
    counter = [0]
    code_ids = {}
    top = _collect_code(top_code, counter, code_ids)
    return BytecodeFile(format_version=FORMAT_VERSION, code_objects=[top])


def _collect_code(code, counter, code_ids):
    my_id = counter[0]
    counter[0] += 1
    code_ids[id(code)] = my_id

    # Pre-order: number all nested code objects (in co_consts order) before
    # normalizing this object's consts, so every `code #N` reference resolves.
    nested = []
    for const in code.co_consts:
        if isinstance(const, types.CodeType):
            nested.append(_collect_code(const, counter, code_ids))

    consts = [normalize_value(c, code_ids) for c in code.co_consts]
    # Forward-fill the source line across instructions. On <=3.10 `dis` reports
    # `starts_line` only on the *first* instruction of each source line (``None``
    # otherwise); 3.11+ gives every instruction its own line. Carrying the last
    # seen line forward makes every instruction line-attributed on all versions,
    # so the reader/frontend need not know which interpreter produced the text.
    instructions = []
    current_line = None
    for instr in dis.get_instructions(code):
        record = normalize_instruction(instr, code_ids)
        if record.starts_line is not None:
            current_line = record.starts_line
        else:
            record.starts_line = current_line
        instructions.append(record)

    return CodeObject(
        id=my_id,
        name=code.co_name,
        qualname=getattr(code, "co_qualname", code.co_name),
        filename=code.co_filename,
        first_line=getattr(code, "co_firstlineno", None),
        flags=code.co_flags,
        arg_count=getattr(code, "co_argcount", 0),
        kwonly_count=getattr(code, "co_kwonlyargcount", 0),
        names=list(code.co_names),
        varnames=list(code.co_varnames),
        consts=consts,
        instructions=instructions,
        nested=nested,
    )
