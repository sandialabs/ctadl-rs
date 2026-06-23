// FieldFlow.java
public final class FieldFlow {
    String data;

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        FieldFlow obj = new FieldFlow();
        obj.data = source();
        String local = obj.data;
        sink(local);
    }
}
