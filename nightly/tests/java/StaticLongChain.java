// StaticLongChain.java
public final class StaticLongChain {
    static long a;
    static long b;

    static long source() {
        return 7L;
    }

    static void sink(long v) {
        System.out.println(v);
    }

    public static void main(String[] args) {
        a = b = source();
        sink(a);
        sink(b);
    }
}
