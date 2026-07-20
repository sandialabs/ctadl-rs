# Opcode: IMPORT_FROM — `from mod import name`. source() taints an attribute of
# the module `mod`; the from-import reads that attribute back (IMPORT_FROM loads
# `mod.secret` without consuming the module) and binds it to a local `secret`,
# which reaches sink(). Exercises taint carried through a from-import binding.
# Expected flow: line 8 (source) -> line 11 (sink).
import mod

mod.secret = source()
from mod import secret

sink(secret)
