// Runs each `jvm:opcodes` case on the JVM and reports which inputs of the
// instruction under test change which of its outputs (see mod.rs).
//
//   java -cp <asm jars> JvmOpcodes.java <out dir>
//
// Each case is one class, `Case_<n>`, built here with ASM. Its `run()` loads
// every input from H -- eight int locals as padding, the case's own locals,
// then the operand stack bottom first, starting with one padding int -- runs
// the one instruction under test, and hands every stack slot and local back to
// H. The classes go to <out dir>/classes, where mod.rs decodes them with
// jvm-reader.
//
// A case runs once with every input at its first candidate value, then once
// per other candidate of each 32-bit input slot (a long or double is two slots,
// perturbed separately). Every run observes a set of places, named as
// `AsmFlow.java` names them:
//
//   out<p>                 stack slot p after the instruction
//   local<n>               local n after
//   field:<C>.<f>@<slot>   a field of the input object in <slot>, if changed
//   field:<C>.<f>          a static field, if changed
//   elem:<slot>            the input array in <slot>, if changed
//   recv, arg<j>           what a callee received
//   thrown                 what athrow threw
//
// and the results, in <out dir>/results.txt, are, per case:
//
//   case <name> <class> op=<opcode> index=<i> h=<height before>
//   place <place> changed=<0|1> influence=<slot>,...
//   error <what went wrong>
//   end
//
// `changed` is whether the baseline run wrote the place (a stack slot or local
// that differs from its input, or any other place observed at all). The
// influence of a place is the set of input slots (in<p>, local<n>) whose
// perturbation changed it. A long or double result is one value, so both of
// its stack slots get what either half shows. Both input slots of a wide local
// are named by its first slot, as jvm-reader names the local.

import java.io.PrintWriter;
import java.lang.invoke.CallSite;
import java.lang.invoke.ConstantCallSite;
import java.lang.invoke.MethodHandles;
import java.lang.invoke.MethodType;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.IdentityHashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.TreeSet;
import java.util.function.Consumer;

import org.objectweb.asm.ClassWriter;
import org.objectweb.asm.Handle;
import org.objectweb.asm.MethodVisitor;
import org.objectweb.asm.Opcodes;
import org.objectweb.asm.Type;
import org.objectweb.asm.tree.AbstractInsnNode;
import org.objectweb.asm.tree.ClassNode;
import org.objectweb.asm.tree.InsnList;
import org.objectweb.asm.tree.InsnNode;
import org.objectweb.asm.tree.IntInsnNode;
import org.objectweb.asm.tree.LabelNode;
import org.objectweb.asm.tree.MethodInsnNode;
import org.objectweb.asm.tree.MethodNode;
import org.objectweb.asm.tree.TypeInsnNode;
import org.objectweb.asm.tree.VarInsnNode;
import org.objectweb.asm.tree.analysis.Analyzer;
import org.objectweb.asm.tree.analysis.BasicInterpreter;
import org.objectweb.asm.tree.analysis.BasicValue;
import org.objectweb.asm.tree.analysis.Frame;

public class JvmOpcodes implements Opcodes {
    static final String H_NAME = "JvmOpcodes$H";
    static final String OBJ_NAME = "JvmOpcodes$Obj";
    static final String S_NAME = "JvmOpcodes$S";
    static final String IFACE_NAME = "JvmOpcodes$Iface";
    /** Padding locals 0..7 hold ints, so a stray write to any of them shows. */
    static final int PAD_LOCALS = 8;
    static final String CALL_DESC = "(IJLjava/lang/Object;)I";

    // ---------------------------------------------------------------- runtime

    /** What `run()` reads its inputs from and writes its outputs to. */
    public static class H {
        public static long[] bits = new long[64];
        public static Object[] objs = new Object[64];
        /** Stack slots and locals after the instruction, by place. */
        public static TreeMap<String, String> out = new TreeMap<>();
        /** What a callee received, by place. */
        public static TreeMap<String, String> rec = new TreeMap<>();
        /** The stack slots that hold the low half of a long or double. */
        public static TreeSet<Integer> wide = new TreeSet<>();

        public static int gI(int id) { return (int) bits[id]; }
        public static float gF(int id) { return Float.intBitsToFloat((int) bits[id]); }
        public static long gJ(int id) { return (bits[id] & 0xffffffffL) | (bits[id + 1] << 32); }
        public static double gD(int id) { return Double.longBitsToDouble(gJ(id)); }
        public static Object gL(int id) { return objs[id]; }

        public static void pI(int v, int p) { out.put("out" + p, word(v)); }
        public static void pF(float v, int p) { pI(Float.floatToRawIntBits(v), p); }
        public static void pJ(long v, int p) {
            wide.add(p);
            out.put("out" + p, word((int) v));
            out.put("out" + (p + 1), word((int) (v >>> 32)));
        }
        public static void pD(double v, int p) { pJ(Double.doubleToRawLongBits(v), p); }
        public static void pL(Object v, int p) { out.put("out" + p, tag(v)); }

        public static void lI(int v, int n) { out.put("local" + n, word(v)); }
        public static void lF(float v, int n) { lI(Float.floatToRawIntBits(v), n); }
        public static void lJ(long v, int n) { out.put("local" + n, Long.toHexString(v)); }
        public static void lD(double v, int n) { lJ(Double.doubleToRawLongBits(v), n); }
        public static void lL(Object v, int n) { out.put("local" + n, tag(v)); }

