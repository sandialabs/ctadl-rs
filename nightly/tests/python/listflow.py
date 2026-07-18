# Taint flowing into a list element and back out. source() taints an element
# appended to `items`; the value is read back by index and reaches sink(). The
# flow survives only if list-element storage carries taint across the append.
def main():
    items = []
    items.append(source())
    got = items[0]
    sink(got)
