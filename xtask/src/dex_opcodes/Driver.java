// Runs every `cargo xtask dex-opcodes` case on ART and reports what each one's
// instruction under test reads, writes and throws (see mod.rs).
//
//   dalvikvm64 -Xint -cp driver.dex:cases.dex Driver <manifest> <results>
//
// A case's inputs are 32-bit register slots. A wide value is two slots, low
// then high, and each half is perturbed on its own. A run sets every slot,
// calls the case's `run()`, and reads back its observation: the registers
// after the instruction, the return value or exception, the callee record, the
// static fields, and the contents of every input object (for stores into the
// heap). The driver makes a baseline run, one run per alternative value of
// each slot, and one run per fault assignment, and reports:
//
//   written <regs>     slots whose value after differs from before, in some run
//                      that did not throw
//   influence <regs>   slots whose perturbation changed the observation
//   threw <classes>    exceptions from baseline and perturbed runs
//   fault <i> <class>  the exception each fault assignment raised, or none

import java.io.BufferedReader;
import java.io.FileReader;
import java.io.PrintWriter;
import java.lang.invoke.MethodHandle;
import java.lang.invoke.MethodHandles;
import java.lang.invoke.MethodType;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.IdentityHashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.TreeSet;

public class Driver {
    /** One 32-bit input slot: an int-like value, or an object chosen from a domain. */
    static class Slot {
        int reg;
        char kind; // I F J D (lo half, or hi half when `hi`) L
        boolean hi;
        long[] cands; // int bits for prim slots; domain indices for L
        String domain;
    }

    static class Out {
        int reg;
        char type; // I F J D L
    }

    static class Case {
        String cls;
        char ret;
        List<Slot> slots = new ArrayList<>();
        List<Out> outs = new ArrayList<>();
        List<Map<Integer, String>> faults = new ArrayList<>();
    }

    /** The objects made for the current run, named by register and domain index. */
    static IdentityHashMap<Object, String> made = new IdentityHashMap<>();

    public static void main(String[] args) throws Exception {
        List<Case> cases = readManifest(args[0]);
        try (PrintWriter out = new PrintWriter(args[1], "UTF-8")) {
            for (Case c : cases) {
                out.println("case " + c.cls);
                try {
                    runCase(c, out);
                } catch (Throwable t) {
                    out.println("error " + oneLine(t));
                }
                out.println("end");
                out.flush();
            }
        }
    }

    static String oneLine(Throwable t) {
        StringBuilder sb = new StringBuilder();
        for (Throwable x = t; x != null; x = x.getCause()) {
            if (sb.length() > 0) sb.append(" <- ");
            sb.append(x.getClass().getName()).append(": ").append(x.getMessage());
        }
        return sb.toString().replace('\n', ' ');
    }

    static List<Case> readManifest(String path) throws Exception {
        List<Case> cases = new ArrayList<>();
        Case cur = null;
        try (BufferedReader r = new BufferedReader(new FileReader(path))) {
            for (String line; (line = r.readLine()) != null; ) {
                String[] w = line.trim().split(" ");
                switch (w[0]) {
                    case "case":
                        cur = new Case();
                        cur.cls = w[1];
                        cur.ret = w[2].charAt(0);
                        break;
                    case "in": {
                        int reg = Integer.parseInt(w[1]);
                        char kind = w[2].charAt(0);
                        if (kind == 'L') {
                            Slot s = slot(reg, kind, false);
                            s.domain = w[3];
                            int n = Integer.parseInt(w[4]);
                            s.cands = new long[n];
                            for (int i = 0; i < n; i++) s.cands[i] = i;
                            cur.slots.add(s);
                        } else if (kind == 'J' || kind == 'D') {
                            Slot lo = slot(reg, kind, false);
                            lo.cands = longs(w[3]);
                            Slot hi = slot(reg + 1, kind, true);
                            hi.cands = longs(w[4]);
                            cur.slots.add(lo);
                            cur.slots.add(hi);
                        } else {
                            Slot s = slot(reg, kind, false);
                            s.cands = longs(w[3]);
                            cur.slots.add(s);
                        }
                        break;
                    }
                    case "out": {
                        Out o = new Out();
                        o.reg = Integer.parseInt(w[1]);
                        o.type = w[2].charAt(0);
                        cur.outs.add(o);
                        break;
                    }
                    case "fault": {
                        Map<Integer, String> f = new LinkedHashMap<>();
                        for (int i = 1; i < w.length; i++) {
                            String[] kv = w[i].split("=");
                            f.put(Integer.parseInt(kv[0]), kv[1]);
                        }
                        cur.faults.add(f);
                        break;
                    }
                    case "end":
                        cases.add(cur);
                        cur = null;
                        break;
                    default:
                        if (!w[0].isEmpty()) throw new IllegalArgumentException(line);
                }
            }
        }
        return cases;
    }

