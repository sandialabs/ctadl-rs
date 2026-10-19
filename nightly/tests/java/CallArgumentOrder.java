// CallArgumentOrder.java
public final class CallArgumentOrder {
    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    static String second(String a, String b) {
        return b;
    }

    public static void main(String[] args) {
        sink(second(source(), "clean"));
    }
}