        static void record(Object receiver, int a, long b, Object c) {
            if (receiver != null) rec.put("recv", tag(receiver));
            rec.put("arg0", word(a));
            rec.put("arg1", word((int) b));
            rec.put("arg2", word((int) (b >>> 32)));
            rec.put("arg3", tag(c));
        }

        public static int sta(int a, long b, Object c) {
            record(null, a, b, c);
            return 42;
        }

        public static CallSite bsm(MethodHandles.Lookup lookup, String name, MethodType type)
                throws Exception {
            return new ConstantCallSite(lookup.findStatic(H.class, "sta", type));
        }
    }

    public interface Iface {
        int itf(int a, long b, Object c);
    }

    /** The object domain: fields for getfield and putfield, methods to call. */
    public static class Obj implements Iface {
        public int i;
        public long j;
        public float f;
        public double d;
        public Object o;

        public Obj() {}

        public void init(int k) {
            i = 1000 + k;
            j = 0x100000000L * (k + 3) + 7 * k + 1;
            f = 0.5f + k;
            d = 0.25 + k;
            o = "o" + k;
        }

        public int vir(int a, long b, Object c) {
            H.record(this, a, b, c);
            return 43;
        }

        public int itf(int a, long b, Object c) {
            H.record(this, a, b, c);
            return 44;
        }

        String state() {
            return i + "," + j + "," + Float.floatToRawIntBits(f) + ","
                    + Double.doubleToRawLongBits(d) + "," + tag(o);
        }
    }

    /** Static fields for getstatic and putstatic. Reset before every run. */
    public static class S {
        public static int i;
        public static long j;
        public static Object o;
    }

    static String word(int v) {
        return Integer.toHexString(v);
    }

    /** The input objects of the current run, by identity: which slot and candidate. */
    static IdentityHashMap<Object, String> made = new IdentityHashMap<>();

    /** An object's identity if it is an input, or what it is if it is new. */
    static String tag(Object o) {
        if (o == null) return "null";
        String k = made.get(o);
        return k != null ? "#" + k : "new:" + describe(o);
    }

    static String describe(Object o) {
        if (o == null) return "null";
        if (o instanceof Obj) return "Obj{" + ((Obj) o).state() + "}";
        if (o instanceof int[]) return Arrays.toString((int[]) o);
        if (o instanceof long[]) return Arrays.toString((long[]) o);
        if (o instanceof float[]) return Arrays.toString((float[]) o);
        if (o instanceof double[]) return Arrays.toString((double[]) o);
        if (o instanceof boolean[]) return Arrays.toString((boolean[]) o);
        if (o instanceof byte[]) return Arrays.toString((byte[]) o);
        if (o instanceof char[]) return Arrays.toString((char[]) o);
        if (o instanceof short[]) return Arrays.toString((short[]) o);
        if (o instanceof Object[]) {
            StringBuilder sb = new StringBuilder("[");
            for (Object e : (Object[]) o) sb.append(tag(e)).append(';');
            return o.getClass().getName() + sb.append(']');
        }
        if (o instanceof Class) return "class " + ((Class<?>) o).getName();
        if (o instanceof Throwable) return o.getClass().getName() + ":" + ((Throwable) o).getMessage();
        return o.getClass().getName() + ":" + o;
    }

    /** The k'th object of a domain, made fresh for every run. */
    static Object make(String domain, int k, Class<?> self) throws Exception {
        int n = 3 + k;
        switch (domain) {
            case "obj": { Obj o = new Obj(); o.init(k); return o; }
            case "self": { Obj o = (Obj) self.getConstructor().newInstance(); o.init(k); return o; }
            case "str": return new String("str" + k);
            case "any":
                if (k == 0) return new String("any");
                if (k == 1) { Obj o = new Obj(); o.init(k); return o; }
                return Integer.valueOf(k);
            case "throwable": return new RuntimeException("t" + k);
            case "[I": { int[] a = new int[n]; for (int i = 0; i < n; i++) a[i] = 10 * k + i + 1; return a; }
            case "[J": { long[] a = new long[n]; for (int i = 0; i < n; i++) a[i] = 0x100000000L * (k + 1) + i; return a; }
            case "[F": { float[] a = new float[n]; for (int i = 0; i < n; i++) a[i] = k + i / 4f; return a; }
            case "[D": { double[] a = new double[n]; for (int i = 0; i < n; i++) a[i] = k + i / 8.0; return a; }
            case "[Z": { boolean[] a = new boolean[n]; for (int i = 0; i < n; i++) a[i] = (i + k) % 2 == 0; return a; }
            case "[B": { byte[] a = new byte[n]; for (int i = 0; i < n; i++) a[i] = (byte) (20 * k + i - 50); return a; }
            case "[C": { char[] a = new char[n]; for (int i = 0; i < n; i++) a[i] = (char) ('A' + 5 * k + i); return a; }
            case "[S": { short[] a = new short[n]; for (int i = 0; i < n; i++) a[i] = (short) (1000 * k + i - 2000); return a; }
            case "[O": { Object[] a = new Object[n]; for (int i = 0; i < n; i++) a[i] = "e" + k + i; return a; }
            default: throw new IllegalArgumentException("domain " + domain);
        }
    }

