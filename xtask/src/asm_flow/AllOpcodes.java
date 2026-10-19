// Writes AllOpcodes.class, which holds every JVM opcode, into the directory
// named on the command line. It is the JVM counterpart of
// xtask/tests/dex/all-opcodes.smali: javac never emits `swap`, `jsr`, the
// `dup2_x*` forms on category-2 values, `goto_w` and others, so the compiled
// samples cannot cover them.
//
//   java -cp <asm jars> AllOpcodes.java <out dir>
//
// The class is analyzed, never loaded, so it need not pass the verifier, but
// each method is well typed: ASM's analyzer must be able to give the frame
// before every instruction. The version is 50 so that `jsr` and `ret` are
// legal and no stack map frames are needed.

import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;

import org.objectweb.asm.ClassWriter;
import org.objectweb.asm.ConstantDynamic;
import org.objectweb.asm.Handle;
import org.objectweb.asm.Label;
import org.objectweb.asm.MethodVisitor;
import org.objectweb.asm.Opcodes;
import org.objectweb.asm.Type;

public class AllOpcodes implements Opcodes {
    static final String C = "AllOpcodes";
    static final Handle BSM = new Handle(H_INVOKESTATIC, C, "bsm",
            "(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/Object;)Ljava/lang/Object;",
            false);

    static ClassWriter cw;

    static MethodVisitor method(String name, String desc) {
        MethodVisitor mv = cw.visitMethod(ACC_PUBLIC | ACC_STATIC, name, desc, null, null);
        mv.visitCode();
        return mv;
    }

    static void end(MethodVisitor mv) {
        mv.visitMaxs(0, 0);
        mv.visitEnd();
    }

    static void ret(MethodVisitor mv) {
        mv.visitInsn(RETURN);
        end(mv);
    }

    /** Every constant-pushing instruction, each popped again. */
    static void constants() {
        MethodVisitor mv = method("constants", "()V");
        int[] narrow = {ACONST_NULL, ICONST_M1, ICONST_0, ICONST_1, ICONST_2, ICONST_3, ICONST_4,
            ICONST_5, FCONST_0, FCONST_1, FCONST_2};
        for (int op : narrow) {
            mv.visitInsn(op);
            mv.visitInsn(POP);
        }
        for (int op : new int[] {LCONST_0, LCONST_1, DCONST_0, DCONST_1}) {
            mv.visitInsn(op);
            mv.visitInsn(POP2);
        }
        mv.visitIntInsn(BIPUSH, -100);
        mv.visitInsn(POP);
        mv.visitIntInsn(SIPUSH, -30000);
        mv.visitInsn(POP);
        Object[] ldcs = {123456, 1.5f, "a string é 😀", Type.getType("Ljava/lang/Object;"),
            Type.getType("[I")};
        for (Object c : ldcs) {
            mv.visitLdcInsn(c);
            mv.visitInsn(POP);
        }
        // Push the constant pool past 256 entries so the next ldc is ldc_w.
        for (int i = 0; i < 260; i++) {
            mv.visitLdcInsn(1000000 + i);
            mv.visitInsn(POP);
        }
        mv.visitLdcInsn("late");
        mv.visitInsn(POP);
        mv.visitLdcInsn(-2.25f);
        mv.visitInsn(POP);
        mv.visitLdcInsn(1L << 40);
        mv.visitInsn(POP2);
        mv.visitLdcInsn(-0.5d);
        mv.visitInsn(POP2);
        ret(mv);
    }

    /** ldc of the constant kinds that are not values in the pool's narrow sense. */
    static void dynamicConstants() {
        MethodVisitor mv = method("dynamicConstants", "()V");
        mv.visitLdcInsn(Type.getMethodType("(I)J"));
        mv.visitInsn(POP);
        mv.visitLdcInsn(BSM);
        mv.visitInsn(POP);
        mv.visitLdcInsn(new ConstantDynamic("cd", "Ljava/lang/Object;", BSM));
        mv.visitInsn(POP);
        ret(mv);
    }

