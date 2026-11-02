// The shape R8 gives a class that merges several lambdas: a final int class id, set by the
// constructor, and a switch on it in each method that differed. The dex frontend splits such a
// class back into one class per id. There are five ids, more than CHA resolves statically, so
// `apply` dispatches on the call-target tag of the receiver, which is the split class.
public final class ClassIdMergedFlow {
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
        Fn keep = new Merged(1, source()); // Line 27
        sink(keep.apply("clean"));         // Line 28
        Fn pass = new Merged(0, source());
        sink(pass.apply("clean"));         // Line 30: no flow once the class is split
        Fn arg = new Merged(0, "safe");
        sink(arg.apply(source()));         // Line 32
        System.out.println(new Merged(2, "a").apply("b") + new Merged(3, "c").apply("d")
            + new Merged(4, "e").apply("f"));
    }
}
