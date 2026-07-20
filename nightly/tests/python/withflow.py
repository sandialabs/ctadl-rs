# Opcodes: BEFORE_WITH (+ WITH_EXCEPT_START on the cleanup path) — a `with`
# statement whose context manager is tainted. `with source() as x` binds the
# entered value to `x`; since the common `__enter__` returns the manager itself,
# the manager's taint reaches `x` and then sink(). The implicit `__exit__`
# cleanup path (WITH_EXCEPT_START) is also emitted but carries no data taint.
# Expected flow: line 8 (source) -> line 9 (sink).
def main():
    with source() as x:
        sink(x)
