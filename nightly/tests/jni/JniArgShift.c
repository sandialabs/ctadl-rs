/* Native half of the JniArgShift case.
 *
 * See JniFlow.c for why the JNI types are declared locally instead of coming
 * from <jni.h>.
 *
 * `self` is the receiver an *instance* native is handed after `JNIEnv *`, so the
 * declared parameters start at index 2. Returning `b` and not `a` is the whole
 * assertion: it is the last declared parameter, so it is the one the port map
 * gets wrong first if the shift is off.
 */

typedef void *JNIEnv;
typedef void *jobject;
typedef void *jstring;

jstring Java_JniArgShift_nativeCat(JNIEnv *env, jobject self, jstring a,
                                   jstring b) {
  return b;
}
