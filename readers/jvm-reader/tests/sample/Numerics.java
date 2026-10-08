/**
 * Wide and floating-point arithmetic with conversions. Every other sample is
 * int-only; this exercises the `long`/`float`/`double` opcodes and the numeric
 * cast/compare instructions (`ladd`, `dmul`, `lcmp`, `i2l`, `l2f`, `f2d`,
 * `d2i`, ...) so the disassembler/javap comparison covers them.
 */
public class Numerics {
    static long addLong(long a, long b) {
        return a + b;
    }

    static double mul(double a, double b) {
        return a * b;
    }

    static int toInt(double d) {
        return (int) d;
    }

    static long widen(int x) {
        return (long) x;
    }

    static int less(long a, long b) {
        return a < b ? 1 : 0;
    }

    static double mix(int i, long l, float f, double d) {
        return i + l + f + d;
    }
}
