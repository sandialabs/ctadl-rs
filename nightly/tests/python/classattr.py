# Taint stored on an instance attribute and read back through a method. The
# constructor stashes source() in self.data; leak() returns it to sink().
# Exercises field storage and method dispatch on a user-defined class.
class Box:
    def __init__(self, v):
        self.data = v

    def leak(self):
        return self.data


def main():
    box = Box(source())
    sink(box.leak())
