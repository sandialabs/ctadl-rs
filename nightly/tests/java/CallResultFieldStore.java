// CallResultFieldStore.java
public final class CallResultFieldStore {
    String data;
    static CallResultFieldStore holder = new CallResultFieldStore();

    static CallResultFieldStore get() {
        return holder;
    }

    static String source() {
        return "tainted";
    }

    static void sink(String s) {
        System.out.println(s);
    }

    static void store() {
        String t = source();
        get().data = t;
    }

    public static void main(String[] args) {
        store();
        CallResultFieldStore h = holder;
        sink(h.data);
    }
}