    static Slot slot(int reg, char kind, boolean hi) {
        Slot s = new Slot();
        s.reg = reg;
        s.kind = kind;
        s.hi = hi;
        return s;
    }

    static long[] longs(String csv) {
        String[] p = csv.split(",");
        long[] v = new long[p.length];
        for (int i = 0; i < p.length; i++) v[i] = Long.parseLong(p[i]);
        return v;
    }

    /** What one run saw: per-slot values before and after, and the whole observation. */
    static class Run {
        Map<Integer, String> before = new LinkedHashMap<>();
        Map<Integer, String> after = new LinkedHashMap<>();
        String observation;
        Throwable thrown;
    }

    static void runCase(Case c, PrintWriter out) throws Exception {
        Method run = Class.forName(c.cls).getMethod("run");
        Map<Integer, String> base = new LinkedHashMap<>();
        for (Slot s : c.slots) base.put(s.reg, Long.toString(s.cands[0]));

        Run baseline = run(c, run, base);
        out.println("baseline " + baseline.observation);
        TreeSet<Integer> written = new TreeSet<>();
        TreeSet<Integer> influence = new TreeSet<>();
        TreeSet<String> threw = new TreeSet<>();
        noteRun(baseline, written, threw);
        for (Slot s : c.slots) {
            for (int i = 1; i < s.cands.length; i++) {
                Map<Integer, String> in = new LinkedHashMap<>(base);
                in.put(s.reg, Long.toString(s.cands[i]));
                Run r = run(c, run, in);
                noteRun(r, written, threw);
                if (!r.observation.equals(baseline.observation)) influence.add(s.reg);
            }
        }
        out.println("written " + join(written));
        out.println("influence " + join(influence));
        out.println("threw " + String.join(",", threw));
        for (int i = 0; i < c.faults.size(); i++) {
            Map<Integer, String> in = new LinkedHashMap<>(base);
            in.putAll(c.faults.get(i));
            Run r = run(c, run, in);
            out.println("fault " + i + " " + (r.thrown == null ? "none" : r.thrown.getClass().getName()));
        }
    }

    static void noteRun(Run r, TreeSet<Integer> written, TreeSet<String> threw) {
        if (r.thrown != null) {
            threw.add(r.thrown.getClass().getName());
            return;
        }
        for (Map.Entry<Integer, String> e : r.after.entrySet()) {
            if (!e.getValue().equals(r.before.get(e.getKey()))) written.add(e.getKey());
        }
    }

    static String join(TreeSet<Integer> s) {
        StringBuilder sb = new StringBuilder();
        for (int x : s) {
            if (sb.length() > 0) sb.append(',');
            sb.append(x);
        }
        return sb.toString();
    }