    /** Loads and stores of each type, in their short, plain and wide forms, and iinc. */
    static void locals() {
        MethodVisitor mv = method("locals", "()V");
        int[][] kinds = {
            {ICONST_1, ISTORE, ILOAD, POP},
            {LCONST_1, LSTORE, LLOAD, POP2},
            {FCONST_1, FSTORE, FLOAD, POP},
            {DCONST_1, DSTORE, DLOAD, POP2},
            {ACONST_NULL, ASTORE, ALOAD, POP},
        };
        for (int[] k : kinds) {
            for (int n : new int[] {0, 1, 2, 3, 6, 300}) {
                mv.visitInsn(k[0]);
                mv.visitVarInsn(k[1], n);
                mv.visitVarInsn(k[2], n);
                mv.visitInsn(k[3]);
            }
        }
        mv.visitInsn(ICONST_0);
        mv.visitVarInsn(ISTORE, 1);
        mv.visitIincInsn(1, -5);
        mv.visitInsn(ICONST_0);
        mv.visitVarInsn(ISTORE, 300);
        mv.visitIincInsn(300, 1000);
        ret(mv);
    }

    /** Locals that hold parameters: a long parameter and the slots around it. */
    static void parameters() {
        MethodVisitor mv = method("parameters", "(IJLjava/lang/Object;)J");
        mv.visitVarInsn(LLOAD, 1);
        mv.visitVarInsn(ILOAD, 0);
        mv.visitInsn(I2L);
        mv.visitInsn(LADD);
        mv.visitVarInsn(LSTORE, 1);
        mv.visitVarInsn(ALOAD, 3);
        mv.visitVarInsn(ASTORE, 4);
        mv.visitIincInsn(0, 1);
        mv.visitVarInsn(LLOAD, 1);
        mv.visitInsn(LRETURN);
        end(mv);
    }

    static void binary(MethodVisitor mv, int push, int op, int pop) {
        mv.visitInsn(push);
        mv.visitInsn(push);
        mv.visitInsn(op);
        mv.visitInsn(pop);
    }

    /** Arithmetic, logic, shifts, conversions and comparisons. */
    static void arithmetic() {
        MethodVisitor mv = method("arithmetic", "()V");
        for (int op : new int[] {IADD, ISUB, IMUL, IDIV, IREM, ISHL, ISHR, IUSHR, IAND, IOR, IXOR})
            binary(mv, ICONST_3, op, POP);
        for (int op : new int[] {LADD, LSUB, LMUL, LDIV, LREM, LAND, LOR, LXOR})
            binary(mv, LCONST_1, op, POP2);
        for (int op : new int[] {FADD, FSUB, FMUL, FDIV, FREM}) binary(mv, FCONST_2, op, POP);
        for (int op : new int[] {DADD, DSUB, DMUL, DDIV, DREM}) binary(mv, DCONST_1, op, POP2);
        for (int op : new int[] {LSHL, LSHR, LUSHR}) {
            mv.visitInsn(LCONST_1);
            mv.visitInsn(ICONST_2);
            mv.visitInsn(op);
            mv.visitInsn(POP2);
        }
        binary(mv, LCONST_0, LCMP, POP);
        binary(mv, FCONST_0, FCMPL, POP);
        binary(mv, FCONST_0, FCMPG, POP);
        binary(mv, DCONST_0, DCMPL, POP);
        binary(mv, DCONST_0, DCMPG, POP);
        int[][] unary = {
            {ICONST_1, INEG, POP}, {LCONST_1, LNEG, POP2}, {FCONST_1, FNEG, POP}, {DCONST_1, DNEG, POP2},
            {ICONST_1, I2L, POP2}, {ICONST_1, I2F, POP}, {ICONST_1, I2D, POP2},
            {LCONST_1, L2I, POP}, {LCONST_1, L2F, POP}, {LCONST_1, L2D, POP2},
            {FCONST_1, F2I, POP}, {FCONST_1, F2L, POP2}, {FCONST_1, F2D, POP2},
            {DCONST_1, D2I, POP}, {DCONST_1, D2L, POP2}, {DCONST_1, D2F, POP},
            {ICONST_1, I2B, POP}, {ICONST_1, I2C, POP}, {ICONST_1, I2S, POP},
        };
        for (int[] u : unary) {
            mv.visitInsn(u[0]);
            mv.visitInsn(u[1]);
            mv.visitInsn(u[2]);
        }
        ret(mv);
    }