    /** The descriptor an object domain is cast to after loading. */
    static String domainType(String domain, String self) {
        switch (domain) {
            case "obj": return OBJ_NAME;
            case "self": return self;
            case "str": return "java/lang/String";
            case "any": return "java/lang/Object";
            case "throwable": return "java/lang/RuntimeException";
            case "[O": return "[Ljava/lang/Object;";
            default: return domain;
        }
    }

    // ------------------------------------------------------------------ cases

    /**
     * One case: the operand stack it needs (bottom first: I F J D, X for an
     * array index, L:<domain> for an object), locals beyond the int padding,
     * and the instruction under test.
     */
    static final class Case {
        final String name;
        final String[] stack;
        final Map<Integer, String> locals = new TreeMap<>();
        final Consumer<MethodVisitor> insn;
        Consumer<MethodVisitor> before = mv -> {};
        Consumer<MethodVisitor> after = mv -> {};
        /** Comparisons: every operand starts at the same value, so any change shows. */
        boolean equalInputs;
        /** Pad the constant pool past 256 entries, for ldc_w. */
        boolean bigPool;

        Case(String name, Consumer<MethodVisitor> insn, String... stack) {
            this.name = name;
            this.insn = insn;
            this.stack = stack;
        }

        Case local(int n, String type) {
            locals.put(n, type);
            return this;
        }

        Case before(Consumer<MethodVisitor> c) {
            before = c;
            return this;
        }

        Case after(Consumer<MethodVisitor> c) {
            after = c;
            return this;
        }

        Case equal() {
            equalInputs = true;
            return this;
        }
    }

    static final List<Case> CASES = new ArrayList<>();

    static Case add(String name, Consumer<MethodVisitor> insn, String... stack) {
        Case c = new Case(name, insn, stack);
        CASES.add(c);
        return c;
    }

    static Case op(String name, int opcode, String... stack) {
        return add(name, mv -> mv.visitInsn(opcode), stack);
    }

