/* Native half of the JniArgShift case.
 *
 * See JniFlow.c for why the JNI types are declared locally instead of coming
 * from <jni.h>, and for why the taint leaves through a call (`keep`) rather than
 * a bare return: a native line is assertable only where there is a call site.
 *
 * `self` is the receiver an *instance* native is handed after `JNIEnv *`, so the
 * declared parameters start at index 2. Returning `b` and not `a` is the whole
 * assertion: it is the last declared parameter, so it is the one the port map
 * gets wrong first if the shift is off. The `keep(b)` call is that same claim
 * seen from the native side -- its argument is tainted only if `b` is what
 * crossed the boundary.
 */

typedef void *JNIEnv;
typedef void *jobject;
typedef void *jstring;

static jstring keep(jstring s) { return s; }

jstring Java_JniArgShift_nativeCat(JNIEnv *env, jobject self, jstring a,
                                   jstring b) {
  return keep(b);
}
