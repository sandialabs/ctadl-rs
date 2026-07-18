"""bytecode_text: normalize Python bytecode into a stable, version-independent form.

This pure-stdlib package (``dis`` + ``marshal``) compiles ``.py`` source or reads
``.pyc``, normalizes each ``dis.Instruction`` into a version-independent record,
and serializes to either the stable text format that ``python-bytecode-reader``
parses or a JSON oracle carrying the *same* records (used by the differential
tests). It runs unmodified on every supported interpreter (3.11-3.14).
"""

from .serialize import to_stable, to_json  # noqa: F401
from .collect import collect_file  # noqa: F401

FORMAT_VERSION = 1
