// DupX2ArrayChainFlow.java
public final class DupX2ArrayChainFlow {
    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        String[] arr = new String[2];
        String x = arr[0] = source();
        sink(x);
    }
}
