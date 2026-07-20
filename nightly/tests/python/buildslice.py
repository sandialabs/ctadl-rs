# Opcode: BUILD_SLICE — extended slice subscript `data[0:3:1]`. source() taints
# the list's elements; the extended (3-argument) slice builds a slice object
# (BUILD_SLICE) that subscripts the list, and the sliced result reaches sink().
# BUILD_SLICE must pop only its bounds, leaving the container for the subscript;
# a wrong pop count would consume the container and sever the flow.
# Expected flow: line 8 (source) -> line 10 (sink).
def main():
    data = [source(), source(), source()]
    part = data[0:3:1]
    sink(part)
