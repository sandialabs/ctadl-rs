/**
 * `long` and `double` parameters, which occupy two local slots each.
 *
 * The local-slot index space and the declared-parameter index space diverge at
 * the first wide parameter: in `onlyLong(long, int, boolean)` the long owns
 * slots 0-1, so `n` is slot 2 and `flag` slot 3, while the parameter ordinals
 * are only 0, 1, 2. A decoder that reports the slot as `Location::Parameter`
 * therefore names parameters that do not exist, and the IR verifier rejects the
 * method.
 *
 * Wide parameters appear here in leading, middle and trailing position, on both
 * static and instance methods; `noWide` is the control, where the two spaces
 * coincide.
 */
public class WideParams {

    private long acc;

    // --- static ---

    public static long onlyLong(long v, int n, boolean flag) {
        long out = v + n;
        if (flag) { out = out + 1L; }
        return out;
    }

    public static int noWide(int v, int n, boolean flag) {
        int out = v + n;
        if (flag) { out = out + 1; }
        return out;
    }

    public static double withDouble(double d, int n) {
        return d + n;
    }

    /** Wide parameter in the middle: slots 0, 1-2, 3. */
    public static long middleLong(int n, long v, boolean flag) {
        long out = v + n;
        if (flag) { out = out + 1L; }
        return out;
    }

    /** Wide parameter last: slots 0, 1, 2-3. */
    public static double trailingDouble(int n, boolean flag, double d) {
        double out = d + n;
        if (flag) { out = out + 1.0; }
        return out;
    }

    /** Two wide parameters, so the two spaces are off by two by the end. */
    public static double twoWide(long v, double d, int n) {
        return v + d + n;
    }

    /** A wide parameter followed by a reference, then a local of its own. */
    public static int wideThenRef(double d, String s, int n) {
        int len = s.length() + n;
        return (int) d + len;
    }

    // --- instance: `this` takes slot 0 and is parameter ordinal 0 ---

    public long instanceLeadingLong(long v, int n) {
        acc = v + n;
        return acc;
    }

    public long instanceMiddleDouble(int n, double d, boolean flag) {
        long out = (long) (d + n);
        if (flag) { out = out + 1L; }
        acc = out;
        return acc;
    }

    public long instanceTrailingLong(int n, long v) {
        acc = v + n;
        return acc;
    }

    public int instanceNoWide(int v, int n) {
        return v + n;
    }
}
