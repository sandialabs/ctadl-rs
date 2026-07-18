# Taint carried through a tuple and recovered by unpacking. source() taints the
# second element of `pair`; destructuring binds it to `b`, which reaches sink().
# Exercises tuple construction and unpacking assignment.
def main():
    pair = ("clean", source())
    a, b = pair
    sink(b)
