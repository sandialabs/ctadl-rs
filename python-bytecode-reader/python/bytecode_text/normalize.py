"""Normalize ``dis.Instruction`` records and constants into version-independent form.

All interpreter-version differences are absorbed here: fields that do not exist on
a given version normalize to ``None`` (serialized as the ``none`` token). We never
assume a particular instruction sequence.
"""

import dis
import types

from .model import Instruction, Position

# i64 range: constants outside it can't round-trip through the reader's `i64`, so
# they normalize to `other` (their repr) instead of `int`.
_I64_MIN = -(2 ** 63)
_I64_MAX = 2 ** 63 - 1

# Opcodes whose operand resolves to a jump target offset.
_JUMP_OPCODES = set(dis.hasjrel) | set(dis.hasjabs)
_JUMP_OPCODES |= set(getattr(dis, "hasjump", ()))  # 3.13+ merged list


def normalize_value(value, code_ids):
    """Normalize a Python constant / operand into a ``(kind, payload)`` tuple.

    ``code_ids`` maps ``id(code_object) -> document-order id`` for code constants.
    """
    # bool must precede int (bool is a subclass of int).
    if value is None:
        return ("none", None)
    if isinstance(value, bool):
        return ("bool", value)
    if isinstance(value, int):
        if _I64_MIN <= value <= _I64_MAX:
            return ("int", value)
        return ("other", repr(value))
    if isinstance(value, float):
        return ("float", repr(value))
    if isinstance(value, str):
        return ("str", value)
    if isinstance(value, (bytes, bytearray)):
        return ("bytes", bytes(value).decode("latin-1"))
    if isinstance(value, types.CodeType):
        key = id(value)
        if key in code_ids:
            return ("code", code_ids[key])
        # A code const we somehow didn't pre-number: fall back to its repr so the
        # output stays well-formed rather than crashing.
        return ("other", repr(value))
    return ("other", repr(value))


_MISSING = object()


def _line_number(instr):
    """The instruction's source line, robust across versions.

    3.13+ exposes ``line_number`` (int | None); there ``starts_line`` is a *bool*,
    so it must not be read as a line number (and ``isinstance(True, int)`` is
    ``True``). <=3.12 exposes ``starts_line`` as ``Optional[int]``.
    """
    line = getattr(instr, "line_number", _MISSING)
    if line is not _MISSING:
        return line  # int or None
    line = getattr(instr, "starts_line", None)
    if isinstance(line, int) and not isinstance(line, bool):
        return line
    pos = getattr(instr, "positions", None)
    if pos is not None:
        return pos.lineno
    return None


def _position(instr):
    """The full source span, or ``None`` if any component is unavailable."""
    pos = getattr(instr, "positions", None)
    if pos is None:
        return None
    lineno = pos.lineno
    end_lineno = pos.end_lineno
    col = pos.col_offset
    end_col = pos.end_col_offset
    if None in (lineno, end_lineno, col, end_col):
        return None
    return Position(lineno, col, end_lineno, end_col)


def normalize_instruction(instr, code_ids):
    """Normalize one ``dis.Instruction`` into an :class:`Instruction` record."""
    argval = normalize_value(instr.argval, code_ids)

    jump_targets = []
    if instr.opcode in _JUMP_OPCODES and isinstance(instr.argval, int):
        jump_targets = [instr.argval]

    # A code object's `argrepr` embeds its (non-deterministic) memory address,
    # e.g. `<code object f at 0x1234, ...>`. Drop it: the resolved `code #N`
    # reference in `argval` is the stable, dataflow-relevant operand.
    argrepr = None if argval[0] == "code" else instr.argrepr

    return Instruction(
        offset=instr.offset,
        opname=instr.opname,
        opcode=instr.opcode,
        arg=instr.arg,
        argval=argval,
        argrepr=argrepr,
        starts_line=_line_number(instr),
        is_jump_target=bool(instr.is_jump_target),
        jump_targets=jump_targets,
        position=_position(instr),
    )
