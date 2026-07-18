# Taint captured by a closure over an enclosing local. outer() taints `secret`;
# the nested inner() closes over it and returns it when called, so the value
# reaches sink(). Exercises free-variable capture across nested function scopes.
def main():
    def outer():
        secret = source()

        def inner():
            return secret

        return inner()

    sink(outer())