    static void cases() {
        // Constants.
        op("aconst_null", ACONST_NULL);
        String[] iconst = {"iconst_m1", "iconst_0", "iconst_1", "iconst_2", "iconst_3", "iconst_4", "iconst_5"};
        for (int i = 0; i < iconst.length; i++) op(iconst[i], ICONST_M1 + i);
        op("lconst_0", LCONST_0);
        op("lconst_1", LCONST_1);
        op("fconst_0", FCONST_0);
        op("fconst_1", FCONST_1);
        op("fconst_2", FCONST_2);
        op("dconst_0", DCONST_0);
        op("dconst_1", DCONST_1);
        add("bipush", mv -> mv.visitIntInsn(BIPUSH, -100));
        add("sipush", mv -> mv.visitIntInsn(SIPUSH, 30000));
        add("ldc int", mv -> mv.visitLdcInsn(123456));
        add("ldc float", mv -> mv.visitLdcInsn(2.5f));
        add("ldc string", mv -> mv.visitLdcInsn("s"));
        add("ldc class", mv -> mv.visitLdcInsn(Type.getType("Ljava/lang/String;")));
        add("ldc_w", mv -> mv.visitLdcInsn(654321)).bigPool = true;
        add("ldc2_w long", mv -> mv.visitLdcInsn(1L << 40));
        add("ldc2_w double", mv -> mv.visitLdcInsn(-0.5d));

        // Locals: the short forms (0-3), the plain form and the wide form.
        String[][] kinds = {{"i", "I"}, {"l", "J"}, {"f", "F"}, {"d", "D"}, {"a", "L:obj"}};
        int[] loads = {ILOAD, LLOAD, FLOAD, DLOAD, ALOAD};
        int[] stores = {ISTORE, LSTORE, FSTORE, DSTORE, ASTORE};
        for (int k = 0; k < kinds.length; k++) {
            for (int n : new int[] {0, 1, 2, 3, 5, 300}) {
                int load = loads[k], store = stores[k], local = n;
                String suffix = n < 4 ? "_" + n : n < 256 ? " " + n : " wide " + n;
                add(kinds[k][0] + "load" + suffix, mv -> mv.visitVarInsn(load, local)).local(n, kinds[k][1]);
                add(kinds[k][0] + "store" + suffix, mv -> mv.visitVarInsn(store, local), kinds[k][1]);
            }
        }
        add("iinc", mv -> mv.visitIincInsn(2, -5));
        add("iinc wide", mv -> mv.visitIincInsn(300, 1000)).local(300, "I");

        // Arrays.
        String[][] arrays = {
            {"i", "[I", "I"}, {"l", "[J", "J"}, {"f", "[F", "F"}, {"d", "[D", "D"},
            {"a", "[O", "L:str"}, {"b", "[B", "I"}, {"c", "[C", "I"}, {"s", "[S", "I"},
        };
        int[] aloads = {IALOAD, LALOAD, FALOAD, DALOAD, AALOAD, BALOAD, CALOAD, SALOAD};
        int[] astores = {IASTORE, LASTORE, FASTORE, DASTORE, AASTORE, BASTORE, CASTORE, SASTORE};
        for (int k = 0; k < arrays.length; k++) {
            String arr = "L:" + arrays[k][1];
            op(arrays[k][0] + "aload", aloads[k], arr, "X");
            op(arrays[k][0] + "astore", astores[k], arr, "X", arrays[k][2]);
        }
        op("baload boolean", BALOAD, "L:[Z", "X");
        op("bastore boolean", BASTORE, "L:[Z", "X", "I");
        int[] atypes = {T_BOOLEAN, T_CHAR, T_FLOAT, T_DOUBLE, T_BYTE, T_SHORT, T_INT, T_LONG};
        for (int t : atypes) add("newarray " + t, mv -> mv.visitIntInsn(NEWARRAY, t), "X");
        add("anewarray", mv -> mv.visitTypeInsn(ANEWARRAY, "java/lang/String"), "X");
        add("anewarray [I", mv -> mv.visitTypeInsn(ANEWARRAY, "[I"), "X");
        add("multianewarray", mv -> mv.visitMultiANewArrayInsn("[[I", 2), "X", "X");
        op("arraylength", ARRAYLENGTH, "L:[I");

        // The stack: every form of JVMS 6.5.
        op("nop", NOP);
        op("pop", POP, "I");
        op("pop2 ints", POP2, "I", "I");
        op("pop2 long", POP2, "J");
        op("dup", DUP, "I");
        op("dup ref", DUP, "L:obj");
        op("dup_x1", DUP_X1, "I", "I");
        op("dup_x2 ints", DUP_X2, "I", "I", "I");
        op("dup_x2 long", DUP_X2, "J", "I");
        op("dup2 ints", DUP2, "I", "I");
        op("dup2 long", DUP2, "J");
        op("dup2_x1 ints", DUP2_X1, "I", "I", "I");
        op("dup2_x1 long", DUP2_X1, "I", "J");
        op("dup2_x2 ints", DUP2_X2, "I", "I", "I", "I");
        op("dup2_x2 long over ints", DUP2_X2, "I", "I", "J");
        op("dup2_x2 ints over long", DUP2_X2, "J", "I", "I");
        op("dup2_x2 long over long", DUP2_X2, "J", "J");
        op("swap", SWAP, "I", "I");
        op("swap refs", SWAP, "L:obj", "L:str");

        // Arithmetic and logic.
        String[] ib = {"iadd", "isub", "imul", "idiv", "irem", "ishl", "ishr", "iushr", "iand", "ior", "ixor"};
        int[] ibo = {IADD, ISUB, IMUL, IDIV, IREM, ISHL, ISHR, IUSHR, IAND, IOR, IXOR};
        // Division and remainder hide changes to an operand much smaller than
        // the other, and the long logic ops changes to bits the other operand
        // masks; equal operands show every change.
        for (int i = 0; i < ib.length; i++) {
            Case c = op(ib[i], ibo[i], "I", "I");
            if (ib[i].endsWith("div") || ib[i].endsWith("rem")) c.equal();
        }
        String[] lb = {"ladd", "lsub", "lmul", "ldiv", "lrem", "land", "lor", "lxor"};
        int[] lbo = {LADD, LSUB, LMUL, LDIV, LREM, LAND, LOR, LXOR};
        for (int i = 0; i < lb.length; i++) {
            Case c = op(lb[i], lbo[i], "J", "J");
            if (!lb[i].equals("ladd") && !lb[i].equals("lsub") && !lb[i].equals("lmul")) c.equal();
        }
        op("lshl", LSHL, "J", "I");
        op("lshr", LSHR, "J", "I");
        op("lushr", LUSHR, "J", "I");
        String[] fb = {"fadd", "fsub", "fmul", "fdiv", "frem"};
        int[] fbo = {FADD, FSUB, FMUL, FDIV, FREM};
        for (int i = 0; i < fb.length; i++) {
            Case c = op(fb[i], fbo[i], "F", "F");
            if (fb[i].endsWith("div") || fb[i].endsWith("rem")) c.equal();
        }
        String[] db = {"dadd", "dsub", "dmul", "ddiv", "drem"};
        int[] dbo = {DADD, DSUB, DMUL, DDIV, DREM};
        for (int i = 0; i < db.length; i++) {
            Case c = op(db[i], dbo[i], "D", "D");
            if (db[i].endsWith("div") || db[i].endsWith("rem")) c.equal();
        }
        op("ineg", INEG, "I");
        op("lneg", LNEG, "J");
        op("fneg", FNEG, "F");
        op("dneg", DNEG, "D");
        Object[][] conv = {
            {"i2l", I2L, "I"}, {"i2f", I2F, "I"}, {"i2d", I2D, "I"},
            {"l2i", L2I, "J"}, {"l2f", L2F, "J"}, {"l2d", L2D, "J"},
            {"f2i", F2I, "F"}, {"f2l", F2L, "F"}, {"f2d", F2D, "F"},
            {"d2i", D2I, "D"}, {"d2l", D2L, "D"}, {"d2f", D2F, "D"},
            {"i2b", I2B, "I"}, {"i2c", I2C, "I"}, {"i2s", I2S, "I"},
        };
        for (Object[] c : conv) op((String) c[0], (Integer) c[1], (String) c[2]);
        op("lcmp", LCMP, "J", "J").equal();
        op("fcmpl", FCMPL, "F", "F").equal();
        op("fcmpg", FCMPG, "F", "F").equal();
        op("dcmpl", DCMPL, "D", "D").equal();
        op("dcmpg", DCMPG, "D", "D").equal();

        // Fields.
        add("getstatic", mv -> mv.visitFieldInsn(GETSTATIC, S_NAME, "i", "I"));
        add("getstatic long", mv -> mv.visitFieldInsn(GETSTATIC, S_NAME, "j", "J"));
        add("putstatic", mv -> mv.visitFieldInsn(PUTSTATIC, S_NAME, "i", "I"), "I");
        add("putstatic long", mv -> mv.visitFieldInsn(PUTSTATIC, S_NAME, "j", "J"), "J");
        add("putstatic ref", mv -> mv.visitFieldInsn(PUTSTATIC, S_NAME, "o", "Ljava/lang/Object;"), "L:str");
        add("getfield", mv -> mv.visitFieldInsn(GETFIELD, OBJ_NAME, "i", "I"), "L:obj");
        add("getfield long", mv -> mv.visitFieldInsn(GETFIELD, OBJ_NAME, "j", "J"), "L:obj");
        add("getfield double", mv -> mv.visitFieldInsn(GETFIELD, OBJ_NAME, "d", "D"), "L:obj");
        add("putfield", mv -> mv.visitFieldInsn(PUTFIELD, OBJ_NAME, "i", "I"), "L:obj", "I");
        add("putfield long", mv -> mv.visitFieldInsn(PUTFIELD, OBJ_NAME, "j", "J"), "L:obj", "J");
        add("putfield float", mv -> mv.visitFieldInsn(PUTFIELD, OBJ_NAME, "f", "F"), "L:obj", "F");
        add("putfield ref", mv -> mv.visitFieldInsn(PUTFIELD, OBJ_NAME, "o", "Ljava/lang/Object;"), "L:obj", "L:str");

        // Calls. Every callee takes (int, long, Object) and returns a constant.
        add("invokestatic", mv -> mv.visitMethodInsn(INVOKESTATIC, H_NAME, "sta", CALL_DESC, false),
                "I", "J", "L:str");
        add("invokevirtual", mv -> mv.visitMethodInsn(INVOKEVIRTUAL, OBJ_NAME, "vir", CALL_DESC, false),
                "L:obj", "I", "J", "L:str");
        add("invokespecial", mv -> mv.visitMethodInsn(INVOKESPECIAL, OBJ_NAME, "vir", CALL_DESC, false),
                "L:self", "I", "J", "L:str");
        add("invokeinterface", mv -> mv.visitMethodInsn(INVOKEINTERFACE, IFACE_NAME, "itf", CALL_DESC, true),
                "L:obj", "I", "J", "L:str");
        Handle bsm = new Handle(H_INVOKESTATIC, H_NAME, "bsm",
                "(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/invoke/MethodType;)Ljava/lang/invoke/CallSite;",
                false);
        add("invokedynamic", mv -> mv.visitInvokeDynamicInsn("sta", CALL_DESC, bsm), "I", "J", "L:str");

        // Objects.
        add("new", mv -> mv.visitTypeInsn(NEW, OBJ_NAME)).after(mv -> {
            mv.visitInsn(DUP);
            mv.visitMethodInsn(INVOKESPECIAL, OBJ_NAME, "<init>", "()V", false);
        });
        add("checkcast", mv -> mv.visitTypeInsn(CHECKCAST, "java/lang/String"), "L:str");
        add("instanceof", mv -> mv.visitTypeInsn(INSTANCEOF, "java/lang/String"), "L:any");
        op("athrow", ATHROW, "L:throwable");
        // Monitors take a copy of the object, so the lock can be released after.
        op("monitorenter", MONITORENTER, "L:obj").before(mv -> mv.visitInsn(DUP)).after(mv -> {
            mv.visitInsn(DUP);
            mv.visitInsn(MONITOREXIT);
        });
        op("monitorexit", MONITOREXIT, "L:obj").before(mv -> {
            mv.visitInsn(DUP);
            mv.visitInsn(MONITORENTER);
            mv.visitInsn(DUP);
        });
    }

