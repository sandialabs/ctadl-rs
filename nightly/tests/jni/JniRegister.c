/* Native half of the JniRegister case: two implementations bound through a
 * `JNINativeMethod[]` rather than through the JNI name-mangling convention.
 *
 * Neither `stash_impl` nor `fetch_impl` is spelled `Java_JniRegister_…`, so the
 * bridge's symbol tiers find nothing at all. The only record of which Java
 * method each implements is the table below, which is what
 * `ctadl_ascent::languages::jni::registry` recovers straight out of the
 * library's data section.
 *
 * See JniFlow.c for why the JNI types are declared locally instead of coming
 * from <jni.h> (the regression flake ships no NDK), and for why the taint
 * leaves through a call (`keep`) rather than a bare return: a native line is
 * assertable only where there is a call site.
 *
 * Three things about the shapes here are load-bearing:
 *
 *  - `jni_register_methods` has *external* linkage and is not `const`, so it
 *    lands in a writable, non-executable data section and no unreferenced-static
 *    elimination can drop it. Nothing has to actually call `RegisterNatives`
 *    for the case to be honest -- the table is what the scan reads, and this is
 *    byte-for-byte the table a real `JNI_OnLoad` would pass.
 *  - The implementations have external linkage too. A `static` function reached
 *    only through a data pointer may never become a Ghidra function at all,
 *    which would fail the *address to function* step rather than test it.
 *  - `g_slot` is a file-scope global, so the flow between the two natives is the
 *    cross-function global flow tests/c/globalflow.c already pins.
 *
 * This is the one regression case that reads the built library's own bytes, so
 * it needs an ELF target; the scan is a quiet no-op on Mach-O.
 */

typedef void *JNIEnv;
typedef void *jclass;
typedef void *jstring;

typedef struct {
  const char *name;
  const char *signature;
  void *fnPtr;
} JNINativeMethod;

jstring g_slot;

static jstring keep(jstring s) { return s; }

void stash_impl(JNIEnv *env, jclass cls, jstring data) { g_slot = keep(data); }

jstring fetch_impl(JNIEnv *env, jclass cls) { return g_slot; }

JNINativeMethod jni_register_methods[] = {
    {"nativeStash", "(Ljava/lang/String;)V", (void *)stash_impl},
    {"nativeFetch", "()Ljava/lang/String;", (void *)fetch_impl},
};
