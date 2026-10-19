// Dup2X1Chain.java
public final class Dup2X1Chain {
    long v;

    static Dup2X1Chain source() {
        return new Dup2X1Chain();
    }

    static void sink(long s) {
        System.out.println(s);
    }

    public static void main(String[] args) {
        Dup2X1Chain o = source();
        long y = System.nanoTime();
        long x = o.v = y;
        sink(x);
    }
}
