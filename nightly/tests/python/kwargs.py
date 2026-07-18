# Taint passed through keyword arguments and **kwargs. source() is bound to the
# `payload` keyword; wrap() forwards its collected **kwargs to unpack(), which
# returns the named value to sink(). Exercises keyword binding and dict-splat
# forwarding.
def unpack(payload=None):
    return payload


def wrap(**kwargs):
    return unpack(**kwargs)


def main():
    got = wrap(payload=source())
    sink(got)
