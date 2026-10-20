// ArraysOfOneSize.java
public final class ArraysOfOneSize {
    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        String[] tainted = new String[2];
        String[] clean = new String[2];
        tainted[0] = source();
        sink(clean[0]);
    }
}
