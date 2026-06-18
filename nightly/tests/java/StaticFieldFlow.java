// StaticFieldFlow.java
public final class StaticFieldFlow {
    static String data;

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        data = source();
        String local = data;
        sink(local);
    }
}
