# Opcodes: MATCH_MAPPING / MATCH_KEYS — mapping pattern `case {"key": t}`.
# source() taints the dict value; the mapping pattern tests the subject's shape
# (MATCH_MAPPING), pulls the value at "key" (MATCH_KEYS), and binds it to `t`,
# which reaches sink(). Exercises taint flowing from a matched mapping value.
# Expected flow: line 7 (source) -> line 10 (sink).
def main():
    obj = {"key": source()}
    match obj:
        case {"key": t}:
            sink(t)
