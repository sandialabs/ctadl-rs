// ConstructorArgumentOrder.java
public final class ConstructorArgumentOrder {
    final String first;
    final String second;

    ConstructorArgumentOrder(String first, String second) {
        this.first = first;
        this.second = second;
    }

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        ConstructorArgumentOrder p = new ConstructorArgumentOrder(source(), "clean");
        sink(p.second);
    }
}