    // ------------------------------------------------------------- generation

    /** One 32-bit input slot of a case. */
    static final class Slot {
        String atom; // in<p> or local<n>
        int id; // index into H.bits / H.objs
        char kind; // I F J D (lo or hi half) or L
        boolean hi;
        long[] cands; // word values, or object indices for L
        String domain;
    }

    static final class Built {
        Case c;
        String cls;
        byte[] bytes;
        int index;
        int height;
        int opcode;
        List<Slot> slots = new ArrayList<>();
    }

    static final String[] DESC = {"I", "F", "J", "D"};

    static long[] intCands(int p, boolean equal) {
        int base = equal ? 1000003 : 1000003 * (p + 1);
        return new long[] {base, base + 1, -base, 1, 3, Integer.MAX_VALUE};
    }

    static long[][] wideCands(int p, boolean equal, boolean dbl) {
        if (dbl) {
            long bits = Double.doubleToRawLongBits(equal ? 1.5 : 1.5 + p);
            long lo = bits & 0xffffffffL, hi = bits >>> 32;
            // The low word's top bits survive d2f; the high word's flips change the value a lot.
            return new long[][] {{lo, lo ^ 0xe0000000L, lo ^ 0x20000001L}, {hi, hi ^ 0x00080000L, hi ^ 0x80000000L}};
        }
        long lo = equal ? 0x01000003L : 0x01000003L * (p + 1), hi = equal ? 5 : 5 + p;
        return new long[][] {{lo, lo + 1, ~lo & 0xffffffffL, 0, 1}, {hi, hi + 1, hi ^ 0x80000000L, 0}};
    }

