# Taint carried as an exception payload across a raise/except boundary. The
# tainted value is wrapped in a ValueError, raised, caught, and recovered from
# err.args before reaching sink(). Exercises taint flow through exception objects.
def main():
    try:
        raise ValueError(source())
    except ValueError as err:
        leaked = err.args[0]
        sink(leaked)
