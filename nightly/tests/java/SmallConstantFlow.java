// SmallConstantFlow.java
//
// `sipush` takes a two-byte signed operand. Reading four bytes and narrowing to
// a short takes the *following* two bytes as the constant, and runs off the end
// of the code array whenever `sipush` is within three bytes of it. `limit()`
// below compiles to exactly `sipush 1024; ireturn` -- four bytes -- so the
// over-read fails the method, and because classes import atomically it takes
// the taint flow in this class down with it.
public final class SmallConstantFlow {

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    /** Four bytes total: `sipush 1024` at pc 0, `ireturn` at pc 3. */
    static int limit() {
        return 1024;
    }

    static String tag(String in, boolean flag) {
        if (flag) {
            return in;
        }
        return in;
    }

    public static void main(String[] args) {
        sink(tag(source(), limit() > 0));
    }
}
