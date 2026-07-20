# Opcode: UNPACK_EX — extended (starred) unpacking `a, *b, c = seq`.
# source() taints the sequence's elements; the extended unpack binds the leading
# target `a`, the starred capture `b`, and the trailing target `c`. Sinking `b`
# recovers taint carried into the starred capture by UNPACK_EX.
# Expected flow: line 7 (source) -> line 9 (sink).
def main():
    seq = [source(), source(), source()]
    a, *b, c = seq
    sink(b)
