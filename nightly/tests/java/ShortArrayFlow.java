// ShortArrayFlow.java
//
// A `short[]` store and load sharing a frame with a taint flow. `sastore`
// (0x56) closes the array-store block, but it sits one past a `0x4f..=0x55`
// range, so a table that stops at `castore` leaves it modelled as a no-op: its
// arrayref, index and value stay on the simulated stack and the first join the
// store dominates reports three phantom slots. Classes import atomically, so
// the failing method takes the flow below down with it.
public final class ShortArrayFlow {

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    /** `sastore` then `saload`, with a join after the store. */
    static String roundTrip(String in, int code, boolean flag) {
        short[] table = new short[4];
        table[0] = (short) code;
        int out = table[0];
        if (flag) {
            out = out + 1;
        }
        if (out > 0) {
            return in;
        }
        return in;
    }

    public static void main(String[] args) {
        sink(roundTrip(source(), 7, true));
    }
}
