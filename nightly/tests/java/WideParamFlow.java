// WideParamFlow.java
//
// Taint carried through a reference parameter that sits *after* a `long` or
// `double` one. A wide parameter occupies two local slots but one declared
// ordinal, so the two index spaces diverge: in `afterWide` the tainted String
// is local slot 2 but parameter ordinal 1. A decoder that reports the slot as
// the parameter attributes the argument to a parameter that does not exist (or
// to the wrong one), and the flow into the sink is lost.
public final class WideParamFlow {

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    /** Leading wide parameter: `s` is ordinal 1, slot 2. */
    static void afterWide(long pad, String s, int n) {
        sink(s);
    }

    /** Trailing wide parameter: `s` is ordinal 0, slot 0, with a wide behind it. */
    static void beforeWide(String s, double pad) {
        sink(s);
    }

    /** Two wide parameters, so the spaces are off by two by the last one. */
    static void afterTwoWide(long a, double b, String s) {
        sink(s);
    }

    /** Instance method: `this` takes slot 0, so every ordinal shifts by one. */
    void instanceAfterWide(double pad, String s) {
        sink(s);
    }

    public static void main(String[] args) {
        afterWide(1L, source(), 2);
        beforeWide(source(), 1.0);
        afterTwoWide(1L, 2.0, source());
        new WideParamFlow().instanceAfterWide(1.0, source());
    }
}
