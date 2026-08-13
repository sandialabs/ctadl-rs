// JniArgShift.java -- pins the *instance* argument shift and the argument order.
//
// An instance native's implementation takes `JNIEnv *`, then the receiver
// `jobject`, then the declared parameters -- so `this` is native index 1, `a` is
// 2 and `b` is 3. The C body returns `b` and nothing else, so exactly one of the
// two calls below may taint its sink. An off-by-one anywhere in the port map
// flips both assertions at once: `expected_lines` stops being reached and
// `unexpected_lines` starts being.
//
// `nativeCat` is deliberately *not* private, so the call is an `invoke-virtual`
// resolved through class-hierarchy analysis rather than an `invoke-direct` that
// names its callee outright.
//
// The native half is JniArgShift.c.
public final class JniArgShift {

    static {
        System.loadLibrary("jniargshift");
    }

    native String nativeCat(String a, String b);

    // SOURCE: returns data that (pretend) comes from outside the program.
    static String source() {
        return System.getProperty("user.name");
    }

    // SINK: consumes the data in a way that could be sensitive.
    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        JniArgShift self = new JniArgShift();
        String tainted = source();
        // Taint in `b`, which the implementation returns: this sink is reached.
        String fromB = self.nativeCat("clean", tainted);
        sink(fromB);
        // Taint in `a`, which the implementation drops: this sink is not.
        String fromA = self.nativeCat(tainted, "clean");
        sink(fromA);
    }
}
