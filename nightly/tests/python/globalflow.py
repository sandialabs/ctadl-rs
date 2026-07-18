# Taint through a module-level global. produce() writes source() into g_data;
# consume() reads it back and sinks it. Neither function passes anything to the
# other, so the flow exists only if taint is tracked through global storage
# across calls.
g_data = None


def produce():
    global g_data
    g_data = source()


def consume():
    sink(g_data)


def main():
    produce()
    consume()