    /** One execution of `run()` with slot values `in` (int bits, a domain index, or "null"). */
    static Run run(Case c, Method method, Map<Integer, String> in) throws Exception {
        resetH();
        H.S.reset();
        made.clear();
        Run r = new Run();
        List<Object> inputObjects = new ArrayList<>();
        List<String> inputBefore = new ArrayList<>();
        Map<Integer, Long> bits = new LinkedHashMap<>();
        for (Slot s : c.slots) {
            String v = in.get(s.reg);
            if (s.kind == 'L') {
                Object o = v.equals("null") ? null : make(s.domain, Integer.parseInt(v), c.cls, s.reg);
                setH("iL" + s.reg, o);
                inputObjects.add(o);
                inputBefore.add(describe(o));
                r.before.put(s.reg, tag(o));
            } else {
                long b = Long.parseLong(v) & 0xffffffffL;
                bits.put(s.reg, b);
                r.before.put(s.reg, Long.toString(b));
            }
        }
        for (Slot s : c.slots) {
            if (s.hi || s.kind == 'L') continue;
            long lo = bits.get(s.reg);
            switch (s.kind) {
                case 'I': setH("iI" + s.reg, (int) lo); break;
                case 'F': setH("iF" + s.reg, Float.intBitsToFloat((int) lo)); break;
                case 'J': setH("iJ" + s.reg, lo | (bits.get(s.reg + 1) << 32)); break;
                case 'D':
                    setH("iD" + s.reg, Double.longBitsToDouble(lo | (bits.get(s.reg + 1) << 32)));
                    break;
                default: throw new IllegalStateException("slot kind " + s.kind);
            }
        }

        StringBuilder obs = new StringBuilder();
        Map<Integer, Object> objects = new LinkedHashMap<>();
        Object ret = null;
        try {
            ret = method.invoke(null);
        } catch (InvocationTargetException e) {
            r.thrown = e.getCause();
        }
        if (r.thrown != null) {
            obs.append("exc=").append(r.thrown.getClass().getName()).append(':')
                    .append(describe(r.thrown)).append(' ');
        } else if (c.ret != 'V') {
            obs.append("ret=").append(describe(ret)).append(' ');
        } else {
            for (Out o : c.outs) {
                switch (o.type) {
                    case 'I': {
                        int v = (Integer) getH("oI" + o.reg);
                        r.after.put(o.reg, Long.toString(v & 0xffffffffL));
                        break;
                    }
                    case 'F': {
                        int v = Float.floatToRawIntBits((Float) getH("oF" + o.reg));
                        r.after.put(o.reg, Long.toString(v & 0xffffffffL));
                        break;
                    }
                    case 'J':
                    case 'D': {
                        long v = o.type == 'J' ? (Long) getH("oJ" + o.reg)
                                : Double.doubleToRawLongBits((Double) getH("oD" + o.reg));
                        r.after.put(o.reg, Long.toString(v & 0xffffffffL));
                        r.after.put(o.reg + 1, Long.toString(v >>> 32));
                        break;
                    }
                    case 'L': {
                        Object v = getH("oL" + o.reg);
                        r.after.put(o.reg, tag(v));
                        objects.put(o.reg, v);
                        break;
                    }
                    default: throw new IllegalStateException("out type " + o.type);
                }
            }
            // Only what the instruction changed: a register that still holds its input is
            // the same in every run, whatever that input was.
            for (Map.Entry<Integer, String> e : r.after.entrySet()) {
                int reg = e.getKey();
                if (e.getValue().equals(r.before.get(reg))) continue;
                String v = objects.containsKey(reg) ? describe(objects.get(reg)) : e.getValue();
                obs.append('v').append(reg).append('=').append(v).append(' ');
            }
        }
        obs.append("rec=").append(H.rec).append(" S=").append(H.S.snapshot()).append(" in=");
        // Only the input objects the instruction changed (stores into the heap).
        for (int i = 0; i < inputObjects.size(); i++) {
            String after = describe(inputObjects.get(i));
            if (!after.equals(inputBefore.get(i))) obs.append(after).append(';');
        }
        r.observation = obs.toString();
        return r;
    }

    static void resetH() throws Exception {
        for (java.lang.reflect.Field f : H.class.getFields()) {
            if (java.lang.reflect.Modifier.isFinal(f.getModifiers())) continue;
            Class<?> t = f.getType();
            if (t == int.class) f.setInt(null, 0);
            else if (t == long.class) f.setLong(null, 0);
            else if (t == float.class) f.setFloat(null, 0);
            else if (t == double.class) f.setDouble(null, 0);
            else f.set(null, null);
        }
        H.retI = 0x5e5e5e;
        H.retJ = 0x0123456789L;
        H.retL = "returned";
        H.exc = new IllegalArgumentException("caught");
    }

    static void setH(String name, Object v) throws Exception {
        H.class.getField(name).set(null, v);
    }

    static Object getH(String name) throws Exception {
        return H.class.getField(name).get(null);
    }

    /**
     * The k'th object of a domain, made fresh for each run for register `reg`. Arrays have length
     * 3 + k, so array-length depends on which one; k == 9 is a length-1 array.
     */
    static Object make(String domain, int k, String caseClass, int reg) throws Exception {
        Object o = makeRaw(domain, k, caseClass);
        made.put(o, reg + "." + k);
        return o;
    }

