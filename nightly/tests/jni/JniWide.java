// JniWide.java -- a tainted `long` crossing the JNI boundary into a 32-bit library.
//
// On a 32-bit ABI a `long` arrives in two native parameters, low half then high
// half, whenever the disassembler recovered no types -- and the bridge has to map
// the one Java slot to both. The native half returns a value computed from the
// *high* half only, so a bridge that mapped the argument to one native parameter
// (the 64-bit layout) would deliver the taint to the low half and lose it.
//
// The case runs with the library built `-g`, so Ghidra recovers the typed
// prototype and the `long` is one parameter, and again with the debug info
// stripped, so it is two. Both must find the flow.
//
// The native returns an `int` rather than a `long`: on 32-bit x86 a `long` comes
// back in two registers, which the pcode frontend does not model as one return
// value, so taint could not return to Java through it.
//
// The native half is JniWide.c.
public final class JniWide {

    static {
        System.loadLibrary("jniwide");
    }

    private static native int nativeHigh(int tag, long data);

    // SOURCE: returns data that (pretend) comes from outside the program.
    static long source() {
        return Long.parseLong(System.getProperty("user.name"));
    }

    // SINK: consumes the data in a way that could be sensitive.
    static void sink(long v) {
        System.out.println(v);
    }

    public static void main(String[] args) {
        long tainted = source();
        int out = nativeHigh(7, tainted);
        sink(out);
    }
}
