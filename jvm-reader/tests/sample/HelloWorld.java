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

    public static String intermediate(String in) {
        return "Hello, " + in;
    } 

    /** A couple of call types: instance method (length) and static/instance (println). */
    public static void calls() {
        String s = "hello";
        int n = s.length();
        String userName = System.getProperty("user.name");
        System.out.println(n);
        System.out.println(userName);
    }

    public static void main(String[] args) {
        dataflow();
        calls();
        int x = 7;
        int y = x + 5;
        int z = y * 2;
        System.out.println(intermediate("world " + z));
    }
}
