# Opcode: MATCH_SEQUENCE (+ GET_LEN, UNPACK_SEQUENCE) — sequence pattern
# `case [a, b]`. source() taints the list's elements; the sequence pattern tests
# the subject's shape (MATCH_SEQUENCE) and length (GET_LEN), then unpacks it,
# binding `b`, which reaches sink(). Exercises taint through a matched sequence
# element — the length guard must not consume the subject.
# Expected flow: line 8 (source) -> line 11 (sink).
def main():
    data = [source(), source()]
    match data:
        case [a, b]:
            sink(b)
