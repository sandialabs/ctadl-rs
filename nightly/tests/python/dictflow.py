# Taint stored as a dictionary value and read back by key. source() taints
# creds["token"]; the same key is read back and passed to sink(). Exercises
# taint through dict value storage keyed by a string literal.
def main():
    creds = {}
    creds["token"] = source()
    value = creds["token"]
    sink(value)