    /** Every form of every stack-shuffling instruction (JVMS 6.5). */
    static void stack() {
        MethodVisitor mv = method("stack", "()V");
        // dup, dup_x1, swap: category 1 only.
        mv.visitInsn(ICONST_1);
        mv.visitInsn(DUP);
        mv.visitInsn(POP2);
        mv.visitInsn(ICONST_1);
        mv.visitInsn(ICONST_2);
        mv.visitInsn(DUP_X1);
        mv.visitInsn(POP2);
        mv.visitInsn(POP);
        mv.visitInsn(ICONST_1);
        mv.visitInsn(ICONST_2);
        mv.visitInsn(SWAP);
        mv.visitInsn(POP2);
        // dup_x2 form 1: three category-1 values; form 2: a long under an int.
        mv.visitInsn(ICONST_1);
        mv.visitInsn(ICONST_2);
        mv.visitInsn(ICONST_3);
        mv.visitInsn(DUP_X2);
        mv.visitInsn(POP2);
        mv.visitInsn(POP2);
        mv.visitInsn(LCONST_1);
        mv.visitInsn(ICONST_3);
        mv.visitInsn(DUP_X2);
        mv.visitInsn(POP);
        mv.visitInsn(POP2);
        mv.visitInsn(POP);
        // dup2 form 1: two ints; form 2: one long.
        mv.visitInsn(ICONST_1);
        mv.visitInsn(ICONST_2);
        mv.visitInsn(DUP2);
        mv.visitInsn(POP2);
        mv.visitInsn(POP2);
        mv.visitInsn(LCONST_1);
        mv.visitInsn(DUP2);
        mv.visitInsn(POP2);
        mv.visitInsn(POP2);
        // dup2_x1 form 1: two ints over an int; form 2: a long over an int.
        mv.visitInsn(ICONST_1);
        mv.visitInsn(ICONST_2);
        mv.visitInsn(ICONST_3);
        mv.visitInsn(DUP2_X1);
        mv.visitInsn(POP2);
        mv.visitInsn(POP2);
        mv.visitInsn(POP);
        mv.visitInsn(ICONST_1);
        mv.visitInsn(LCONST_1);
        mv.visitInsn(DUP2_X1);
        mv.visitInsn(POP2);
        mv.visitInsn(POP);
        mv.visitInsn(POP2);
        // dup2_x2 forms 1-4.
        mv.visitInsn(ICONST_1); // form 1: int int over int int
        mv.visitInsn(ICONST_2);
        mv.visitInsn(ICONST_3);
        mv.visitInsn(ICONST_4);
        mv.visitInsn(DUP2_X2);
        mv.visitInsn(POP2);
        mv.visitInsn(POP2);
        mv.visitInsn(POP2);
        mv.visitInsn(ICONST_1); // form 2: long over int int
        mv.visitInsn(ICONST_2);
        mv.visitInsn(LCONST_1);
        mv.visitInsn(DUP2_X2);
        mv.visitInsn(POP2);
        mv.visitInsn(POP2);
        mv.visitInsn(POP2);
        mv.visitInsn(LCONST_0); // form 3: int int over long
        mv.visitInsn(ICONST_1);
        mv.visitInsn(ICONST_2);
        mv.visitInsn(DUP2_X2);
        mv.visitInsn(POP2);
        mv.visitInsn(POP2);
        mv.visitInsn(POP2);
        mv.visitInsn(LCONST_0); // form 4: long over long
        mv.visitInsn(LCONST_1);
        mv.visitInsn(DUP2_X2);
        mv.visitInsn(POP2);
        mv.visitInsn(POP2);
        mv.visitInsn(POP2);
        mv.visitInsn(NOP);
        ret(mv);
    }

    static void arrays() {
        MethodVisitor mv = method("arrays", "()V");
        // {atype, load, store, push value, pop}
        int[][] kinds = {
            {T_INT, IALOAD, IASTORE, ICONST_1, POP},
            {T_LONG, LALOAD, LASTORE, LCONST_1, POP2},
            {T_FLOAT, FALOAD, FASTORE, FCONST_1, POP},
            {T_DOUBLE, DALOAD, DASTORE, DCONST_1, POP2},
            {T_BYTE, BALOAD, BASTORE, ICONST_1, POP},
            {T_BOOLEAN, BALOAD, BASTORE, ICONST_1, POP},
            {T_CHAR, CALOAD, CASTORE, ICONST_1, POP},
            {T_SHORT, SALOAD, SASTORE, ICONST_1, POP},
        };
        for (int[] k : kinds) {
            mv.visitInsn(ICONST_2);
            mv.visitIntInsn(NEWARRAY, k[0]);
            mv.visitVarInsn(ASTORE, 0);
            mv.visitVarInsn(ALOAD, 0);
            mv.visitInsn(ICONST_1);
            mv.visitInsn(k[3]);
            mv.visitInsn(k[2]);
            mv.visitVarInsn(ALOAD, 0);
            mv.visitInsn(ICONST_1);
            mv.visitInsn(k[1]);
            mv.visitInsn(k[4]);
            mv.visitVarInsn(ALOAD, 0);
            mv.visitInsn(ARRAYLENGTH);
            mv.visitInsn(POP);
        }
        mv.visitInsn(ICONST_2);
        mv.visitTypeInsn(ANEWARRAY, "java/lang/Object");
        mv.visitInsn(DUP);
        mv.visitInsn(ICONST_0);
        mv.visitInsn(ACONST_NULL);
        mv.visitInsn(AASTORE);
        mv.visitInsn(ICONST_0);
        mv.visitInsn(AALOAD);
        mv.visitInsn(POP);
        mv.visitInsn(ICONST_2);
        mv.visitTypeInsn(ANEWARRAY, "[I");
        mv.visitInsn(POP);
        mv.visitInsn(ICONST_2);
        mv.visitInsn(ICONST_3);
        mv.visitMultiANewArrayInsn("[[J", 2);
        mv.visitInsn(POP);
        ret(mv);
    }

