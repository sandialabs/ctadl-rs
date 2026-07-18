# Taint propagated through a list comprehension. Every element of `tainted` is
# derived from source(); the first is pulled back out and sunk. Exercises the
# implicit loop and per-element binding a comprehension introduces.
def main():
    seed = source()
    tainted = [seed for _ in range(3)]
    sink(tainted[0])
