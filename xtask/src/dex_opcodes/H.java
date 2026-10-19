// The Java side of each `cargo xtask dex-opcodes` case (see mod.rs). A case's
// smali loads its registers from H's `i*` fields, runs the instruction under
// test, and stores its registers to H's `o*` fields. Invoke cases record their
// callee's arguments in `rec`. The per-register fields are generated (the
// @FIELDS@ marker), one of each type for every register a case may use.

import java.lang.invoke.CallSite;
import java.lang.invoke.ConstantCallSite;
import java.lang.invoke.MethodHandles;
import java.lang.invoke.MethodType;

public class H {
    /** An int argument that makes every callee throw, so invokes can fault. */
    public static final int MAGIC = 0xDEAD;

    public static String rec;
    public static int retI;
    public static long retJ;
    public static Object retL;
    public static Throwable exc;

    // @FIELDS@

    static void record(String what, Object... args) {
        StringBuilder sb = new StringBuilder(what).append('(');
        for (int i = 0; i < args.length; i++) {
            if (i > 0) sb.append(',');
            sb.append(Driver.describe(args[i]));
        }
        rec = sb.append(')').toString();
        for (Object a : args) {
            if (a instanceof Integer && (Integer) a == MAGIC) {
                throw new IllegalStateException("magic argument");
            }
        }
    }

    public static class Obj implements Iface {
        public int id;
        public int i;
        public long j;
        public Object o;
        public boolean z;
        public byte b;
        public char c;
        public short s;

        public Obj() {}

        public Obj(int k) {
            init(k);
        }

        public void init(int k) {
            id = k;
            i = 1000 + k;
            j = 0x100000000L * (k + 1) + 7 * k;
            o = "o" + k;
            z = k % 2 == 1;
            b = (byte) (10 + k);
            c = (char) ('a' + k);
            s = (short) (300 + k);
        }

        public void v(int a, long b, Object c) {
            record("virtual#" + id, a, b, c);
        }
    }

    public interface Iface {
        void v(int a, long b, Object c);
    }

    /** Static fields for sget and sput. Reset before every run. */
    public static class S {
        public static int i;
        public static long j;
        public static Object o;
        public static boolean z;
        public static byte b;
        public static char c;
        public static short s;
        public static float f;
        public static double d;

        static void reset() {
            i = 41;
            j = 0x4200000043L;
            o = "static";
            z = true;
            b = 44;
            c = 'E';
            s = 46;
            f = 4.75f;
            d = 48.5;
        }

        static String snapshot() {
            return i + "," + j + "," + Driver.describe(o) + "," + z + "," + b + "," + (int) c
                    + "," + s + "," + Float.floatToRawIntBits(f) + ","
                    + Double.doubleToRawLongBits(d);
        }
    }

    public static void s(int a, int a2, long b, Object c) {
        record("static", a, a2, b, c);
    }

    public static void recDirect(Obj self, int a, long b, Object c) {
        record("direct#" + self.id, a, b, c);
    }

    public static void mh0(int a, int b) {
        record("mh0", a, b);
    }

    public static void mh1(int a, int b) {
        record("mh1", a, b);
    }

    public static void cs(int a, int b) {
        record("cs", a, b);
    }

    public static CallSite bsm(MethodHandles.Lookup lookup, String name, MethodType type)
            throws Exception {
        return new ConstantCallSite(lookup.findStatic(H.class, name, type));
    }

    public static int retI() {
        return retI;
    }

    public static long retJ() {
        return retJ;
    }

    public static Object retL() {
        return retL;
    }
}
