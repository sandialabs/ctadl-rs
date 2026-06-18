// ArrayFlow.java
public final class ArrayFlow {
    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        String[] arr = new String[3];
        arr[1] = source();
        String local = arr[1];
        sink(local);
    }
}
