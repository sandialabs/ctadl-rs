/**
 * `iushr` followed by meaningful one-byte instructions, plus the whole shift
 * family.
 *
 * iushr is a one-byte instruction. Giving it length 3 makes the linear decoder
 * skip the two instructions that follow it; those skipped instructions would
 * have consumed stack slots, so phantom slots pile up and surface at the next
 * control-flow join.
 *
 * The int and long shifts alternate by opcode, so assigning their stack
 * effects by opcode *range* would give `lshl` the int effect and `iushr` the
 * long one.
 */
public class IushrLength {

    /**
     * Each `>>>` emits iushr immediately followed by short one-byte-heavy
     * sequences (bipush/iand/istore_N), and the `if` gives the decoder a join
     * to check the accumulated height against.
     */
    public static int unpackLanguageOrRegion(int in, char base, boolean flag) {
        int first = (in >>> 10) & 0x1f;
        int second = (in >>> 5) & 0x1f;
        int third = in & 0x1f;
        int out = first + second + third + base;
        if (flag) {
            out = out + 1;
        }
        return out;                 // join: both paths must arrive at height 0
    }

    /** Long shifts, to exercise lshl/lshr/lushr stack effects too. */
    public static long shifts(long v, int n, boolean flag) {
        long a = v << n;
        long b = v >> n;
        long c = v >>> n;
        long out = a + b + c;
        if (flag) {
            out = out + 1L;
        }
        return out;
    }

    /** Int shifts, to exercise ishl/ishr/iushr stack effects. */
    public static int intShifts(int v, int n, boolean flag) {
        int a = v << n;
        int b = v >> n;
        int c = v >>> n;
        int out = a + b + c;
        if (flag) {
            out = out + 1;
        }
        return out;
    }
}
