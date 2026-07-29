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
 */

typedef void *JNIEnv;
typedef void *jclass;
typedef void *jstring;

jstring g_slot;

void Java_JniFlow_nativeStash(JNIEnv *env, jclass cls, jstring data) {
  g_slot = data;
}

jstring Java_JniFlow_nativeFetch(JNIEnv *env, jclass cls) {
  return g_slot;
}
