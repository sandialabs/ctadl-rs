// DupX2ArrayChain.java
public final class DupX2ArrayChain {
    static int source() {
        return 1;
    }

    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        String[] arr = new String[2];
        int i = source();
        String x = arr[i] = "clean";
        sink(x);
    }
}
