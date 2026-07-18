"""Normalized, version-independent bytecode records.

A ``Value`` (a constant or a resolved instruction operand) is a ``(kind, payload)``
tuple whose ``kind`` is one of: ``none``, ``bool``, ``int``, ``float``, ``str``,
``bytes``, ``code``, ``other``. ``float``/``other`` payloads are ``repr`` strings;
``bytes`` payloads are latin-1 decodes; ``code`` payloads are the document-order
id of the referenced nested code object.
"""

from dataclasses import dataclass, field
from typing import List, Optional, Tuple

# A normalized value: (kind, payload).
Value = Tuple[str, object]


@dataclass
class Position:
    start_line: int
    start_column: int
    end_line: int
    end_column: int


@dataclass
class Instruction:
    offset: int
    opname: str
    opcode: int
    arg: Optional[int]
    argval: Value
    argrepr: Optional[str]
    starts_line: Optional[int]
    is_jump_target: bool
    jump_targets: List[int]
    position: Optional[Position]


@dataclass
class CodeObject:
    id: int
    name: str
    qualname: str
    filename: str
    first_line: Optional[int]
    flags: int
    arg_count: int
    kwonly_count: int
    names: List[str]
    varnames: List[str]
    consts: List[Value]
    instructions: List[Instruction] = field(default_factory=list)
    nested: List["CodeObject"] = field(default_factory=list)


@dataclass
class BytecodeFile:
    format_version: int
    code_objects: List[CodeObject]
