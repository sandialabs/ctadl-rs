// WideFieldFlow.java
//
// Taint carried across `long` and `double` field accesses. Those are category-2
// types: the four field opcodes move two operand-stack words for them and one
// for everything else. A decoder that pushes or pops one word regardless leaves
// the simulated stack a word short at every such access, and the shortfall
// surfaces as an underflow at whatever consumes the value -- an `lcmp`, an
// arithmetic opcode, or a call argument -- which drops the whole method.
public final class WideFieldFlow {
    private long budget;
    private double ratio;
    private static long total;
    private static double scale;

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    /** putfield J, then getfield J under an lcmp. */
    String throughLongField(String in, long v) {
        this.budget = v;
        if (this.budget > 0L) {
            return in;
        }
        return "";
    }

    /** putstatic D, then getstatic D under a dcmp. */
    static String throughDoubleStatic(String in, double d) {
        scale = d;
        if (scale > 1.0d) {
            return in;
        }
        return "";
    }

    /** Both widths in one frame: putfield D, getfield D, putstatic J, getstatic J. */
    String throughMixed(String in, double d) {
        this.ratio = d;
        total = (long) this.ratio;
        if (this.ratio > 0.0d && total > 0L) {
            return in;
        }
        return "";
    }

    public static void main(String[] args) {
        WideFieldFlow f = new WideFieldFlow();
        sink(f.throughLongField(source(), 5L));
        sink(throughDoubleStatic(source(), 2.0d));
        sink(f.throughMixed(source(), 3.0d));
    }
}
