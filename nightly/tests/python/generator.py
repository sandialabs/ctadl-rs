# Taint produced by a generator and consumed downstream. gen() yields a value
# derived from source(); the driving for-loop binds each item and sinks it.
# Exercises taint flow through `yield` and generator iteration.
def gen():
    yield source()


def main():
    for item in gen():
        sink(item)
