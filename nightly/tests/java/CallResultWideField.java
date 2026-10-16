// CallResultWideField.java
public final class CallResultWideField {
    long v;
    static CallResultWideField holder = new CallResultWideField();

    static CallResultWideField get() {
        return holder;
    }

    static long source() {
        return 42L;
    }

    static void sink(long s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        long t = source();
        get().v = t;
        CallResultWideField h = holder;
        sink(h.v);
    }
}
