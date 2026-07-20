# Opcode: FORMAT_WITH_SPEC — f-string field with a *dynamic* format spec
# `f"{t:{width}}"`. source() taints t; formatting it with a computed spec passes
# the value's taint through into the formatted string, which reaches sink().
# (A literal spec compiles to FORMAT_SIMPLE; a `{...}` spec forces
# FORMAT_WITH_SPEC, which pops the spec and value and pushes the formatted value.)
# Expected flow: line 8 (source) -> line 11 (sink).
def main():
    t = source()
    width = 10
    out = f"{t:{width}}"
    sink(out)
