/* Native half of the JniWide case: a static `(IJ)I` native, built for 32-bit x86.
 *
 * cdecl passes `data` on the stack in two words, low then high, after `env`,
 * `cls` and `tag`. With DWARF, Ghidra recovers four parameters and the `long` is
 * one of them; stripped, it recovers five, and only the `SplitWide` layout maps
 * the Java argument onto the fifth. `keep` is handed the high word alone, so its
 * argument is tainted only if the high half was mapped.
 *
 * Two things about the shape are load-bearing for the stripped build:
 *
 *  - The function reads every parameter. Ghidra recovers a stack parameter list
 *    for an export nothing calls only when the lower slots are read too: one
 *    that ignored `env`, `cls` and `tag` gets no parameters at all.
 *  - The case's config makes `keep`'s argument a sink. Stripped, Ghidra infers
 *    no return value for an export nothing calls, so the taint cannot come back
 *    to Java through the return, and the native sink is what that build's flow
 *    reaches. The `-g` build's flow reaches the Java sink as well.
 *
 * See JniFlow.c for why the JNI types are declared locally instead of coming
 * from <jni.h>.
 */

typedef void *JNIEnv;
typedef void *jclass;
typedef int jint;
typedef long long jlong;

static jint keep(jint v) { return v; }

jint Java_JniWide_nativeHigh(JNIEnv *env, jclass cls, jint tag, jlong data) {
  jint used = tag + (env != 0) + (cls != 0);
  return keep((jint)(data >> 32) + used);
}
