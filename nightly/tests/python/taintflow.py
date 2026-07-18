# End-to-end taint through an interprocedural summary, the Python analogue of
# the C source->transfer->sink cases: source() is tainted, transfer returns its
# argument, and the tainted value reaches sink().
def transfer(a):
    b = a
    return b


def main():
    x = source()
    y = transfer(x)
    sink(y)
