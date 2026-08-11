/* Native half of the JniFlow case: the implementations of JniFlow's two
 * `static native` methods, under the names the JNI mangler derives from them
 * (`Java_` + class + `_` + method, with no package to mangle here).
 *
 * The JNI types are declared locally rather than pulled from <jni.h>: the
 * regression flake supplies no NDK (flake.nix, includeNDK = false), and the
 * analysis only needs each function's arity and the shape of the dataflow.
 *
 * `g_slot` is a file-scope global, not a static one, so the flow is the same
 * cross-function global flow tests/c/globalflow.c already pins -- what is new
 * here is that the write and the read are reached from Java.
 *
 * `keep` is what gives the case a native line to assert on. CTADL reports a
 * tainted *instruction* at a call whose argument is tainted, so a native
 * statement is nameable in `expected_native_lines` only if it is a call: with
 * `g_slot = data;` written directly there is no native call site at all, and the
 * C half contributes no located result however plainly the taint runs through it.
 */

typedef void *JNIEnv;
typedef void *jclass;
typedef void *jstring;

jstring g_slot;

static jstring keep(jstring s) { return s; }

void Java_JniFlow_nativeStash(JNIEnv *env, jclass cls, jstring data) {
  g_slot = keep(data);
}

jstring Java_JniFlow_nativeFetch(JNIEnv *env, jclass cls) {
  return g_slot;
}
