// JniRegister.java -- taint crossing a JNI boundary that no symbol name joins.
//
// The two natives below have no `Java_JniRegister_…` symbol anywhere in the
// shared library. Their implementations are bound at run time, the way most
// real Android apps bind theirs: `JNI_OnLoad` hands `RegisterNatives` a
// `JNINativeMethod[]` of (name, signature, function pointer) triples. Nothing
// in the library's symbol table records which Java method each one belongs to.
//
// So this case fails outright under the mangled-name convention alone. It
// passes only if the table itself is recovered from the library's data section
// at import time and its entries attributed back to this class -- see
// `docs/jni.md` and `ctadl_ascent::languages::jni::registry`.
//
// The dataflow is JniFlow's, deliberately: the tainted value enters one native
// function, survives in native storage, and leaves through a *different* one,
// so no per-function propagation model could fake the answer. What is new here
// is only how the two halves are joined.
//
// The native half is JniRegister.c.
public final class JniRegister {

    static {
        System.loadLibrary("jniregister");
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