    static long[] floatCands(int p, boolean equal) {
        float base = equal ? 1.5f : 1.5f + p;
        return new long[] {Float.floatToRawIntBits(base), Float.floatToRawIntBits(base + 0.25f),
            Float.floatToRawIntBits(-base), Float.floatToRawIntBits(0.75f), Float.floatToRawIntBits(1e9f)};
    }

    /** Push the input value of `type` whose first slot is `id`, as `run()` reads it. */
    static void load(InsnList il, String type, int id, String self) {
        il.add(new IntInsnNode(SIPUSH, id));
        char t = type.charAt(0);
        switch (t) {
            case 'I': case 'X':
                il.add(new MethodInsnNode(INVOKESTATIC, H_NAME, "gI", "(I)I", false));
                break;
            case 'F': case 'J': case 'D':
                il.add(new MethodInsnNode(INVOKESTATIC, H_NAME, "g" + t, "(I)" + t, false));
                break;
            case 'L':
                il.add(new MethodInsnNode(INVOKESTATIC, H_NAME, "gL", "(I)Ljava/lang/Object;", false));
                il.add(new TypeInsnNode(CHECKCAST, domainType(type.substring(2), self)));
                break;
            default:
                throw new IllegalArgumentException(type);
        }
    }

    static int width(String type) {
        return type.equals("J") || type.equals("D") ? 2 : 1;
    }

    /** Add the input slots for one value of `type`, named from `atom` and `pos`. */
    static int addSlots(Built b, String prefix, int pos, String type, int id, boolean equal) {
        char t = type.charAt(0);
        if (t == 'J' || t == 'D') {
            long[][] c = wideCands(pos, equal, t == 'D');
            for (int h = 0; h < 2; h++) {
                Slot s = new Slot();
                s.atom = prefix + (pos + h);
                s.id = id + h;
                s.kind = t;
                s.hi = h == 1;
                s.cands = c[h];
                b.slots.add(s);
            }
            return 2;
        }
        Slot s = new Slot();
        s.atom = prefix + pos;
        s.id = id;
        s.kind = t == 'X' ? 'I' : t;
        if (t == 'I') s.cands = intCands(pos, equal);
        else if (t == 'X') s.cands = new long[] {1, 0, 2};
        else if (t == 'F') s.cands = floatCands(pos, equal);
        else {
            s.domain = type.substring(2);
            s.cands = new long[] {0, 1, 2};
        }
        b.slots.add(s);
        return 1;
    }

