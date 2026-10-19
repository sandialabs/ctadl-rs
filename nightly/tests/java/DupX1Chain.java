// DupX1Chain.java
public final class DupX1Chain {
    String data;

    static DupX1Chain source() {
        return new DupX1Chain();
    }

    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        DupX1Chain o = source();
        String y = "clean";
        String x = o.data = y;
        sink(x);
    }
}
