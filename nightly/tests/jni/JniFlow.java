// JniFlow.java -- taint crossing the JNI boundary in both directions.
//
// Deliberately shaped so that no per-function propagation model could fake the
// answer: the tainted value has to *enter* one native function, survive in
// native storage, and *leave* through a different one. Nothing in the Java half
// carries it from `nativeStash` to `nativeFetch`.
//
// It also pins the static-method argument shift. A JNI implementation of a
// static native takes `JNIEnv *` then `jclass` before any declared parameter, so
// with no shift the Java argument would land on `env`, and with a shift of one it
// would land on `cls`. Either way nothing reaches the sink.
//
// The native half is JniFlow.c.
public final class JniFlow {

    static {
        System.loadLibrary("jniflow");
    }

    private static native void nativeStash(String data);

    private static native String nativeFetch();

    // SOURCE: returns data that (pretend) comes from outside the program.
    static String source() {
        return System.getProperty("user.name");
    }

    // SINK: consumes the data in a way that could be sensitive.
    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        String tainted = source();
        nativeStash(tainted);
        String out = nativeFetch();
        sink(out);
    }
}
