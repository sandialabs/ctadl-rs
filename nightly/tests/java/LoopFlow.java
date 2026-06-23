// LoopFlow.java
public final class LoopFlow {
    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        String data = source();
        String result = "";
        for (int i = 0; i < 5; i++) {
            result += data;
        }
        sink(result);
    }
}