    static Object makeRaw(String domain, int k, String caseClass) throws Exception {
        int n = k == 9 ? 1 : 3 + k;
        switch (domain) {
            case "obj": return new H.Obj(k);
            case "self": {
                H.Obj o = (H.Obj) Class.forName(caseClass).getConstructor().newInstance();
                o.init(k);
                return o;
            }
            case "str": return new String("str" + k);
            case "integer": return new Integer(100 + k);
            case "any":
                if (k % 3 == 0) return new String("any" + k);
                if (k % 3 == 1) return new H.Obj(k);
                return new Integer(k);
            case "throwable": return new RuntimeException("t" + k);
            case "mh":
                return MethodHandles.lookup().findStatic(H.class, "mh" + (k % 2),
                        MethodType.methodType(void.class, int.class, int.class));
            case "[I": { int[] a = new int[n]; for (int i = 0; i < n; i++) a[i] = 10 * k + i + 1; return a; }
            case "[J": { long[] a = new long[n]; for (int i = 0; i < n; i++) a[i] = 0x100000000L * (k + 1) + i; return a; }
            case "[F": { float[] a = new float[n]; for (int i = 0; i < n; i++) a[i] = k + i / 4f; return a; }
            case "[D": { double[] a = new double[n]; for (int i = 0; i < n; i++) a[i] = k + i / 8.0; return a; }
            case "[Z": { boolean[] a = new boolean[n]; for (int i = 0; i < n; i++) a[i] = (i + k) % 2 == 0; return a; }
            case "[B": { byte[] a = new byte[n]; for (int i = 0; i < n; i++) a[i] = (byte) (20 * k + i - 50); return a; }
            case "[C": { char[] a = new char[n]; for (int i = 0; i < n; i++) a[i] = (char) ('A' + 5 * k + i); return a; }
            case "[S": { short[] a = new short[n]; for (int i = 0; i < n; i++) a[i] = (short) (1000 * k + i - 2000); return a; }
            case "[Object": { Object[] a = new Object[n]; for (int i = 0; i < n; i++) a[i] = "e" + k + i; return a; }
            case "[String": { String[] a = new String[n]; for (int i = 0; i < n; i++) a[i] = "s" + k + i; return a; }
            default: throw new IllegalArgumentException("domain " + domain);
        }
    }

    /** An object's identity: which input it is, null, or new. */
    static String tag(Object o) {
        if (o == null) return "null";
        String k = made.get(o);
        return k != null ? "#" + k : "new";
    }

    static String describe(Object o) {
        if (o == null) return "null";
        String t = tag(o);
        String body;
        if (o instanceof String) body = '"' + (String) o + '"';
        else if (o instanceof H.Obj) {
            H.Obj x = (H.Obj) o;
            body = x.getClass().getName() + "{" + x.id + "," + x.i + "," + x.j + ","
                    + (x.o instanceof String ? x.o : String.valueOf(x.o)) + "," + x.z + "," + x.b
                    + "," + (int) x.c + "," + x.s + "}";
        } else if (o instanceof int[]) body = java.util.Arrays.toString((int[]) o);
        else if (o instanceof long[]) body = java.util.Arrays.toString((long[]) o);
        else if (o instanceof float[]) body = java.util.Arrays.toString((float[]) o);
        else if (o instanceof double[]) body = java.util.Arrays.toString((double[]) o);
        else if (o instanceof boolean[]) body = java.util.Arrays.toString((boolean[]) o);
        else if (o instanceof byte[]) body = java.util.Arrays.toString((byte[]) o);
        else if (o instanceof char[]) body = java.util.Arrays.toString((char[]) o);
        else if (o instanceof short[]) body = java.util.Arrays.toString((short[]) o);
        else if (o instanceof Object[]) body = java.util.Arrays.toString((Object[]) o);
        else if (o instanceof Throwable) body = o.getClass().getName() + ":" + ((Throwable) o).getMessage();
        else if (o instanceof MethodHandle) body = "MH" + ((MethodHandle) o).type();
        else if (o instanceof MethodType) body = "MT" + o;
        else if (o instanceof Class) body = "class " + ((Class<?>) o).getName();
        else if (o instanceof Number || o instanceof Boolean || o instanceof Character) {
            body = o.getClass().getSimpleName() + ":" + o;
        } else body = o.getClass().getName();
        return t + ":" + body;
    }
}
