// Prints, for every instruction of every method in the given .class files,
// what ASM says it does to the operand stack, the locals and the heap. The
// `jvm:asm-flow` check (asm_flow/mod.rs) prints the same form from jvm-reader's
// dataflow and requires the two to agree.
//
//   java -cp <asm jars> AsmFlow.java <file.class>...
//
// ASM's analyzer gives the frame (stack and locals, with value sizes) before
// each instruction. The frame is then refilled with one fresh atom per input
// -- `in<p>` for stack slot p (counted from the bottom, a category-2 value
// taking two), `local<n>` for local n -- and ASM's `Frame.execute` runs the
// instruction over it with the interpreter below. ASM, not this file, decides
// where each value goes: which slots `dup_x1` copies, which value `putfield`
// stores, which local `lstore` writes. The interpreter only says what a
// computed value is made of: every input of an arithmetic op, the element of
// an array load, the field of a field load.
//
// Output, one line per instruction:
//
//   method <class> <name><descriptor>
//   <index> op=<opcode> h=<height before>-><height after> <effect>...
//   <index> dead
//
// <index> counts instructions, not bytes. <opcode> is the one ASM reports, so
// `iload_0` is `iload` and `ldc_w` is `ldc`. Each effect is <place>=<atoms>,
// with atoms joined by `+`; effects are sorted by place. The places:
//
//   out<p>                  stack slot p after, unless it still holds in<p>
//   local<n>                local n after, unless unchanged
//   field:<C>.<f>[@<obj>]   a field store; <obj> is the object's slot
//   elem:<arr>[<idx>]       an array store
//   recv, arg<j>            a call's receiver and its j-th argument slot
//   thrown                  the value athrow throws
//
// The atoms: in<p>, local<n>, field:..., elem:... (a load), ret (a call's
// result), const:<value> and alloc:<type>.

import java.io.FileInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.TreeSet;

import org.objectweb.asm.ClassReader;
import org.objectweb.asm.ConstantDynamic;
import org.objectweb.asm.Handle;
import org.objectweb.asm.Opcodes;
import org.objectweb.asm.Type;
import org.objectweb.asm.tree.AbstractInsnNode;
import org.objectweb.asm.tree.ClassNode;
import org.objectweb.asm.tree.FieldInsnNode;
import org.objectweb.asm.tree.IntInsnNode;
import org.objectweb.asm.tree.InvokeDynamicInsnNode;
import org.objectweb.asm.tree.LdcInsnNode;
import org.objectweb.asm.tree.MethodInsnNode;
import org.objectweb.asm.tree.MethodNode;
import org.objectweb.asm.tree.MultiANewArrayInsnNode;
import org.objectweb.asm.tree.TypeInsnNode;
import org.objectweb.asm.tree.analysis.Analyzer;
import org.objectweb.asm.tree.analysis.AnalyzerException;
import org.objectweb.asm.tree.analysis.BasicInterpreter;
import org.objectweb.asm.tree.analysis.BasicValue;
import org.objectweb.asm.tree.analysis.Frame;
import org.objectweb.asm.tree.analysis.Interpreter;
import org.objectweb.asm.tree.analysis.Value;

public class AsmFlow {
    /**
     * A value: the atoms each of its slots holds. A category-2 value keeps its
     * two halves apart while it is only moved, so `dup2` of a long can be told
     * from `dup2` that crosses the halves; anything computed from it gets the
     * union in both.
     */
    static final class A implements Value {
        final TreeSet<String> lo;
        final TreeSet<String> hi; // null for a category-1 value

        A(TreeSet<String> lo, TreeSet<String> hi) {
            this.lo = lo;
            this.hi = hi;
        }

        static A of(int size, TreeSet<String> atoms) {
            return new A(atoms, size == 2 ? atoms : null);
        }

        static A of(int size, String atom) {
            TreeSet<String> s = new TreeSet<>();
            s.add(atom);
            return of(size, s);
        }

        @Override
        public int getSize() {
            return hi == null ? 1 : 2;
        }

        TreeSet<String> all() {
            TreeSet<String> s = new TreeSet<>(lo);
            if (hi != null) s.addAll(hi);
            return s;
        }
    }

    /** One instruction's heap and call effects, filled in by the interpreter. */
    static TreeMap<String, TreeSet<String>> effects;

    static TreeSet<String> union(List<? extends A> vs) {
        TreeSet<String> s = new TreeSet<>();
        for (A v : vs) s.addAll(v.all());
        return s;
    }

    static String join(TreeSet<String> atoms) {
        return String.join("+", atoms);
    }

    static void effect(String place, TreeSet<String> atoms) {
        effects.computeIfAbsent(place, k -> new TreeSet<>()).addAll(atoms);
    }

