// Prints every instruction of every method in one .dex file, decoded by
// dexlib2, in the canonical form `xtask/src/dexlib2.rs` prints for dex-reader.
// xtask runs it as `java -cp <baksmali-fat.jar> DumpDexlib2Instructions.java <dex>`.
//
// Output, one line per method and one per instruction:
//
//   method <method_idx>
//   <offset> <mnemonic> <operands>
//
// Offsets are in 16-bit code units and are the running sum of dexlib2's
// instruction sizes, so they also check each instruction's size. Operands are
// `r=` (registers in operand order; a range is listed in full), `lit=` (the
// value loaded or used, after any shift), `off=` (branch offset), `idx=` and
// `idx2=` (constant-pool indices), and payload contents.
//
// Index operands are read from the code units. Every format keeps them in a
// fixed place, and dexlib2 exposes no index for some reference kinds.

import com.android.tools.smali.dexlib2.Format;
import com.android.tools.smali.dexlib2.Opcodes;
import com.android.tools.smali.dexlib2.dexbacked.DexBackedClassDef;
import com.android.tools.smali.dexlib2.dexbacked.DexBackedDexFile;
import com.android.tools.smali.dexlib2.dexbacked.DexBackedMethod;
import com.android.tools.smali.dexlib2.dexbacked.DexBackedMethodImplementation;
import com.android.tools.smali.dexlib2.dexbacked.DexBuffer;
import com.android.tools.smali.dexlib2.dexbacked.instruction.DexBackedInstruction;
import com.android.tools.smali.dexlib2.iface.instruction.FiveRegisterInstruction;
import com.android.tools.smali.dexlib2.iface.instruction.Instruction;
import com.android.tools.smali.dexlib2.iface.instruction.NarrowLiteralInstruction;
import com.android.tools.smali.dexlib2.iface.instruction.OffsetInstruction;
import com.android.tools.smali.dexlib2.iface.instruction.OneRegisterInstruction;
import com.android.tools.smali.dexlib2.iface.instruction.RegisterRangeInstruction;
import com.android.tools.smali.dexlib2.iface.instruction.SwitchElement;
import com.android.tools.smali.dexlib2.iface.instruction.SwitchPayload;
import com.android.tools.smali.dexlib2.iface.instruction.ThreeRegisterInstruction;
import com.android.tools.smali.dexlib2.iface.instruction.TwoRegisterInstruction;
import com.android.tools.smali.dexlib2.iface.instruction.WideLiteralInstruction;
import com.android.tools.smali.dexlib2.iface.instruction.formats.ArrayPayload;
import java.io.BufferedOutputStream;
import java.io.PrintStream;
import java.nio.file.Files;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.List;

public class DumpDexlib2Instructions {
    public static void main(String[] args) throws Exception {
        byte[] bytes = Files.readAllBytes(Paths.get(args[0]));
        DexBackedDexFile dex = new DexBackedDexFile(Opcodes.forApi(28), bytes);
        DexBuffer buf = dex.getBuffer();
        PrintStream out = new PrintStream(new BufferedOutputStream(System.out), false, "UTF-8");
        for (DexBackedClassDef cls : dex.getClasses()) {
            for (DexBackedMethod method : cls.getMethods()) {
                DexBackedMethodImplementation impl = method.getImplementation();
                if (impl == null) {
                    continue;
                }
                out.println("method " + method.methodIndex);
                int offset = 0;
                for (Instruction insn : impl.getInstructions()) {
                    out.println(offset + " " + describe(insn, buf));
                    offset += insn.getCodeUnits();
                }
            }
        }
        out.flush();
    }

    static String describe(Instruction insn, DexBuffer buf) {
        Format format = insn.getOpcode().format;
        List<String> parts = new ArrayList<>();
        parts.add(insn.getOpcode().name);
        if (insn instanceof ArrayPayload) {
            ArrayPayload p = (ArrayPayload) insn;
            parts.add("width=" + p.getElementWidth());
            parts.add("values=" + join(p.getArrayElements()));
            return String.join(" ", parts);
        }
        if (insn instanceof SwitchPayload) {
            List<Object> keys = new ArrayList<>();
            List<Object> targets = new ArrayList<>();
            for (SwitchElement e : ((SwitchPayload) insn).getSwitchElements()) {
                keys.add(e.getKey());
                targets.add(e.getOffset());
            }
            parts.add("keys=" + join(keys));
            parts.add("targets=" + join(targets));
            return String.join(" ", parts);
        }

        List<Object> regs = new ArrayList<>();
        if (insn instanceof FiveRegisterInstruction) {
            FiveRegisterInstruction f = (FiveRegisterInstruction) insn;
            int[] all = {f.getRegisterC(), f.getRegisterD(), f.getRegisterE(), f.getRegisterF(),
                f.getRegisterG()};
            for (int i = 0; i < f.getRegisterCount(); i++) {
                regs.add(all[i]);
            }
        } else if (insn instanceof RegisterRangeInstruction) {
            RegisterRangeInstruction r = (RegisterRangeInstruction) insn;
            for (int i = 0; i < r.getRegisterCount(); i++) {
                regs.add(r.getStartRegister() + i);
            }
        } else {
            if (insn instanceof OneRegisterInstruction) {
                regs.add(((OneRegisterInstruction) insn).getRegisterA());
            }
            if (insn instanceof TwoRegisterInstruction) {
                regs.add(((TwoRegisterInstruction) insn).getRegisterB());
            }
            if (insn instanceof ThreeRegisterInstruction) {
                regs.add(((ThreeRegisterInstruction) insn).getRegisterC());
            }
        }
        if (!regs.isEmpty()) {
            parts.add("r=" + join(regs));
        }
        if (insn instanceof WideLiteralInstruction) {
            parts.add("lit=" + ((WideLiteralInstruction) insn).getWideLiteral());
        } else if (insn instanceof NarrowLiteralInstruction) {
            parts.add("lit=" + ((NarrowLiteralInstruction) insn).getNarrowLiteral());
        }
        if (insn instanceof OffsetInstruction) {
            parts.add("off=" + ((OffsetInstruction) insn).getCodeOffset());
        }
        int start = ((DexBackedInstruction) insn).instructionStart;
        switch (format) {
            case Format21c:
            case Format22c:
            case Format35c:
            case Format3rc:
                parts.add("idx=" + buf.readUshort(start + 2));
                break;
            case Format31c:
                parts.add("idx=" + buf.readSmallUint(start + 2));
                break;
            case Format45cc:
            case Format4rcc:
                parts.add("idx=" + buf.readUshort(start + 2));
                parts.add("idx2=" + buf.readUshort(start + 6));
                break;
            default:
                break;
        }
        return String.join(" ", parts);
    }

    static String join(List<?> items) {
        List<String> s = new ArrayList<>();
        for (Object o : items) {
            s.add(o.toString());
        }
        return String.join(",", s);
    }
}
