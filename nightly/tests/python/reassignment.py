# Negative case: the tainted value is overwritten before it reaches the sink, so
# no flow should be reported. source() taints `data`, but `data` is reassigned
# to a constant on the next line; the sink therefore sees only clean data.
# Empty expected_lines makes this a negative test.
def main():
    data = source()
    data = "clean"
    sink(data)