    /** A field, and the object it belongs to for an instance field. */
    static String field(FieldInsnNode f, A obj) {
        String s = "field:" + f.owner + "." + f.name;
        return obj == null ? s : s + "@" + join(obj.all());
    }

    static String elem(A arr, A idx) {
        return "elem:" + join(arr.all()) + "[" + join(idx.all()) + "]";
    }

    /**
     * A string constant with every character outside [A-Za-z0-9] escaped, so
     * it cannot be confused with the line's separators. jvm-reader holds string
     * constants as Rust strings, which turns each unpaired surrogate into
     * U+FFFD, so do the same here.
     */
    static String escape(String s) {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            if (Character.isHighSurrogate(c) && i + 1 < s.length()
                    && Character.isLowSurrogate(s.charAt(i + 1))) {
                sb.append(String.format("\\u%04x\\u%04x", (int) c, (int) s.charAt(i + 1)));
                i++;
            } else if (Character.isSurrogate(c)) {
                sb.append("\\ufffd");
            } else if ((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9')) {
                sb.append(c);
            } else {
                sb.append(String.format("\\u%04x", (int) c));
            }
        }
        return sb.toString();
    }

    static String constant(Object cst) {
        if (cst instanceof Integer) return "const:I" + cst;
        if (cst instanceof Long) return "const:J" + cst;
        if (cst instanceof Float)
            return "const:F" + Integer.toHexString(Float.floatToRawIntBits((Float) cst));
        if (cst instanceof Double)
            return "const:D" + Long.toHexString(Double.doubleToRawLongBits((Double) cst));
        if (cst instanceof String) return "const:S" + escape((String) cst);
        if (cst instanceof Type) {
            Type t = (Type) cst;
            if (t.getSort() == Type.METHOD) return "const:MT" + t.getDescriptor();
            return "const:C" + t.getInternalName();
        }
        if (cst instanceof Handle) return "const:MH" + ((Handle) cst).getName();
        if (cst instanceof ConstantDynamic) return "const:CD" + ((ConstantDynamic) cst).getName();
        throw new IllegalArgumentException("constant " + cst);
    }

    static String arrayOf(String elementDescriptor) {
        return "alloc:[" + elementDescriptor;
    }

    static String primitiveArray(int atype) {
        switch (atype) {
            case Opcodes.T_BOOLEAN: return arrayOf("Z");
            case Opcodes.T_CHAR: return arrayOf("C");
            case Opcodes.T_FLOAT: return arrayOf("F");
            case Opcodes.T_DOUBLE: return arrayOf("D");
            case Opcodes.T_BYTE: return arrayOf("B");
            case Opcodes.T_SHORT: return arrayOf("S");
            case Opcodes.T_INT: return arrayOf("I");
            case Opcodes.T_LONG: return arrayOf("J");
            default: throw new IllegalArgumentException("newarray type " + atype);
        }
    }

    static TreeSet<String> with(TreeSet<String> atoms, String atom) {
        TreeSet<String> s = new TreeSet<>(atoms);
        s.add(atom);
        return s;
    }

    /** Says what each computed value is made of; see the file comment. */
    static final class Atoms extends Interpreter<A> {
        Atoms() {
            super(Opcodes.ASM9);
        }

        @Override
        public A newValue(Type type) {
            if (type == Type.VOID_TYPE) return null;
            return A.of(type == null ? 1 : type.getSize(), new TreeSet<>());
        }

        @Override
        public A newOperation(AbstractInsnNode insn) {
            int op = insn.getOpcode();
            switch (op) {
                case Opcodes.ACONST_NULL:
                    return A.of(1, "const:null");
                case Opcodes.ICONST_M1: case Opcodes.ICONST_0: case Opcodes.ICONST_1:
                case Opcodes.ICONST_2: case Opcodes.ICONST_3: case Opcodes.ICONST_4:
                case Opcodes.ICONST_5:
                    return A.of(1, constant(op - Opcodes.ICONST_0));
                case Opcodes.LCONST_0: case Opcodes.LCONST_1:
                    return A.of(2, constant((long) (op - Opcodes.LCONST_0)));
                case Opcodes.FCONST_0: case Opcodes.FCONST_1: case Opcodes.FCONST_2:
                    return A.of(1, constant((float) (op - Opcodes.FCONST_0)));
                case Opcodes.DCONST_0: case Opcodes.DCONST_1:
                    return A.of(2, constant((double) (op - Opcodes.DCONST_0)));
                case Opcodes.BIPUSH: case Opcodes.SIPUSH:
                    return A.of(1, constant(((IntInsnNode) insn).operand));
                case Opcodes.LDC: {
                    Object cst = ((LdcInsnNode) insn).cst;
                    int size = (cst instanceof Long || cst instanceof Double) ? 2 : 1;
                    if (cst instanceof ConstantDynamic)
                        size = Type.getType(((ConstantDynamic) cst).getDescriptor()).getSize();
                    return A.of(size, constant(cst));
                }
                case Opcodes.JSR:
                    return A.of(1, new TreeSet<>());
                case Opcodes.GETSTATIC: {
                    FieldInsnNode f = (FieldInsnNode) insn;
                    return A.of(Type.getType(f.desc).getSize(), field(f, null));
                }
                case Opcodes.NEW:
                    return A.of(1, "alloc:" + ((TypeInsnNode) insn).desc);
                default:
                    throw new IllegalArgumentException("newOperation " + op);
            }
        }

