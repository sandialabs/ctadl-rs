// TryFinallyReturn.java
public final class TryFinallyReturn {
    static int count;

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    static String guarded(String s) {
        try {
            return s;
        } finally {
            count++;
        }
    }

    public static void main(String[] args) {
        sink(guarded(source()));
    }
}
