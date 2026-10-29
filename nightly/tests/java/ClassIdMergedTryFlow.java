// A merged class, as in ClassIdMergedFlow, whose object is built inside a `try`. There a
// `new-instance` can throw, so it ends its basic block and the constructor call that passes the
// class id is in the next block; the dex frontend has to match the two to allocate the split
// class. If it does not, `pass` is the merged class itself, `apply` reaches every arm, and arm 1
// returns the tainted capture. This case asserts that no flow reaches the sink.
public final class ClassIdMergedTryFlow {
    interface Fn { String apply(String s); }

    static final class Merged implements Fn {
        final int id;
        final String captured;
        Merged(int id, String captured) { this.id = id; this.captured = captured; }
        public String apply(String s) {
            switch (id) {
                case 0: return s;
                case 1: return captured;
                case 2: return s.trim();
                case 3: return captured.trim();
                default: return "other";
            }
        }
    }

    static String source() { return "tainted"; }
    static void sink(String s) { System.out.println(s); }

    public static void main(String[] args) {
        Fn pass;
        try {
            pass = new Merged(0, source());
        } catch (RuntimeException e) {
            return;
        }
        sink(pass.apply("clean"));
        System.out.println(new Merged(1, "a").apply("b") + new Merged(2, "c").apply("d")
            + new Merged(3, "e").apply("f") + new Merged(4, "g").apply("h"));
    }
}