        @Override
        public A copyOperation(AbstractInsnNode insn, A value) {
            return value;
        }

        @Override
        public A unaryOperation(AbstractInsnNode insn, A value) {
            int op = insn.getOpcode();
            switch (op) {
                case Opcodes.INEG: case Opcodes.FNEG: case Opcodes.IINC:
                case Opcodes.L2I: case Opcodes.F2I: case Opcodes.D2I:
                case Opcodes.I2F: case Opcodes.L2F: case Opcodes.D2F:
                case Opcodes.I2B: case Opcodes.I2C: case Opcodes.I2S:
                case Opcodes.ARRAYLENGTH: case Opcodes.INSTANCEOF: case Opcodes.CHECKCAST:
                    return A.of(1, value.all());
                case Opcodes.LNEG: case Opcodes.DNEG:
                case Opcodes.I2L: case Opcodes.F2L: case Opcodes.D2L:
                case Opcodes.I2D: case Opcodes.L2D: case Opcodes.F2D:
                    return A.of(2, value.all());
                case Opcodes.NEWARRAY:
                    return A.of(1, with(value.all(), primitiveArray(((IntInsnNode) insn).operand)));
                case Opcodes.ANEWARRAY: {
                    String desc = ((TypeInsnNode) insn).desc;
                    String element = desc.startsWith("[") ? desc : "L" + desc + ";";
                    return A.of(1, with(value.all(), arrayOf(element)));
                }
                case Opcodes.GETFIELD: {
                    FieldInsnNode f = (FieldInsnNode) insn;
                    return A.of(Type.getType(f.desc).getSize(), with(value.all(), field(f, value)));
                }
                case Opcodes.PUTSTATIC:
                    effect(field((FieldInsnNode) insn, null), value.all());
                    return null;
                case Opcodes.ATHROW:
                    effect("thrown", value.all());
                    return null;
                default:
                    // Branches, switches, returns and monitors produce nothing.
                    return null;
            }
        }

        @Override
        public A binaryOperation(AbstractInsnNode insn, A v1, A v2) {
            int op = insn.getOpcode();
            switch (op) {
                case Opcodes.IALOAD: case Opcodes.FALOAD: case Opcodes.AALOAD:
                case Opcodes.BALOAD: case Opcodes.CALOAD: case Opcodes.SALOAD:
                    return A.of(1, elem(v1, v2));
                case Opcodes.LALOAD: case Opcodes.DALOAD:
                    return A.of(2, elem(v1, v2));
                case Opcodes.PUTFIELD:
                    effect(field((FieldInsnNode) insn, v1), v2.all());
                    return null;
                case Opcodes.IF_ICMPEQ: case Opcodes.IF_ICMPNE: case Opcodes.IF_ICMPLT:
                case Opcodes.IF_ICMPGE: case Opcodes.IF_ICMPGT: case Opcodes.IF_ICMPLE:
                case Opcodes.IF_ACMPEQ: case Opcodes.IF_ACMPNE:
                    return null;
                default: {
                    TreeSet<String> s = v1.all();
                    s.addAll(v2.all());
                    int size;
                    switch (op) {
                        case Opcodes.LADD: case Opcodes.LSUB: case Opcodes.LMUL:
                        case Opcodes.LDIV: case Opcodes.LREM: case Opcodes.LSHL:
                        case Opcodes.LSHR: case Opcodes.LUSHR: case Opcodes.LAND:
                        case Opcodes.LOR: case Opcodes.LXOR:
                        case Opcodes.DADD: case Opcodes.DSUB: case Opcodes.DMUL:
                        case Opcodes.DDIV: case Opcodes.DREM:
                            size = 2;
                            break;
                        default:
                            size = 1;
                    }
                    return A.of(size, s);
                }
            }
        }

        @Override
        public A ternaryOperation(AbstractInsnNode insn, A v1, A v2, A v3) {
            effect(elem(v1, v2), v3.all());
            return null;
        }