    static void fields() {
        cw.visitField(ACC_STATIC, "si", "I", null, null).visitEnd();
        cw.visitField(ACC_STATIC, "sj", "J", null, null).visitEnd();
        cw.visitField(0, "fi", "I", null, null).visitEnd();
        cw.visitField(0, "fj", "J", null, null).visitEnd();
        MethodVisitor mv = method("fields", "(LAllOpcodes;)V");
        mv.visitFieldInsn(GETSTATIC, C, "si", "I");
        mv.visitFieldInsn(PUTSTATIC, C, "si", "I");
        mv.visitFieldInsn(GETSTATIC, C, "sj", "J");
        mv.visitFieldInsn(PUTSTATIC, C, "sj", "J");
        mv.visitVarInsn(ALOAD, 0);
        mv.visitVarInsn(ALOAD, 0);
        mv.visitFieldInsn(GETFIELD, C, "fi", "I");
        mv.visitFieldInsn(PUTFIELD, C, "fi", "I");
        mv.visitVarInsn(ALOAD, 0);
        mv.visitVarInsn(ALOAD, 0);
        mv.visitFieldInsn(GETFIELD, C, "fj", "J");
        mv.visitFieldInsn(PUTFIELD, C, "fj", "J");
        ret(mv);
    }

    static void calls() {
        MethodVisitor mv = method("calls", "(LAllOpcodes;Ljava/lang/Runnable;)J");
        mv.visitTypeInsn(NEW, C);
        mv.visitInsn(DUP);
        mv.visitMethodInsn(INVOKESPECIAL, C, "<init>", "()V", false);
        mv.visitInsn(POP);
        mv.visitVarInsn(ALOAD, 0);
        mv.visitInsn(ICONST_1);
        mv.visitInsn(LCONST_1);
        mv.visitInsn(ACONST_NULL);
        mv.visitMethodInsn(INVOKEVIRTUAL, C, "virt", "(IJLjava/lang/Object;)J", false);
        mv.visitInsn(POP2);
        mv.visitVarInsn(ALOAD, 1);
        mv.visitMethodInsn(INVOKEINTERFACE, "java/lang/Runnable", "run", "()V", true);
        mv.visitInsn(DCONST_1);
        mv.visitInsn(ICONST_2);
        mv.visitMethodInsn(INVOKESTATIC, C, "stat", "(DI)I", false);
        mv.visitInsn(POP);
        mv.visitInsn(LCONST_0);
        mv.visitVarInsn(ALOAD, 0);
        mv.visitInvokeDynamicInsn("indy", "(JLAllOpcodes;)J", BSM);
        mv.visitInsn(LRETURN);
        end(mv);

        MethodVisitor init = cw.visitMethod(ACC_PUBLIC, "<init>", "()V", null, null);
        init.visitCode();
        init.visitVarInsn(ALOAD, 0);
        init.visitMethodInsn(INVOKESPECIAL, "java/lang/Object", "<init>", "()V", false);
        ret(init);
    }

    static void objects() {
        MethodVisitor mv = method("objects", "(Ljava/lang/Object;)V");
        mv.visitVarInsn(ALOAD, 0);
        mv.visitTypeInsn(CHECKCAST, "java/lang/String");
        mv.visitTypeInsn(INSTANCEOF, "java/lang/String");
        mv.visitInsn(POP);
        mv.visitVarInsn(ALOAD, 0);
        mv.visitInsn(MONITORENTER);
        mv.visitVarInsn(ALOAD, 0);
        mv.visitInsn(MONITOREXIT);
        mv.visitVarInsn(ALOAD, 0);
        mv.visitTypeInsn(CHECKCAST, "java/lang/Throwable");
        mv.visitInsn(ATHROW);
        end(mv);
    }

