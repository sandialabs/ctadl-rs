// CallResultArrayStore.java
public final class CallResultArrayStore {
    static String[] holder = new String[1];

    static String[] get() {
        return holder;
    }

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        String t = source();
        get()[0] = t;
        String[] h = holder;
        sink(h[0]);
    }
}