        @Override
        public A naryOperation(AbstractInsnNode insn, List<? extends A> values) {
            int op = insn.getOpcode();
            if (op == Opcodes.MULTIANEWARRAY) {
                return A.of(1, with(union(values), "alloc:" + ((MultiANewArrayInsnNode) insn).desc));
            }
            String desc;
            int first = 0;
            if (op == Opcodes.INVOKEDYNAMIC) {
                desc = ((InvokeDynamicInsnNode) insn).desc;
            } else {
                desc = ((MethodInsnNode) insn).desc;
                if (op != Opcodes.INVOKESTATIC) {
                    effect("recv", values.get(0).all());
                    first = 1;
                }
            }
            int slot = 0;
            for (A v : values.subList(first, values.size())) {
                effect("arg" + slot++, v.lo);
                if (v.hi != null) effect("arg" + slot++, v.hi);
            }
            Type ret = Type.getReturnType(desc);
            return ret == Type.VOID_TYPE ? null : A.of(ret.getSize(), "ret");
        }

        @Override
        public void returnOperation(AbstractInsnNode insn, A value, A expected) {}

        @Override
        public A merge(A a, A b) {
            return a;
        }
    }

    /** Stack slots of a frame, bottom first, each with the atoms it holds. */
    static List<TreeSet<String>> slots(Frame<A> f) {
        List<TreeSet<String>> out = new ArrayList<>();
        for (int i = 0; i < f.getStackSize(); i++) {
            A v = f.getStack(i);
            out.add(v.lo);
            if (v.hi != null) out.add(v.hi);
        }
        return out;
    }

    static void dumpMethod(String owner, MethodNode mn, StringBuilder out) throws AnalyzerException {
        out.append("method ").append(owner).append(' ').append(mn.name).append(mn.desc).append('\n');
        Frame<BasicValue>[] frames = new Analyzer<>(new BasicInterpreter()).analyze(owner, mn);
        Atoms atoms = new Atoms();
        int index = 0;
        for (int i = 0; i < mn.instructions.size(); i++) {
            AbstractInsnNode insn = mn.instructions.get(i);
            if (insn.getOpcode() < 0) continue;
            int idx = index++;
            Frame<BasicValue> types = frames[i];
            if (types == null) {
                out.append(idx).append(" dead\n");
                continue;
            }

            Frame<A> f = new Frame<>(types.getLocals(), Math.max(mn.maxStack, 1));
            for (int n = 0; n < types.getLocals(); n++) {
                BasicValue t = types.getLocal(n);
                if (t == BasicValue.UNINITIALIZED_VALUE) {
                    f.setLocal(n, A.of(1, new TreeSet<>()));
                } else {
                    f.setLocal(n, A.of(t.getSize(), "local" + n));
                }
            }
            int height = 0;
            for (int k = 0; k < types.getStackSize(); k++) {
                BasicValue t = types.getStack(k);
                TreeSet<String> lo = new TreeSet<>();
                lo.add("in" + height++);
                TreeSet<String> hi = null;
                if (t.getSize() == 2) {
                    hi = new TreeSet<>();
                    hi.add("in" + height++);
                }
                f.push(new A(lo, hi));
            }

            Frame<A> after = new Frame<>(f);
            effects = new TreeMap<>();
            after.execute(insn, atoms);

            List<TreeSet<String>> stack = slots(after);
            for (int p = 0; p < stack.size(); p++) {
                TreeSet<String> s = stack.get(p);
                if (p < height && s.size() == 1 && s.first().equals("in" + p)) continue;
                effect("out" + p, s);
            }
            for (int n = 0; n < after.getLocals(); n++) {
                TreeSet<String> s = after.getLocal(n).all();
                if (s.isEmpty() || (s.size() == 1 && s.first().equals("local" + n))) continue;
                effect("local" + n, s);
            }

            out.append(idx).append(" op=").append(insn.getOpcode())
                    .append(" h=").append(height).append("->").append(stack.size());
            for (Map.Entry<String, TreeSet<String>> e : effects.entrySet()) {
                out.append(' ').append(e.getKey()).append('=').append(join(e.getValue()));
            }
            out.append('\n');
        }
    }

    public static void main(String[] args) throws IOException {
        StringBuilder out = new StringBuilder();
        for (String path : args) {
            ClassNode cn = new ClassNode();
            try (InputStream in = new FileInputStream(path)) {
                new ClassReader(in).accept(cn, 0);
            }
            out.append("class ").append(cn.name).append('\n');
            for (MethodNode mn : cn.methods) {
                if (mn.instructions.size() == 0) continue;
                try {
                    dumpMethod(cn.name, mn, out);
                } catch (AnalyzerException e) {
                    throw new IOException(path + ": " + cn.name + "." + mn.name + mn.desc + ": " + e, e);
                }
            }
        }
        System.out.print(out);
    }
}
