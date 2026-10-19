// ConstructorArgumentOrderFlow.java
public final class ConstructorArgumentOrderFlow {
    final String first;
    final String second;

    ConstructorArgumentOrderFlow(String first, String second) {
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
        ConstructorArgumentOrderFlow p = new ConstructorArgumentOrderFlow(source(), "clean");
        sink(p.first);
    }
}
