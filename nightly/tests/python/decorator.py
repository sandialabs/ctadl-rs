# Taint through a decorated function. identity() returns its argument; the
# @trace decorator wraps it in a pass-through that forwards args and return
# value. The tainted value survives the wrapper and reaches sink().
def trace(fn):
    def wrapper(*args):
        return fn(*args)

    return wrapper


@trace
def identity(x):
    return x


def main():
    got = identity(source())
    sink(got)
