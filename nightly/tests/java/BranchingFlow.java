// BranchingFlow.java
public final class BranchingFlow {
    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        String data = source();
        String result;
        if (args.length > 0) {
            result = data + "_suffix";
        } else {
            result = "safe";
        }
        sink(result); // result is tainted if branch is taken
    }
}
