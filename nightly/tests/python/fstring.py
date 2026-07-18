# Taint propagated through f-string interpolation. source() is embedded in an
# f-string; the resulting string is tainted and reaches sink(). Exercises taint
# flow through string formatting/concatenation.
def main():
    secret = source()
    message = f"value={secret}"
    sink(message)
