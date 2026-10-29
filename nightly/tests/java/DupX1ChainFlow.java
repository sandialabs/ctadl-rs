// DupX1ChainFlow.java
public final class DupX1ChainFlow {
    String data;

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        DupX1ChainFlow o = new DupX1ChainFlow();
        String x = o.data = source();
        sink(x);
    }
}