    /** Every conditional branch, goto and both switches. */
    static void branches() {
        MethodVisitor mv = method("branches", "(ILjava/lang/Object;)V");
        Label next = new Label();
        for (int op : new int[] {IFEQ, IFNE, IFLT, IFGE, IFGT, IFLE}) {
            mv.visitVarInsn(ILOAD, 0);
            mv.visitJumpInsn(op, next = new Label());
            mv.visitLabel(next);
        }
        for (int op : new int[] {IF_ICMPEQ, IF_ICMPNE, IF_ICMPLT, IF_ICMPGE, IF_ICMPGT, IF_ICMPLE}) {
            mv.visitVarInsn(ILOAD, 0);
            mv.visitInsn(ICONST_1);
            mv.visitJumpInsn(op, next = new Label());
            mv.visitLabel(next);
        }
        for (int op : new int[] {IF_ACMPEQ, IF_ACMPNE}) {
            mv.visitVarInsn(ALOAD, 1);
            mv.visitInsn(ACONST_NULL);
            mv.visitJumpInsn(op, next = new Label());
            mv.visitLabel(next);
        }
        for (int op : new int[] {IFNULL, IFNONNULL}) {
            mv.visitVarInsn(ALOAD, 1);
            mv.visitJumpInsn(op, next = new Label());
            mv.visitLabel(next);
        }
        Label a = new Label(), b = new Label(), dflt = new Label();
        mv.visitVarInsn(ILOAD, 0);
        mv.visitTableSwitchInsn(0, 1, dflt, a, b);
        mv.visitLabel(a);
        mv.visitVarInsn(ILOAD, 0);
        mv.visitLookupSwitchInsn(dflt, new int[] {-7, 1000}, new Label[] {b, dflt});
        mv.visitLabel(b);
        mv.visitJumpInsn(GOTO, dflt);
        mv.visitLabel(dflt);
        ret(mv);
    }

    static void returns() {
        Object[][] kinds = {
            {"ireturn", "()I", ICONST_0, IRETURN}, {"lreturn", "()J", LCONST_0, LRETURN},
            {"freturn", "()F", FCONST_0, FRETURN}, {"dreturn", "()D", DCONST_0, DRETURN},
            {"areturn", "()Ljava/lang/Object;", ACONST_NULL, ARETURN},
        };
        for (Object[] k : kinds) {
            MethodVisitor mv = method((String) k[0], (String) k[1]);
            mv.visitInsn((Integer) k[2]);
            mv.visitInsn((Integer) k[3]);
            end(mv);
        }
    }

    /**
     * goto_w and jsr_w, which ASM emits only for an offset that does not fit
     * in 16 bits, and jsr and ret, plain and wide.
     */
    static void farJumps() {
        MethodVisitor mv = method("farJumps", "()V");
        Label far = new Label(), sub = new Label(), wideSub = new Label(), done = new Label();
        mv.visitJumpInsn(JSR, sub);
        mv.visitJumpInsn(JSR, wideSub);
        mv.visitJumpInsn(GOTO, far);
        mv.visitLabel(sub);
        mv.visitVarInsn(ASTORE, 0);
        mv.visitVarInsn(RET, 0);
        mv.visitLabel(wideSub);
        mv.visitVarInsn(ASTORE, 300);
        mv.visitVarInsn(RET, 300);
        Label farSub = new Label();
        mv.visitLabel(far);
        mv.visitJumpInsn(JSR, farSub);
        mv.visitJumpInsn(GOTO, done);
        for (int i = 0; i < 33000; i++) mv.visitInsn(NOP);
        mv.visitLabel(farSub);
        mv.visitVarInsn(ASTORE, 1);
        mv.visitVarInsn(RET, 1);
        mv.visitLabel(done);
        ret(mv);
    }

    public static void main(String[] args) throws Exception {
        cw = new ClassWriter(ClassWriter.COMPUTE_MAXS);
        cw.visit(V1_6, ACC_PUBLIC | ACC_SUPER, C, null, "java/lang/Object", null);
        constants();
        dynamicConstants();
        locals();
        parameters();
        arithmetic();
        stack();
        arrays();
        fields();
        calls();
        objects();
        branches();
        returns();
        farJumps();
        cw.visitEnd();
        Path out = Paths.get(args[0], C + ".class");
        Files.write(out, cw.toByteArray());
    }
}
