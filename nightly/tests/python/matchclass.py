# Opcode: MATCH_CLASS — class pattern with keyword capture `case Point(x=t)`.
# The constructor stores source() in self.x; the structural match binds that
# attribute to a fresh local `t`, which reaches sink(). Exercises MATCH_CLASS
# binding a captured class attribute into a new name.
# Expected flow: line 14 (source) -> line 17 (sink).
class Point:
    __match_args__ = ("x",)

    def __init__(self, x):
        self.x = x


def main():
    p = Point(source())
    match p:
        case Point(x=t):
            sink(t)
