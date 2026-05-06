/**
 * Minimal sample for instruction-flow tests: one method with simple dataflow,
 * another with a couple of call kinds (instance + static/println).
 */
public class HelloWorld {

    /** Simple dataflow: locals, constants, arithmetic. */
    public static void dataflow() {
        int a = 1;
        int b = 2;
        int c = a + b;
        int d = c * 3;
    }

    /** A couple of call types: instance method (length) and static/instance (println). */
    public static void calls() {
        String s = "hello";
        int n = s.length();
        System.out.println(n);
    }

    public static void main(String[] args) {
        dataflow();
        calls();
    }
}
