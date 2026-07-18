# Taint forwarded through positional varargs. source() is passed positionally
# into forward(*args), which splats the collected tuple into first(), returning
# the leading element to sink(). Exercises *args packing and unpacking.
def first(x, *rest):
    return x


def forward(*args):
    return first(*args)


def main():
    got = forward(source(), "clean")
    sink(got)
