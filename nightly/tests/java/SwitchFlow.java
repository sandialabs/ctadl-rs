// SwitchFlow.java
//
// Taint carried across an integer switch whose default arm is a back edge to
// the enclosing loop header -- the shape that first exposed the unmodelled
// switch selector. `lookupswitch` and `tableswitch` each pop one int; if they
// are given a zero stack effect the selector survives as a phantom slot and
// the loop header sees height 1 where the StackMapTable says 0. The method
// then fails to decode, so the flow inside it disappears entirely.
public final class SwitchFlow {

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    /** Sparse case values, so javac emits `lookupswitch`. */
    static void sparse(int n) {
        int cursor = 0;
        while (cursor < n) {
            switch (cursor * 7919) {
                case 0:
                    sink(source());
                    return;
                case 4097:
                    cursor += 2;
                    break;
                case 131073:
                    cursor += 3;
                    break;
                case 16777216:
                    cursor += 4;
                    break;
                default:
                    break;
            }
            cursor++;
        }
    }

    /** Dense case values in the same shape, so javac emits `tableswitch`. */
    static void dense(int n) {
        int cursor = 0;
        while (cursor < n) {
            switch (cursor % 4) {
                case 0:
                    sink(source());
                    return;
                case 1:
                    cursor += 2;
                    break;
                case 2:
                    cursor += 3;
                    break;
                case 3:
                    cursor += 4;
                    break;
                default:
                    break;
            }
            cursor++;
        }
    }

    public static void main(String[] args) {
        sparse(8);
        dense(8);
    }
}