    static Built build(Case c, String cls) throws Exception {
        Built b = new Built();
        b.c = c;
        b.cls = cls;

        MethodNode mn = new MethodNode(ACC_PUBLIC | ACC_STATIC, "run", "()V", null, null);
        InsnList il = mn.instructions;
        int id = 0;

        // Locals: int padding, then the case's own.
        TreeMap<Integer, String> locals = new TreeMap<>();
        for (int n = 0; n < PAD_LOCALS; n++) locals.put(n, "I");
        for (Map.Entry<Integer, String> e : c.locals.entrySet()) {
            locals.put(e.getKey(), e.getValue());
            if (width(e.getValue()) == 2) locals.remove(e.getKey() + 1);
        }
        for (Map.Entry<Integer, String> e : locals.entrySet()) {
            int n = e.getKey();
            String type = e.getValue();
            load(il, type, id, cls);
            int store = "IXFJDL".indexOf(type.charAt(0));
            il.add(new VarInsnNode(new int[] {ISTORE, ISTORE, FSTORE, LSTORE, DSTORE, ASTORE}[store], n));
            // jvm-reader names a wide local by its first slot, so both input
            // slots of one carry that name.
            int w = addSlots(b, "local", n, type, id, false);
            if (w == 2) b.slots.get(b.slots.size() - 1).atom = "local" + n;
            id += w;
        }
        int localSlots = b.slots.size();

        // The stack: one int of padding, then the operands.
        List<String> stack = new ArrayList<>();
        stack.add("I");
        stack.addAll(Arrays.asList(c.stack));
        int pos = 0;
        for (String type : stack) {
            load(il, type, id, cls);
            int w = addSlots(b, "in", pos, type, id, c.equalInputs && pos > 0);
            id += w;
            pos += w;
        }

        // The instruction under test, between whatever setup and cleanup it needs.
        MethodNode tmp = new MethodNode(ACC_PUBLIC | ACC_STATIC, "run", "()V", null, null);
        c.before.accept(tmp);
        LabelNode mark = new LabelNode();
        tmp.instructions.add(mark);
        c.insn.accept(tmp);
        c.after.accept(tmp);
        il.add(tmp.instructions);
        int index = 0;
        for (AbstractInsnNode n = il.getFirst(); n != mark; n = n.getNext()) {
            if (n.getOpcode() >= 0) index++;
        }
        b.index = index;
        AbstractInsnNode tested = mark.getNext();
        while (tested.getOpcode() < 0) tested = tested.getNext();
        b.opcode = tested.getOpcode();

        // Capture: what ASM says the stack and locals hold now, top of stack first.
        mn.maxStack = 64;
        mn.maxLocals = 302; // the wide cases use local 300
        InsnNode ret = new InsnNode(RETURN);
        il.add(ret);
        Frame<BasicValue>[] frames = new Analyzer<>(new BasicInterpreter()).analyze(cls, mn);
        // Nothing to capture after athrow: the return is unreachable.
        Frame<BasicValue> f = frames[il.indexOf(ret)];
        InsnList capture = new InsnList();
        if (f != null) {
            int p = 0;
            for (int k = 0; k < f.getStackSize(); k++) p += f.getStack(k).getSize();
            for (int k = f.getStackSize() - 1; k >= 0; k--) {
                BasicValue v = f.getStack(k);
                p -= v.getSize();
                capture.add(new IntInsnNode(SIPUSH, p));
                capture.add(new MethodInsnNode(INVOKESTATIC, H_NAME, "p" + letter(v), "(" + desc(v) + "I)V", false));
            }
            for (int n = 0; n < f.getLocals(); n++) {
                BasicValue v = f.getLocal(n);
                if (v == BasicValue.UNINITIALIZED_VALUE || v.getType() == null) continue;
                capture.add(new VarInsnNode(Type.getType(desc(v)).getOpcode(ILOAD), n));
                capture.add(new IntInsnNode(SIPUSH, n));
                capture.add(new MethodInsnNode(INVOKESTATIC, H_NAME, "l" + letter(v), "(" + desc(v) + "I)V", false));
            }
        }
        il.insertBefore(ret, capture);
        Frame<BasicValue> at = frames[il.indexOf(tested)];
        b.height = 0;
        for (int k = 0; k < at.getStackSize(); k++) b.height += at.getStack(k).getSize();

        ClassNode cn = new ClassNode();
        cn.version = V1_8;
        cn.access = ACC_PUBLIC | ACC_SUPER;
        cn.name = cls;
        cn.superName = OBJ_NAME;
        cn.methods.add(mn);
        MethodNode init = new MethodNode(ACC_PUBLIC, "<init>", "()V", null, null);
        init.instructions.add(new VarInsnNode(ALOAD, 0));
        init.instructions.add(new MethodInsnNode(INVOKESPECIAL, OBJ_NAME, "<init>", "()V", false));
        init.instructions.add(new InsnNode(RETURN));
        cn.methods.add(init);
        ClassWriter cw = new ClassWriter(ClassWriter.COMPUTE_MAXS | ClassWriter.COMPUTE_FRAMES) {
            @Override
            protected String getCommonSuperClass(String a, String b2) {
                return "java/lang/Object";
            }
        };
        if (c.bigPool) for (int i = 0; i < 300; i++) cw.newConst(2000000 + i);
        cn.accept(cw);
        b.bytes = cw.toByteArray();
        return b;
    }

    static char letter(BasicValue v) {
        if (v == BasicValue.INT_VALUE) return 'I';
        if (v == BasicValue.FLOAT_VALUE) return 'F';
        if (v == BasicValue.LONG_VALUE) return 'J';
        if (v == BasicValue.DOUBLE_VALUE) return 'D';
        return 'L';
    }

    static String desc(BasicValue v) {
        char l = letter(v);
        return l == 'L' ? "Ljava/lang/Object;" : String.valueOf(l);
    }

    // ------------------------------------------------------------------ runs

    static final class Loader extends ClassLoader {
        Loader() {
            super(JvmOpcodes.class.getClassLoader());
        }

        Class<?> define(String name, byte[] b) {
            return defineClass(name, b, 0, b.length);
        }
    }

    /** One run's observation: every place's value. */
    static final class Run {
        TreeMap<String, String> places = new TreeMap<>();
        TreeSet<Integer> wide = new TreeSet<>();
        Map<String, String> before = new LinkedHashMap<>();
        Throwable thrown;
    }

