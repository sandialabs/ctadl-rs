# Taint carried through a recursive call chain. descend() hands its tainted
# argument to itself until the base case returns it back up, so the analysis
# must reach a fixed point on descend()'s own summary rather than walk a finite
# chain of distinct functions.
def descend(depth, v):
    if depth == 0:
        return v
    return descend(depth - 1, v)


def main():
    data = source()
    result = descend(3, data)
    sink(result)
