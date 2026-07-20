# Taint carried by an exception and recovered via `except*`. The raised
# exception wraps source(); the `except*` handler binds the matching exception
# group as `eg` and sinks it. Exercises taint flow through `CHECK_EG_MATCH`,
# which splits the in-flight exception group into matched / rest subgroups.
def main():
    try:
        raise ValueError(source())
    except* ValueError as eg:
        sink(eg)