    static Run run(Built b, Class<?> cls, Method m, Map<Slot, Integer> choice) throws Exception {
        H.out.clear();
        H.rec.clear();
        H.wide.clear();
        S.i = 0;
        S.j = 0;
        S.o = null;
        made.clear();
        Arrays.fill(H.bits, 0);
        Arrays.fill(H.objs, null);
        Run r = new Run();
        Map<String, Object> inputs = new LinkedHashMap<>();
        Map<String, String> inputState = new LinkedHashMap<>();
        for (Slot s : b.slots) {
            int k = choice.getOrDefault(s, 0);
            if (s.kind == 'L') {
                Object o = make(s.domain, k, cls);
                made.put(o, s.atom + "." + k);
                H.objs[s.id] = o;
                inputs.put(s.atom, o);
                r.before.put(s.atom, tag(o));
            } else {
                H.bits[s.id] = s.cands[k];
                r.before.put(s.atom, word((int) s.cands[k]));
            }
        }
        // A wide local is captured whole, under its first slot's name.
        for (Slot s : b.slots) {
            if (s.hi && s.atom.startsWith("local")) {
                long v = (H.bits[s.id - 1] & 0xffffffffL) | (H.bits[s.id] << 32);
                r.before.put(s.atom, Long.toHexString(v));
            }
        }
        for (Map.Entry<String, Object> e : inputs.entrySet()) inputState.put(e.getKey(), state(e.getValue()));

        try {
            m.invoke(null);
        } catch (InvocationTargetException e) {
            r.thrown = e.getCause();
        }
        if (r.thrown != null) {
            r.places.put("thrown", tag(r.thrown));
            return r;
        }
        r.places.putAll(H.out);
        r.wide.addAll(H.wide);
        r.places.putAll(H.rec);
        if (S.i != 0) r.places.put("field:" + S_NAME + ".i", word(S.i));
        if (S.j != 0) r.places.put("field:" + S_NAME + ".j", Long.toHexString(S.j));
        if (S.o != null) r.places.put("field:" + S_NAME + ".o", tag(S.o));
        for (Map.Entry<String, Object> e : inputs.entrySet()) {
            Object o = e.getValue();
            if (state(o).equals(inputState.get(e.getKey()))) continue;
            if (o instanceof Obj) {
                String[] was = inputState.get(e.getKey()).split(",");
                String[] now = state(o).split(",");
                String[] names = {"i", "j", "f", "d", "o"};
                for (int i = 0; i < names.length; i++) {
                    if (!was[i].equals(now[i])) {
                        r.places.put("field:" + OBJ_NAME + "." + names[i] + "@" + e.getKey(), now[i]);
                    }
                }
            } else {
                r.places.put("elem:" + e.getKey(), describe(o));
            }
        }
        return r;
    }

    static String state(Object o) {
        return o instanceof Obj ? ((Obj) o).state() : describe(o);
    }

    static void report(Built b, Class<?> cls, PrintWriter out) throws Exception {
        out.println("case " + b.c.name.replace(' ', '_') + " " + b.cls + " op=" + b.opcode
                + " index=" + b.index + " h=" + b.height);
        Method m = cls.getMethod("run");
        Run base = run(b, cls, m, new LinkedHashMap<>());
        boolean athrow = b.opcode == ATHROW;
        if (base.thrown != null && !athrow) {
            out.println("error baseline threw " + describe(base.thrown));
        }
        TreeMap<String, TreeSet<String>> influence = new TreeMap<>();
        TreeSet<String> places = new TreeSet<>(base.places.keySet());
        for (Slot s : b.slots) {
            for (int k = 1; k < s.cands.length; k++) {
                Map<Slot, Integer> choice = new LinkedHashMap<>();
                choice.put(s, k);
                Run r = run(b, cls, m, choice);
                if (r.thrown != null && !athrow) {
                    out.println("error threw " + describe(r.thrown) + " with " + s.atom + " candidate " + k);
                    continue;
                }
                places.addAll(r.places.keySet());
                for (String place : places) {
                    String was = base.places.getOrDefault(place, "-");
                    String now = r.places.getOrDefault(place, "-");
                    if (!was.equals(now)) {
                        influence.computeIfAbsent(place, x -> new TreeSet<>()).add(s.atom);
                    }
                }
            }
        }
        // A long or double is one value: what changes either half changes it.
        TreeSet<String> changedWide = new TreeSet<>();
        for (int p : base.wide) {
            String lo = "out" + p, hi = "out" + (p + 1);
            TreeSet<String> both = new TreeSet<>(influence.getOrDefault(lo, new TreeSet<>()));
            both.addAll(influence.getOrDefault(hi, new TreeSet<>()));
            influence.put(lo, both);
            influence.put(hi, new TreeSet<>(both));
            if (changed(base, lo) || changed(base, hi)) {
                changedWide.add(lo);
                changedWide.add(hi);
            }
        }
        for (String place : places) {
            boolean changed = changedWide.contains(place) || changed(base, place);
            out.println("place " + place + " changed=" + (changed ? 1 : 0) + " influence="
                    + String.join(",", influence.getOrDefault(place, new TreeSet<>())));
        }
        out.println("end");
    }

    /** Whether the baseline run wrote `place`. */
    static boolean changed(Run base, String place) {
        String now = base.places.get(place);
        if (now == null) return false;
        if (place.startsWith("out") && base.before.containsKey("in" + place.substring(3))) {
            return !now.equals(base.before.get("in" + place.substring(3)));
        }
        if (place.startsWith("local")) return !now.equals(base.before.get(place));
        return true;
    }

    public static void main(String[] args) throws Exception {
        Path dir = Paths.get(args[0]);
        Path classes = dir.resolve("classes");
        Files.createDirectories(classes);
        cases();
        List<Built> built = new ArrayList<>();
        for (int i = 0; i < CASES.size(); i++) {
            Built b = build(CASES.get(i), "Case_" + i);
            Files.write(classes.resolve(b.cls + ".class"), b.bytes);
            built.add(b);
        }
        Loader loader = new Loader();
        try (PrintWriter out = new PrintWriter(dir.resolve("results.txt").toFile(), "UTF-8")) {
            for (Built b : built) {
                try {
                    report(b, loader.define(b.cls, b.bytes), out);
                } catch (Throwable t) {
                    out.println("case " + b.c.name.replace(' ', '_') + " " + b.cls + " op=" + b.opcode
                            + " index=" + b.index + " h=" + b.height);
                    out.println("error " + t.toString().replace('\n', ' '));
                    out.println("end");
                }
                out.flush();
            }
        }
    }
}
