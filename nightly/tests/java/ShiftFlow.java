// ShiftFlow.java
//
// Taint carried through the shift family. `iushr` is a one-byte instruction;
// decoding it as if it had two inline operand bytes desynchronizes every later
// pc in the method, and the int/long shift opcodes alternate, so stack effects
// assigned by opcode range mis-type `lshl` and `iushr`. Either defect makes
// the enclosing method decode into nonsense and the flow disappears.
public final class ShiftFlow {

    static int source() {
        return 0xdead;
    }

    static void sink(int v) {
        System.out.println(v);
    }

    /** `iushr` followed by one-byte instructions, with a join after them. */
    static int unpack(int in, boolean flag) {
        int first = (in >>> 10) & 0x1f;
        int second = (in >>> 5) & 0x1f;
        int third = in & 0x1f;
        int out = first + second + third;
        if (flag) {
            out = out + 1;
        }
        return out;
    }

    /** The long shifts, whose stack effects differ from the int ones. */
    static long wide(long v, int n, boolean flag) {
        long out = (v << n) + (v >> n) + (v >>> n);
        if (flag) {
            out = out + 1L;
        }
        return out;
    }

    public static void main(String[] args) {
        sink(unpack(source(), true));
        sink((int) wide(source(), 3, true));
    }
}
