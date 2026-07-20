# Opcode: LOAD_SUPER_ATTR — `super().reveal()` in a subclass. A tainted value is
# stashed on the instance's `data` attribute; the subclass method `leak` reaches
# the inherited `reveal` through `super()`, which returns `self.data` back to
# sink(). Exercises taint carried through a super-bound method call.
# Expected flow: line 21 (source) -> line 22 (sink).
class Base:
    def reveal(self):
        return self.data


class Derived(Base):
    def stash(self, v):
        self.data = v

    def leak(self):
        return super().reveal()


def main():
    d = Derived()
    d.stash(source())
    sink(d.leak())
